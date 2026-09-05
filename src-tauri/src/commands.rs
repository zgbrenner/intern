use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use intern_core::{
    HISTORY_LIMIT, HistoryEntry, OperationDirection, OperationKind, OperationStage, QueueStatus,
    QueueStore,
};
use intern_engine::{
    DocumentAnalysis, DocumentSource, Engine, LlamaServer, ModelClient, ModelManifest,
    ServerOptions, SupervisedWorker, prepare_worker_temp_root,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use intern_engine::download::{
    CancellationToken, Downloader, ReqwestHttpTransport, SetupProgress, SystemDiskSpace,
    validate_selected_file,
};
use intern_engine::setup::{
    ExistingModelSelection, SetupOperationGate, install_existing_model_files, semantic_probes,
    validate_semantic_probe,
};
use intern_intake::{IntakeConfig, IntakeWatcher, MachineIdentity};
use intern_queue::{
    AnalyzerBoundary, AppSettings, FilingSink, FilingSinks, ModelFailure, Pipeline, PipelineError,
    PipelineEventSink, PipelineItem, PipelineProgress, SettingsStore,
    paths::{
        SUPPORTED_EXTENSIONS, canonical_file, canonical_folder, canonical_model_file,
        collect_supported_files, display_path, parse_item_id,
    },
};

use crate::intake::{
    CloudLocationDto, CloudRootDto, DescriptionsStatusDto, IntakeStatusDto, LedgerSink,
    PipelineIntakeHost, SharedFiledIndex, classify_folder, list_cloud_roots, now_unix, status_dto,
};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileSelectionDto {
    pub path: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FolderSelectionDto {
    pub path: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExistingModelFilesDto {
    pub model_path: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: String,
    pub message: String,
}

impl From<PipelineError> for CommandError {
    fn from(error: PipelineError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

impl From<intern_engine::EngineError> for CommandError {
    fn from(error: intern_engine::EngineError) -> Self {
        Self {
            code: error.code().as_str().into(),
            message: "local model operation failed".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItemDto {
    id: String,
    original_filename: String,
    status: QueueStatus,
    proposed_filename: Option<String>,
    confidence: Option<f32>,
    description: Option<String>,
    evidence: Option<EvidenceDto>,
    reason: Option<String>,
    error_code: Option<String>,
    undoable: bool,
    proposal_revision: Option<String>,
    reconciliation: Option<ReconciliationDto>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceDto {
    date: Option<String>,
    r#type: Option<String>,
    parties: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReconciliationDto {
    source_path: String,
    destination_path: String,
    error_code: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum SetupStatus {
    Ready,
    Required,
    Downloading,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupStateDto {
    state: SetupStatus,
    downloaded_bytes: u64,
    total_bytes: u64,
    error: Option<String>,
}

struct TauriPipelineEvents {
    app: AppHandle,
}

impl PipelineEventSink for TauriPipelineEvents {
    fn queue_changed(&self) {
        let _ = self.app.emit("queue://changed", serde_json::json!({}));
    }
    fn progress(&self, progress: PipelineProgress) {
        let _ = self.app.emit("queue://progress", progress);
    }
}

struct RuntimeModel {
    executable: PathBuf,
    model_directory: PathBuf,
    engine: RwLock<Option<Engine>>,
    server: Mutex<Option<LlamaServer>>,
}

impl RuntimeModel {
    fn new(executable: PathBuf, model_directory: PathBuf) -> Self {
        Self {
            executable,
            model_directory,
            engine: RwLock::new(None),
            server: Mutex::new(None),
        }
    }

    fn installed(&self, manifest: &ModelManifest) -> bool {
        manifest.files.iter().all(|file| {
            validate_selected_file(&self.model_directory.join(&file.name), file).is_ok()
        })
    }

    /// Starts the local server.
    ///
    /// Text only, and not as a mode: no vision projector is pinned, downloaded,
    /// or loaded. Essentially every business document carries usable text, and a
    /// projector for this model is 668,227,264 bytes - 637 MiB - which every
    /// user would download and hold resident for a path almost nothing takes.
    fn start(&self, manifest: &ModelManifest) -> Result<(), CommandError> {
        if !self.installed(manifest) {
            return Err(CommandError {
                code: "MODEL_NOT_READY".into(),
                message: "model files are not installed".into(),
            });
        }
        let model = manifest.model().ok_or_else(|| CommandError {
            code: "MODEL_MANIFEST_INVALID".into(),
            message: "model manifest names no text model".into(),
        })?;
        let server = LlamaServer::start(
            &self.executable,
            &self.model_directory.join(&model.name),
            None,
            &ServerOptions::default(),
        )?;
        let client = ModelClient::new(
            &server.completion_endpoint(),
            server.api_key().to_owned(),
            manifest.served_model_name.clone(),
        )?;
        let mut server_state = self.server.lock().map_err(|_| CommandError {
            code: "MODEL_NOT_READY".into(),
            message: "model process state is unavailable".into(),
        })?;
        let mut engine_state = self.engine.write().map_err(|_| CommandError {
            code: "MODEL_NOT_READY".into(),
            message: "model state is unavailable".into(),
        })?;
        if server_state.is_some() || engine_state.is_some() {
            return Err(CommandError {
                code: "MODEL_ALREADY_RUNNING".into(),
                message: "local model process is already running".into(),
            });
        }
        *server_state = Some(server);
        *engine_state = Some(Engine::new(client));
        Ok(())
    }

    fn start_verified(
        &self,
        manifest: &ModelManifest,
        cancellation: &CancellationToken,
    ) -> Result<(), CommandError> {
        self.stop_runtime().map_err(|error| CommandError {
            code: error.code,
            message: "existing local model process could not be stopped".into(),
        })?;
        if cancellation.is_canceled() {
            return Err(setup_canceled_error());
        }
        self.start(manifest)?;
        let result = self.semantic_self_test(cancellation);
        if result.is_err() {
            let _ = self.stop_runtime();
        }
        result
    }

    fn semantic_self_test(&self, cancellation: &CancellationToken) -> Result<(), CommandError> {
        for probe in semantic_probes()? {
            if cancellation.is_canceled() {
                return Err(setup_canceled_error());
            }
            let analysis = {
                let engine = self.engine.read().map_err(|_| CommandError {
                    code: "MODEL_SELF_TEST_FAILED".into(),
                    message: "local model state is unavailable during self-test".into(),
                })?;
                let engine = engine.as_ref().ok_or_else(|| CommandError {
                    code: "MODEL_SELF_TEST_FAILED".into(),
                    message: "local model is unavailable during self-test".into(),
                })?;
                engine.analyze(&probe.document, "pdf", &[])
            };
            if cancellation.is_canceled() {
                return Err(setup_canceled_error());
            }
            let analysis = analysis.map_err(|_| CommandError {
                code: "MODEL_SELF_TEST_FAILED".into(),
                message: "local model semantic self-test request failed".into(),
            })?;
            validate_semantic_probe(&probe, &analysis)?;
        }
        Ok(())
    }

    fn stop_runtime(&self) -> Result<(), ModelFailure> {
        let stop_result = {
            let mut server = self
                .server
                .lock()
                .map_err(|_| ModelFailure::fatal("MODEL_CANCEL_FAILED"))?;
            server
                .take()
                .map(|server| {
                    server
                        .stop()
                        .map_err(|_| ModelFailure::fatal("MODEL_CANCEL_FAILED"))
                })
                .unwrap_or(Ok(()))
        };
        *self
            .engine
            .write()
            .map_err(|_| ModelFailure::fatal("MODEL_CANCEL_FAILED"))? = None;
        stop_result
    }
}

impl AnalyzerBoundary for RuntimeModel {
    fn analyze(
        &self,
        source: &DocumentSource,
        extension: &str,
        existing_names: &[&str],
    ) -> Result<DocumentAnalysis, ModelFailure> {
        let engine = self
            .engine
            .read()
            .map_err(|_| ModelFailure::fatal("MODEL_NOT_READY"))?;
        let engine = engine
            .as_ref()
            .ok_or_else(|| ModelFailure::fatal("MODEL_NOT_READY"))?;
        engine
            .analyze(source, extension, existing_names)
            .map_err(|error| ModelFailure::retryable(error.code().as_str()))
    }

    fn recover(&self, failure: &ModelFailure) -> Result<(), ModelFailure> {
        if failure.code != "MODEL_REQUEST_FAILED" {
            return Ok(());
        }
        self.stop_runtime()
            .map_err(|_| ModelFailure::fatal("MODEL_RECOVERY_FAILED"))?;
        let manifest =
            ModelManifest::embedded().map_err(|_| ModelFailure::fatal("MODEL_RECOVERY_FAILED"))?;
        self.start(&manifest)
            .map_err(|_| ModelFailure::fatal("MODEL_RECOVERY_FAILED"))
    }

    fn cancel(&self) -> Result<(), ModelFailure> {
        self.stop_runtime()?;
        let manifest = ModelManifest::embedded()
            .map_err(|error| ModelFailure::fatal(error.code().as_str()))?;
        self.start(&manifest)
            .map_err(|error| ModelFailure::fatal(error.code))
    }

    fn shutdown(&self) -> Result<(), ModelFailure> {
        self.stop_runtime()
    }
}

struct SetupManager {
    app: AppHandle,
    runtime: Arc<RuntimeModel>,
    state: Mutex<SetupStateDto>,
    operation: SetupOperationGate,
    scheduler: Mutex<Option<std::sync::mpsc::Sender<SchedulerMessage>>>,
    model_ready: Arc<AtomicBool>,
}

impl SetupManager {
    fn new(app: AppHandle, runtime: Arc<RuntimeModel>, manifest: &ModelManifest) -> Self {
        let total_bytes = manifest.total_bytes();
        let installed = runtime.installed(manifest);
        let state = SetupStateDto {
            state: if installed {
                SetupStatus::Ready
            } else {
                SetupStatus::Required
            },
            downloaded_bytes: if installed { total_bytes } else { 0 },
            total_bytes,
            error: None,
        };
        Self {
            app,
            runtime,
            state: Mutex::new(state),
            operation: SetupOperationGate::default(),
            scheduler: Mutex::new(None),
            model_ready: Arc::new(AtomicBool::new(installed)),
        }
    }

    fn get(&self) -> Result<SetupStateDto, CommandError> {
        self.state
            .lock()
            .map(|state| state.clone())
            .map_err(|_| CommandError {
                code: "SETUP_UNAVAILABLE".into(),
                message: "setup state is unavailable".into(),
            })
    }

    fn start(self: &Arc<Self>) -> Result<(), CommandError> {
        if self.model_ready.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.start_operation(SetupSource::Download)
    }

    fn choose_existing(
        self: &Arc<Self>,
        selection: ExistingModelSelection,
    ) -> Result<(), CommandError> {
        if self.model_ready.load(Ordering::SeqCst) {
            return Err(CommandError {
                code: "SETUP_ALREADY_READY".into(),
                message: "local model setup is already complete".into(),
            });
        }
        self.start_operation(SetupSource::Existing(selection))
    }

    fn start_operation(self: &Arc<Self>, source: SetupSource) -> Result<(), CommandError> {
        let completed = self.get()?.downloaded_bytes;
        let cancellation = self.operation.begin()?;
        self.set_state(SetupStatus::Downloading, completed, None);
        let manager = Arc::clone(self);
        std::thread::Builder::new()
            .name("intern-model-setup".into())
            .spawn(move || {
                let result = manager.install_and_start(source, &cancellation);
                let final_state = match result {
                    Ok(total) => (SetupStatus::Ready, total, None),
                    Err(error) if error.code == "MODEL_DOWNLOAD_CANCELED" => {
                        let completed = manager
                            .get()
                            .map(|state| state.downloaded_bytes)
                            .unwrap_or(0);
                        (SetupStatus::Required, completed, Some(error.code))
                    }
                    Err(error) => {
                        let completed = manager
                            .get()
                            .map(|state| state.downloaded_bytes)
                            .unwrap_or(0);
                        (SetupStatus::Failed, completed, Some(error.code))
                    }
                };
                manager.set_state(final_state.0, final_state.1, final_state.2);
                manager.operation.finish();
            })
            .map_err(|_| {
                self.set_state(
                    SetupStatus::Failed,
                    completed,
                    Some("SETUP_UNAVAILABLE".into()),
                );
                self.operation.finish();
                CommandError {
                    code: "SETUP_UNAVAILABLE".into(),
                    message: "setup thread could not start".into(),
                }
            })?;
        Ok(())
    }

    fn cancel(&self) -> Result<(), CommandError> {
        if !self.operation.cancel() {
            return Ok(());
        }
        self.runtime.stop_runtime().map_err(|error| CommandError {
            code: error.code,
            message: "local model setup could not be canceled cleanly".into(),
        })
    }

    fn install_and_start(
        &self,
        source: SetupSource,
        cancellation: &CancellationToken,
    ) -> Result<u64, CommandError> {
        let manifest = ModelManifest::embedded()?;
        let total = manifest.total_bytes();
        match source {
            SetupSource::Download => {
                let downloader = Downloader::new(ReqwestHttpTransport::new()?, SystemDiskSpace);
                let mut completed_before = 0;
                for file in &manifest.files {
                    let offset = completed_before;
                    downloader.download(
                        file,
                        &self.runtime.model_directory,
                        cancellation,
                        |progress: SetupProgress| {
                            self.set_state(
                                SetupStatus::Downloading,
                                offset + progress.completed_bytes,
                                None,
                            );
                        },
                    )?;
                    completed_before += file.size;
                }
            }
            SetupSource::Existing(selection) => {
                install_existing_model_files(
                    &manifest,
                    &selection,
                    &self.runtime.model_directory,
                    &SystemDiskSpace,
                    cancellation,
                    |progress| {
                        self.set_state(SetupStatus::Downloading, progress.completed_bytes, None);
                    },
                )?;
            }
        }
        if cancellation.is_canceled() {
            return Err(setup_canceled_error());
        }
        self.runtime.start_verified(&manifest, cancellation)?;
        Ok(total)
    }

    fn set_state(&self, state: SetupStatus, downloaded_bytes: u64, error: Option<String>) {
        let ready = matches!(state, SetupStatus::Ready);
        self.model_ready.store(ready, Ordering::SeqCst);
        if let Ok(mut current) = self.state.lock() {
            current.state = state;
            current.downloaded_bytes = downloaded_bytes.min(current.total_bytes);
            current.error = error;
            let _ = self.app.emit("setup://progress", current.clone());
        }
        if ready
            && let Ok(scheduler) = self.scheduler.lock()
            && let Some(sender) = scheduler.as_ref()
        {
            let _ = sender.send(SchedulerMessage::Wake);
        }
    }
}

enum SetupSource {
    Download,
    Existing(ExistingModelSelection),
}

fn setup_canceled_error() -> CommandError {
    CommandError {
        code: "MODEL_DOWNLOAD_CANCELED".into(),
        message: "model setup was canceled".into(),
    }
}

pub(crate) enum SchedulerMessage {
    Wake,
    Shutdown,
}

fn scheduler_actions(timed_out: bool, model_ready: bool) -> (bool, bool) {
    (timed_out, model_ready)
}

struct PipelineScheduler {
    sender: std::sync::mpsc::Sender<SchedulerMessage>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
    pipeline: Arc<Pipeline>,
}

impl PipelineScheduler {
    fn start(
        pipeline: Arc<Pipeline>,
        model_ready: Arc<AtomicBool>,
        app: AppHandle,
    ) -> Result<Self, CommandError> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let scheduled_pipeline = Arc::clone(&pipeline);
        let join = std::thread::Builder::new()
            .name("intern-pipeline-scheduler".into())
            .spawn(move || {
                loop {
                    let timed_out = match receiver.recv_timeout(Duration::from_secs(65)) {
                        Ok(SchedulerMessage::Shutdown) => return,
                        Ok(SchedulerMessage::Wake) => false,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => true,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    };
                    let (recover, drain) =
                        scheduler_actions(timed_out, model_ready.load(Ordering::SeqCst));
                    if recover && let Err(error) = scheduled_pipeline.recover() {
                        let _ = app.emit(
                            "queue://changed",
                            serde_json::json!({ "error": error.code }),
                        );
                    }
                    if drain && let Err(error) = scheduled_pipeline.run_until_idle() {
                        let _ = app.emit(
                            "queue://changed",
                            serde_json::json!({ "error": error.code }),
                        );
                    }
                }
            })
            .map_err(|_| CommandError {
                code: "STATE_CONFLICT".into(),
                message: "pipeline scheduler could not start".into(),
            })?;
        Ok(Self {
            sender,
            join: Mutex::new(Some(join)),
            pipeline,
        })
    }

    fn wake(&self) -> Result<(), CommandError> {
        self.sender
            .send(SchedulerMessage::Wake)
            .map_err(|_| CommandError {
                code: "STATE_CONFLICT".into(),
                message: "pipeline scheduler is unavailable".into(),
            })
    }
}

impl Drop for PipelineScheduler {
    fn drop(&mut self) {
        let _ = self.pipeline.shutdown();
        let _ = self.sender.send(SchedulerMessage::Shutdown);
        if let Ok(join) = self.join.get_mut()
            && let Some(join) = join.take()
        {
            let _ = join.join();
        }
    }
}

pub struct AppState {
    pipeline: Arc<Pipeline>,
    settings: SettingsStore,
    setup: Arc<SetupManager>,
    scheduler: PipelineScheduler,
    app: AppHandle,
    data_dir: PathBuf,
    identity: Mutex<MachineIdentity>,
    intake: Mutex<Option<IntakeWatcher>>,
    /// Why the watcher is not running even though intake is enabled — a stale
    /// intake folder must surface in `intake_status`, not block launch/save.
    intake_error: Mutex<Option<String>>,
    /// A dedicated read-only connection to the queue database for the history
    /// view. The pipeline owns its store privately; history listing and CSV
    /// export are pure reads, so they take their own SQLite session (WAL
    /// readers never block the pipeline's writes) instead of widening the
    /// pipeline's surface.
    history: QueueStore,
    /// Writes description records beside filed documents when the setting
    /// asks for them; the pipeline reports every completed rename to it.
    ledger: Arc<LedgerSink>,
    /// Leaves a marker in the watched intake folder for every document filed
    /// out of it, and reads the markers teammates left, so the same content
    /// is never filed twice across machines.
    filed_index: Arc<SharedFiledIndex>,
}

impl AppState {
    pub fn initialize(app: &AppHandle) -> Result<Self, CommandError> {
        let data = app.path().app_local_data_dir().map_err(|_| CommandError {
            code: "APP_DATA_UNAVAILABLE".into(),
            message: "local application data directory is unavailable".into(),
        })?;
        std::fs::create_dir_all(&data).map_err(|_| CommandError {
            code: "APP_DATA_UNAVAILABLE".into(),
            message: "local application data directory could not be created".into(),
        })?;
        let executable_directory = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .ok_or_else(|| CommandError {
                code: "SIDECAR_UNAVAILABLE".into(),
                message: "application executable directory is unavailable".into(),
            })?;
        let worker_name = if cfg!(windows) {
            "intern-worker.exe"
        } else {
            "intern-worker"
        };
        let server_name = if cfg!(windows) {
            "llama-server.exe"
        } else {
            "llama-server"
        };
        let model_directory = data.join("models");
        let runtime = Arc::new(RuntimeModel::new(
            executable_directory.join(server_name),
            model_directory,
        ));
        let manifest = ModelManifest::embedded()?;
        let setup = Arc::new(SetupManager::new(
            app.clone(),
            Arc::clone(&runtime),
            &manifest,
        ));
        if runtime.installed(&manifest)
            && let Err(error) = runtime.start_verified(&manifest, &CancellationToken::new())
        {
            let installed_bytes = manifest.total_bytes();
            setup.set_state(SetupStatus::Failed, installed_bytes, Some(error.code));
        }
        let settings = SettingsStore::new(data.join("settings.json"));
        let worker_temp_root = data.join("worker-temp");
        prepare_worker_temp_root(&worker_temp_root, 128).map_err(|_| CommandError {
            code: "APP_DATA_UNAVAILABLE".into(),
            message: "private worker temporary directory is unavailable".into(),
        })?;
        let ledger = Arc::new(LedgerSink::new(settings.clone(), data.clone()));
        ledger.attach(app.clone());
        let filed_index = Arc::new(SharedFiledIndex::new(settings.clone(), data.clone()));
        let pipeline = Arc::new(
            Pipeline::with_local_files(
                data.join("queue.sqlite3"),
                Arc::new(SupervisedWorker::with_temp_root(
                    executable_directory.join(worker_name),
                    worker_temp_root,
                )),
                runtime,
                Arc::new(TauriPipelineEvents { app: app.clone() }),
                settings.clone(),
            )?
            .with_filing_sink(Arc::new(FilingSinks(vec![
                ledger.clone() as Arc<dyn FilingSink>,
                filed_index.clone(),
            ])))
            .with_duplicate_oracle(filed_index.clone()),
        );
        pipeline.recover()?;
        // Opened after the pipeline so the pipeline's own store has already
        // migrated the schema this connection reads.
        let history = QueueStore::open(data.join("queue.sqlite3")).map_err(|_| CommandError {
            code: "APP_DATA_UNAVAILABLE".into(),
            message: "the rename history database is unavailable".into(),
        })?;
        let scheduler = PipelineScheduler::start(
            Arc::clone(&pipeline),
            Arc::clone(&setup.model_ready),
            app.clone(),
        )?;
        *setup.scheduler.lock().map_err(|_| CommandError {
            code: "STATE_CONFLICT".into(),
            message: "setup scheduler state is unavailable".into(),
        })? = Some(scheduler.sender.clone());
        let startup_settings = settings.load().unwrap_or_default();
        let identity = MachineIdentity::load_or_create(&data, &startup_settings.machine_label)
            .map_err(|_| CommandError {
                code: "APP_DATA_UNAVAILABLE".into(),
                message: "machine identity could not be created".into(),
            })?;
        let state = Self {
            pipeline,
            settings,
            setup,
            scheduler,
            app: app.clone(),
            data_dir: data,
            identity: Mutex::new(identity),
            intake: Mutex::new(None),
            intake_error: Mutex::new(None),
            history,
            ledger,
            filed_index,
        };
        if matches!(state.setup.get()?.state, SetupStatus::Ready) {
            state.schedule()?;
        }
        if startup_settings.intake_enabled
            && let Err(error) = state.restart_intake(&startup_settings)
            && let Ok(mut slot) = state.intake_error.lock()
        {
            *slot = Some(format!("{}: {}", error.code, error.message));
        }
        Ok(state)
    }

    fn schedule(&self) -> Result<(), CommandError> {
        if self.setup.model_ready.load(Ordering::SeqCst) {
            self.scheduler.wake()?;
        }
        Ok(())
    }

    /// Stops any running watcher and starts a fresh one when the settings
    /// call for it. The identity is reloaded so a changed machine label takes
    /// effect. An intake folder that no longer canonicalizes is recorded for
    /// `intake_status` instead of returned as an error.
    fn restart_intake(&self, settings: &AppSettings) -> Result<(), CommandError> {
        let identity = MachineIdentity::load_or_create(&self.data_dir, &settings.machine_label)
            .map_err(|_| CommandError {
                code: "APP_DATA_UNAVAILABLE".into(),
                message: "machine identity could not be loaded".into(),
            })?;
        *self.identity.lock().map_err(|_| intake_state_conflict())? = identity.clone();
        let previous = self
            .intake
            .lock()
            .map_err(|_| intake_state_conflict())?
            .take();
        // Dropping the watcher joins its scan thread; never do that while
        // holding the slot's lock.
        drop(previous);
        let mut watcher = None;
        let mut error = None;
        if settings.intake_enabled {
            match canonical_folder(Path::new(&settings.intake_folder)) {
                Ok(folder) => {
                    let mut config = IntakeConfig::new(
                        folder,
                        SUPPORTED_EXTENSIONS
                            .iter()
                            .map(|extension| (*extension).to_owned())
                            .collect(),
                    );
                    config.process_others_uploads = settings.process_others_uploads;
                    let host = Arc::new(PipelineIntakeHost::new(
                        Arc::clone(&self.pipeline),
                        self.scheduler.sender.clone(),
                        Arc::clone(&self.setup.model_ready),
                        self.app.clone(),
                        identity.clone(),
                        Arc::clone(&self.filed_index),
                    ));
                    watcher = Some(IntakeWatcher::start(config, identity, host));
                }
                Err(folder_error) => {
                    error = Some(format!("INTAKE_FOLDER_MISSING: {}", folder_error.message));
                }
            }
        }
        *self.intake.lock().map_err(|_| intake_state_conflict())? = watcher;
        *self
            .intake_error
            .lock()
            .map_err(|_| intake_state_conflict())? = error;
        Ok(())
    }

    fn intake_status_dto(&self) -> Result<IntakeStatusDto, CommandError> {
        let settings = self.settings.load().unwrap_or_default();
        let identity = self
            .identity
            .lock()
            .map_err(|_| intake_state_conflict())?
            .clone();
        let status = self
            .intake
            .lock()
            .map_err(|_| intake_state_conflict())?
            .as_ref()
            .map(IntakeWatcher::status);
        let error = self
            .intake_error
            .lock()
            .map_err(|_| intake_state_conflict())?
            .clone()
            .or_else(|| self.filed_index.last_error());
        Ok(status_dto(
            settings.intake_enabled,
            &identity,
            &settings.intake_folder,
            status.as_ref(),
            error,
            now_unix(),
        ))
    }

    fn emit_intake_changed(&self) -> Result<(), CommandError> {
        let dto = self.intake_status_dto()?;
        let _ = self.app.emit("intake://changed", dto);
        Ok(())
    }

    /// The settings as currently stored, for startup decisions (tray,
    /// start-hidden). A missing file is the defaults, same as `load`.
    pub(crate) fn settings_snapshot(&self) -> AppSettings {
        self.settings.load().unwrap_or_default()
    }

    /// Whether a main-window close should hide to the tray instead of running
    /// the normal exit path. Read fresh on every close so a settings save
    /// takes effect on the very next close, and erring toward `false`: a
    /// broken settings file must fall back to the ordinary exit, never to a
    /// window that hides with no tray to bring it back.
    pub(crate) fn hide_window_on_close(&self) -> bool {
        self.settings
            .load()
            .map(|settings| crate::tray::close_hides_to_tray(settings.run_in_background))
            .unwrap_or(false)
    }
}

/// The explicit quit path, used by the tray's "Quit Intern" item: shut the
/// pipeline (and with it the local model process) down deliberately, then
/// leave without starting window teardown - the same shape as the close-time
/// exit, which deliberately avoids wedging in WebView destruction.
pub(crate) fn shutdown_and_exit(app: &AppHandle) -> ! {
    if let Some(state) = app.try_state::<AppState>() {
        let _ = state.pipeline.shutdown();
    }
    std::process::exit(0);
}

fn intake_state_conflict() -> CommandError {
    CommandError {
        code: "STATE_CONFLICT".into(),
        message: "intake state is unavailable".into(),
    }
}

#[tauri::command]
pub fn queue_list(state: State<'_, AppState>) -> Result<Vec<QueueItemDto>, CommandError> {
    let items = state.pipeline.list()?;
    // The window asks for the list on every queue change, which makes this
    // the one place that always knows the current counts - so the tray's
    // tooltip is kept here rather than on a second event path.
    let (needs_review, ready) = attention_counts(&items);
    crate::tray::update_tooltip(&state.app, needs_review, ready);
    items.into_iter().map(queue_item_dto).collect()
}

/// How many items wait on a person: those needing review, and those ready
/// to rename but not applied automatically.
fn attention_counts(items: &[PipelineItem]) -> (usize, usize) {
    let count = |status: QueueStatus| items.iter().filter(|item| item.status == status).count();
    (count(QueueStatus::NeedsReview), count(QueueStatus::Ready))
}

#[tauri::command]
pub fn queue_add_files(
    files: Vec<FileSelectionDto>,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    let mut paths = Vec::new();
    for file in files {
        let input = Path::new(&file.path);
        match canonical_file(input) {
            Ok(path) => paths.push(path),
            Err(file_error) => match canonical_folder(input) {
                Ok(folder) => paths.extend(collect_supported_files(&folder)?),
                Err(_) => return Err(file_error.into()),
            },
        }
    }
    state.pipeline.enqueue_files(&paths)?;
    state.schedule()
}

#[tauri::command]
pub fn queue_add_folder(
    folder: FolderSelectionDto,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    let folder = canonical_folder(Path::new(&folder.path))?;
    let paths = collect_supported_files(&folder)?;
    state.pipeline.enqueue_files(&paths)?;
    state.schedule()
}

#[tauri::command]
pub fn queue_pause(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.pipeline.pause();
    let _ = state
        .app
        .emit("queue://changed", serde_json::json!({ "paused": true }));
    Ok(())
}

#[tauri::command]
pub fn queue_resume(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.pipeline.resume();
    let _ = state
        .app
        .emit("queue://changed", serde_json::json!({ "paused": false }));
    state.schedule()
}

#[tauri::command]
pub fn queue_cancel(id: String, state: State<'_, AppState>) -> Result<(), CommandError> {
    state.pipeline.cancel(parse_item_id(&id)?)?;
    Ok(())
}

#[tauri::command]
pub fn queue_retry(id: String, state: State<'_, AppState>) -> Result<(), CommandError> {
    state.pipeline.retry(parse_item_id(&id)?)?;
    state.schedule()
}

#[tauri::command]
pub fn queue_remove(id: String, state: State<'_, AppState>) -> Result<(), CommandError> {
    state.pipeline.remove(parse_item_id(&id)?)?;
    Ok(())
}

#[tauri::command]
pub fn proposal_approve(
    id: String,
    filename: String,
    description: String,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    state
        .pipeline
        .approve(parse_item_id(&id)?, &filename, &description)?;
    Ok(())
}

#[tauri::command]
pub fn proposal_keep_original(id: String, state: State<'_, AppState>) -> Result<(), CommandError> {
    state.pipeline.keep_original(parse_item_id(&id)?)?;
    Ok(())
}

#[tauri::command]
pub fn operation_undo(id: String, state: State<'_, AppState>) -> Result<(), CommandError> {
    state.pipeline.undo(parse_item_id(&id)?)?;
    Ok(())
}

/// The settings as the interface should show them: folders in their readable
/// spelling. Storage keeps the canonical form (on Windows, the verbatim
/// `\\?\` prefix that long and oddly named paths need), and `settings_save`
/// canonicalizes whatever comes back, so the round trip is lossless.
#[tauri::command]
pub fn settings_get(state: State<'_, AppState>) -> Result<AppSettings, CommandError> {
    let mut settings = state.settings.load()?;
    settings.destination = display_folder(&settings.destination);
    settings.intake_folder = display_folder(&settings.intake_folder);
    Ok(settings)
}

fn display_folder(folder: &str) -> String {
    if folder.trim().is_empty() {
        String::new()
    } else {
        display_path(Path::new(folder))
    }
}

#[tauri::command]
pub fn settings_save(
    mut settings: AppSettings,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    let previous = state.settings.load().unwrap_or_default();
    validate_intake_settings(&mut settings, &|path| canonical_folder(path).ok())?;
    // With intake enabled the destination was already canonicalized (with the
    // intake-specific error code); otherwise keep the original behavior.
    if !settings.intake_enabled && !settings.destination.trim().is_empty() {
        settings.destination = canonical_folder(Path::new(&settings.destination))?
            .to_string_lossy()
            .into_owned();
    }
    validate_description_settings(&settings)?;
    // Applied before anything persists so an operating system that refuses
    // the login entry leaves the stored settings unchanged - the dialog shows
    // the error against a state that is still true.
    if previous.start_at_login != settings.start_at_login {
        apply_autostart(&state.app, settings.start_at_login)?;
    }
    state.settings.save(&settings)?;
    if previous.run_in_background != settings.run_in_background {
        crate::tray::sync_tray(&state.app, settings.run_in_background);
        // A tray that was just created starts with the bare tooltip; give it
        // the current counts rather than waiting for the next queue change.
        if settings.run_in_background
            && let Ok(items) = state.pipeline.list()
        {
            let (needs_review, ready) = attention_counts(&items);
            crate::tray::update_tooltip(&state.app, needs_review, ready);
        }
    }
    if previous.intake_folder != settings.intake_folder
        || previous.intake_enabled != settings.intake_enabled
        || previous.process_others_uploads != settings.process_others_uploads
        || previous.machine_label != settings.machine_label
    {
        state.restart_intake(&settings)?;
        state.emit_intake_changed()?;
    }
    Ok(())
}

/// Enables or disables the start-at-login entry to match the setting.
///
/// Registration lives with the operating system and can be refused (a locked
/// registry key, a read-only autostart directory); that refusal becomes an
/// ordinary save error the dialog can show, never a crash.
fn apply_autostart(app: &AppHandle, enabled: bool) -> Result<(), CommandError> {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|_| CommandError {
        code: "AUTOSTART_FAILED".into(),
        message: if enabled {
            "starting Intern at sign-in could not be enabled".into()
        } else {
            "starting Intern at sign-in could not be disabled".into()
        },
    })
}

/// Intake-related validation for `settings_save`, before anything persists.
///
/// A watched intake with an in-place rename would loop (the renamed file
/// reappears as new), so intake enabled requires a real destination outside
/// the intake folder. Canonical forms are written back so later comparisons
/// and the containment check are component-wise, never string-prefix.
/// `canonicalize` is `canonical_folder` in production and a seam for tests.
fn validate_intake_settings(
    settings: &mut AppSettings,
    canonicalize: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Result<(), CommandError> {
    if settings.intake_folder.trim().is_empty() {
        if settings.intake_enabled {
            return Err(CommandError {
                code: "INTAKE_FOLDER_MISSING".into(),
                message: "an intake folder must be chosen".into(),
            });
        }
    } else {
        settings.intake_folder = canonicalize(Path::new(&settings.intake_folder))
            .ok_or_else(|| CommandError {
                code: "INTAKE_FOLDER_MISSING".into(),
                message: "the intake folder does not exist".into(),
            })?
            .to_string_lossy()
            .into_owned();
    }
    if !settings.intake_enabled {
        return Ok(());
    }
    if settings.destination.trim().is_empty() {
        return Err(CommandError {
            code: "INTAKE_NEEDS_DESTINATION".into(),
            message: "watching an intake folder requires a destination folder".into(),
        });
    }
    let destination =
        canonicalize(Path::new(&settings.destination)).ok_or_else(|| CommandError {
            code: "INTAKE_NEEDS_DESTINATION".into(),
            message: "the destination folder does not exist".into(),
        })?;
    if destination.starts_with(Path::new(&settings.intake_folder)) {
        return Err(CommandError {
            code: "DESTINATION_INSIDE_INTAKE".into(),
            message: "the destination cannot be the intake folder or live inside it".into(),
        });
    }
    settings.destination = destination.to_string_lossy().into_owned();
    Ok(())
}

/// Description records live under the destination folder, so asking for them
/// without one is a configuration that could never do anything. Refused at
/// save time, like the intake rules, rather than silently ignored.
fn validate_description_settings(settings: &AppSettings) -> Result<(), CommandError> {
    if settings.record_descriptions && settings.destination.trim().is_empty() {
        return Err(CommandError {
            code: "DESCRIPTIONS_NEED_DESTINATION".into(),
            message: "description records need a destination folder to live in".into(),
        });
    }
    Ok(())
}

#[tauri::command]
pub fn intake_status(state: State<'_, AppState>) -> Result<IntakeStatusDto, CommandError> {
    state.intake_status_dto()
}

/// The OneDrive accounts and SharePoint libraries the sync client keeps on
/// this machine. A local lookup of the sync client's own configuration; no
/// network request is made.
#[tauri::command]
pub fn cloud_roots() -> Result<Vec<CloudRootDto>, CommandError> {
    Ok(list_cloud_roots())
}

#[tauri::command]
pub fn descriptions_status(
    state: State<'_, AppState>,
) -> Result<DescriptionsStatusDto, CommandError> {
    Ok(state.ledger.status())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackfillResultDto {
    pub written: u32,
    pub failed: u32,
}

/// Writes a description record for every document Intern has already filed
/// and not undone, for a records folder switched on after the fact. Each
/// document's record is rewritten from the queue's own copy of its sentence
/// and facts, so running it twice changes nothing.
#[tauri::command]
pub fn descriptions_backfill(
    state: State<'_, AppState>,
) -> Result<BackfillResultDto, CommandError> {
    let settings = state.settings.load()?;
    if !settings.record_descriptions {
        return Err(CommandError {
            code: "DESCRIPTIONS_DISABLED".into(),
            message: "turn on description records and save before writing them".into(),
        });
    }
    let documents = state.pipeline.filed_documents()?;
    let (written, failed) = state.ledger.backfill(&documents);
    Ok(BackfillResultDto { written, failed })
}

#[tauri::command]
pub fn intake_scan_now(state: State<'_, AppState>) -> Result<(), CommandError> {
    let watcher = state.intake.lock().map_err(|_| intake_state_conflict())?;
    if let Some(watcher) = watcher.as_ref() {
        watcher.scan_now();
    }
    Ok(())
}

#[tauri::command]
pub fn folder_classify(path: String) -> Result<Option<CloudLocationDto>, CommandError> {
    Ok(classify_folder(&path))
}

#[tauri::command]
pub fn setup_get(state: State<'_, AppState>) -> Result<SetupStateDto, CommandError> {
    state.setup.get()
}

#[tauri::command]
pub fn setup_start(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.setup.start()
}

#[tauri::command]
pub fn setup_cancel(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.setup.cancel()
}

#[tauri::command]
pub fn setup_choose_existing(
    files: ExistingModelFilesDto,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    let model_path = canonical_model_file(Path::new(&files.model_path))?;
    state
        .setup
        .choose_existing(ExistingModelSelection { model_path })
}

#[tauri::command]
pub fn history_clear(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.pipeline.clear_history()?;
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntryDto {
    receipt_id: String,
    queue_item_id: String,
    /// Unix seconds; the frontend formats it locally, the CSV as ISO-8601 UTC.
    at: i64,
    /// "apply" | "undo" (serde snake_case of the core enum).
    direction: OperationDirection,
    /// "rename" | "verified_copy".
    kind: OperationKind,
    /// "complete" | "rolled_back" — only terminal stages are listed.
    stage: OperationStage,
    original_path: String,
    new_path: String,
    /// The one-sentence description that was applied with the rename, when
    /// the item still has its proposal.
    description: Option<String>,
}

fn history_entry_dto(entry: HistoryEntry, description: Option<String>) -> HistoryEntryDto {
    HistoryEntryDto {
        receipt_id: entry.receipt_id.to_string(),
        queue_item_id: entry.queue_item_id.to_string(),
        at: entry.at,
        direction: entry.direction,
        kind: entry.kind,
        stage: entry.stage,
        original_path: display_path(&entry.original_path),
        new_path: display_path(&entry.new_path),
        description,
    }
}

/// The applied description for every queue item that still has a proposal,
/// so history rows and the CSV export can carry the sentence beside the
/// rename it belongs to.
fn descriptions_by_item(
    state: &AppState,
) -> Result<std::collections::HashMap<i64, String>, CommandError> {
    Ok(state
        .pipeline
        .list()?
        .into_iter()
        .filter_map(|item| {
            item.proposal
                .map(|proposal| (item.id, proposal.description))
        })
        .collect())
}

fn history_read_error(error: intern_core::InternError) -> CommandError {
    CommandError {
        code: error.code().as_str().into(),
        message: "the rename history could not be read".into(),
    }
}

fn history_export_failed(message: impl Into<String>) -> CommandError {
    CommandError {
        code: "HISTORY_EXPORT_FAILED".into(),
        message: message.into(),
    }
}

#[tauri::command]
pub fn history_list(state: State<'_, AppState>) -> Result<Vec<HistoryEntryDto>, CommandError> {
    let descriptions = descriptions_by_item(&state)?;
    Ok(state
        .history
        .list_operation_history(HISTORY_LIMIT)
        .map_err(history_read_error)?
        .into_iter()
        .map(|entry| {
            let description = descriptions.get(&entry.queue_item_id).cloned();
            history_entry_dto(entry, description)
        })
        .collect())
}

/// Writes the rename history to `path` as RFC 4180 CSV and reports how many
/// operations were written.
///
/// The path comes from the native save dialog, so it is expected to be
/// absolute with an existing parent folder; anything else is refused before a
/// byte is written rather than being resolved against whatever the process's
/// working directory happens to be.
#[tauri::command]
pub fn history_export(path: String, state: State<'_, AppState>) -> Result<usize, CommandError> {
    let destination = Path::new(&path);
    if !destination.is_absolute() {
        return Err(history_export_failed("the export path must be absolute"));
    }
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| history_export_failed("the export path has no parent folder"))?;
    if !parent.is_dir() {
        return Err(history_export_failed("the export folder does not exist"));
    }
    let entries = state
        .history
        .list_operation_history(HISTORY_LIMIT)
        .map_err(history_read_error)?;
    let descriptions = descriptions_by_item(&state)?;
    std::fs::write(destination, history_csv(&entries, &descriptions))
        .map_err(|_| history_export_failed("the history CSV could not be written"))?;
    Ok(entries.len())
}

/// Renders history entries as RFC 4180 CSV: CRLF row endings, and any field
/// containing a comma, quote, or line break is quoted with quotes doubled.
/// The description column is last, so a spreadsheet opened from this export
/// can be pasted straight into a SharePoint grid view beside the filenames.
fn history_csv(
    entries: &[HistoryEntry],
    descriptions: &std::collections::HashMap<i64, String>,
) -> String {
    let mut csv = String::from("at,direction,kind,stage,originalPath,newPath,description\r\n");
    for entry in entries {
        let fields = [
            iso8601_utc(entry.at),
            match entry.direction {
                OperationDirection::Apply => "apply".into(),
                OperationDirection::Undo => "undo".into(),
            },
            match entry.kind {
                OperationKind::Rename => "rename".into(),
                OperationKind::VerifiedCopy => "verified_copy".into(),
            },
            match entry.stage {
                OperationStage::Complete => "complete".to_owned(),
                OperationStage::RolledBack => "rolled_back".to_owned(),
                // Unreachable for listed history (terminal stages only), but a
                // receipt must never be silently mislabeled if that changes.
                other => format!("{other:?}").to_lowercase(),
            },
            display_path(&entry.original_path),
            display_path(&entry.new_path),
            descriptions
                .get(&entry.queue_item_id)
                .cloned()
                .unwrap_or_default(),
        ];
        let row = fields
            .iter()
            .map(|field| csv_field(field))
            .collect::<Vec<_>>()
            .join(",");
        csv.push_str(&row);
        csv.push_str("\r\n");
    }
    csv
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// Formats unix seconds as ISO-8601 UTC ("2024-04-12T09:30:00Z") without
/// pulling in a date-time dependency (days-from-civil inverse, Howard
/// Hinnant's algorithm).
fn iso8601_utc(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let seconds = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day as u32)
}

/// Abandons every item still waiting, for a folder chosen by mistake.
///
/// Returns the number dropped so the interface can say what it did rather than
/// leaving the user to count rows.
#[tauri::command]
pub fn queue_discard_waiting(state: State<'_, AppState>) -> Result<usize, CommandError> {
    Ok(state.pipeline.discard_waiting()?)
}

fn queue_item_dto(item: PipelineItem) -> Result<QueueItemDto, CommandError> {
    let proposal = item.proposal.as_ref();
    let evidence = proposal.map(|record| EvidenceDto {
        date: record.analysis.proposal.evidence.date.clone(),
        r#type: record.analysis.proposal.evidence.document_type.clone(),
        parties: (!record.analysis.proposal.parties.is_empty())
            .then(|| record.analysis.proposal.parties.join("; ")),
    });
    let reconciliation = item
        .receipt
        .as_ref()
        .filter(|receipt| {
            item.status == QueueStatus::NeedsReview
                && item.error_code == Some(intern_core::ErrorCode::SourceDeleteFailed)
                && receipt.direction == OperationDirection::Apply
                && receipt.stage == OperationStage::Published
        })
        .map(|receipt| ReconciliationDto {
            source_path: display_path(&receipt.source),
            destination_path: display_path(&receipt.destination),
            error_code: "SOURCE_DELETE_FAILED".into(),
        });
    Ok(QueueItemDto {
        id: item.id.to_string(),
        original_filename: item
            .source_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Document")
            .to_owned(),
        status: item.status,
        proposed_filename: proposal.map(|record| record.filename.clone()),
        confidence: proposal.map(|record| record.analysis.proposal.confidence),
        description: proposal.map(|record| record.description.clone()),
        evidence,
        reason: proposal
            .filter(|record| !record.reasons.is_empty())
            .map(|record| record.reasons.join(", "))
            // A DUPLICATE flag is raised before analysis, so no proposal
            // carries its reason; name the file the content is already
            // filed under.
            .or_else(|| {
                item.duplicate_of
                    .as_deref()
                    .map(|name| format!("Duplicate of {name}"))
            }),
        error_code: item.error_code.map(|code| code.as_str().to_owned()),
        undoable: item.status == QueueStatus::Completed
            && item.receipt.as_ref().is_some_and(|receipt| {
                receipt.direction == OperationDirection::Apply
                    && receipt.stage == OperationStage::Complete
            }),
        proposal_revision: proposal.map(|record| record.revision.to_string()),
        reconciliation,
    })
}

#[cfg(test)]
mod intake_tests {
    use std::path::{Path, PathBuf};

    use intern_core::{
        OperationDirection, OperationKind, OperationReceipt, OperationStage, QueueStatus,
    };
    use intern_intake::{CloudProviderKind, DoneOutcome, ItemState, MachineIdentity};
    use intern_queue::AppSettings;

    use super::{validate_description_settings, validate_intake_settings};
    use crate::intake::{CloudProviderDto, item_fate, presence_active, status_dto};

    /// A fake folder canonicalizer: pairs of (as-entered, canonical form).
    fn canonicalizer(
        known: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&Path) -> Option<PathBuf> {
        move |path| {
            known
                .iter()
                .find(|(entered, _)| Path::new(entered) == path)
                .map(|(_, canonical)| PathBuf::from(canonical))
        }
    }

    fn settings(intake_enabled: bool, intake_folder: &str, destination: &str) -> AppSettings {
        AppSettings {
            destination: destination.into(),
            intake_folder: intake_folder.into(),
            intake_enabled,
            ..AppSettings::default()
        }
    }

    fn error_code(result: Result<(), super::CommandError>) -> String {
        result.expect_err("validation should fail").code
    }

    #[test]
    fn enabling_intake_requires_an_existing_intake_folder() {
        let fs = canonicalizer(&[("/out", "/out")]);
        let mut blank = settings(true, "  ", "/out");
        assert_eq!(
            error_code(validate_intake_settings(&mut blank, &fs)),
            "INTAKE_FOLDER_MISSING"
        );
        let mut missing = settings(true, "/gone", "/out");
        assert_eq!(
            error_code(validate_intake_settings(&mut missing, &fs)),
            "INTAKE_FOLDER_MISSING"
        );
    }

    #[test]
    fn description_records_require_a_destination_to_live_in() {
        let mut wanted = settings(false, "", "");
        wanted.record_descriptions = true;
        assert_eq!(
            error_code(validate_description_settings(&wanted)),
            "DESCRIPTIONS_NEED_DESTINATION"
        );
        wanted.destination = "/out".into();
        assert!(validate_description_settings(&wanted).is_ok());
        let unwanted = settings(false, "", "");
        assert!(validate_description_settings(&unwanted).is_ok());
    }

    #[test]
    fn enabling_intake_requires_a_real_destination() {
        let fs = canonicalizer(&[("/in", "/in")]);
        let mut blank = settings(true, "/in", "");
        assert_eq!(
            error_code(validate_intake_settings(&mut blank, &fs)),
            "INTAKE_NEEDS_DESTINATION"
        );
        let mut missing = settings(true, "/in", "/gone");
        assert_eq!(
            error_code(validate_intake_settings(&mut missing, &fs)),
            "INTAKE_NEEDS_DESTINATION"
        );
    }

    #[test]
    fn destination_containment_is_component_wise_not_string_prefix() {
        let fs = canonicalizer(&[
            ("/a/b", "/a/b"),
            ("/a/b/c", "/a/b/c"),
            ("/a/bc", "/a/bc"),
            ("/a/other", "/a/other"),
        ]);
        let mut equal = settings(true, "/a/b", "/a/b");
        assert_eq!(
            error_code(validate_intake_settings(&mut equal, &fs)),
            "DESTINATION_INSIDE_INTAKE"
        );
        let mut inside = settings(true, "/a/b", "/a/b/c");
        assert_eq!(
            error_code(validate_intake_settings(&mut inside, &fs)),
            "DESTINATION_INSIDE_INTAKE"
        );
        // `/a/bc` shares the string prefix `/a/b` but is a sibling, not a child.
        let mut sibling = settings(true, "/a/b", "/a/bc");
        assert!(validate_intake_settings(&mut sibling, &fs).is_ok());
        let mut outside = settings(true, "/a/b", "/a/other");
        assert!(validate_intake_settings(&mut outside, &fs).is_ok());
    }

    #[test]
    fn folders_are_written_back_in_canonical_form() {
        let fs = canonicalizer(&[
            ("/in-entered", "/in/canonical"),
            ("/out-entered", "/out/canonical"),
        ]);
        let mut enabled = settings(true, "/in-entered", "/out-entered");
        validate_intake_settings(&mut enabled, &fs).expect("valid settings");
        assert_eq!(enabled.intake_folder, "/in/canonical");
        assert_eq!(enabled.destination, "/out/canonical");
        // A non-blank intake folder is canonicalized even while disabled; a
        // missing one is an error, matching destination handling.
        let mut disabled = settings(false, "/in-entered", "");
        validate_intake_settings(&mut disabled, &fs).expect("valid settings");
        assert_eq!(disabled.intake_folder, "/in/canonical");
        let mut disabled_missing = settings(false, "/gone", "");
        assert_eq!(
            error_code(validate_intake_settings(&mut disabled_missing, &fs)),
            "INTAKE_FOLDER_MISSING"
        );
    }

    fn receipt(
        direction: OperationDirection,
        stage: OperationStage,
        destination: &str,
    ) -> OperationReceipt {
        OperationReceipt {
            id: 1,
            queue_item_id: 1,
            direction,
            source: PathBuf::from("/intake/original.pdf"),
            destination: PathBuf::from(destination),
            temporary_path: None,
            pre_operation_hash: "hash".into(),
            post_operation_hash: None,
            kind: OperationKind::Rename,
            stage,
            source_exists: false,
            destination_exists: true,
            temporary_exists: false,
        }
    }

    #[test]
    fn queue_statuses_map_onto_claim_item_states() {
        for status in [
            QueueStatus::Queued,
            QueueStatus::Extracting,
            QueueStatus::Analyzing,
            QueueStatus::Ready,
            QueueStatus::Applying,
        ] {
            assert_eq!(item_fate(status, None, None), ItemState::Active);
        }
        assert_eq!(
            item_fate(QueueStatus::NeedsReview, None, None),
            ItemState::NeedsReview
        );
        assert_eq!(
            item_fate(QueueStatus::Failed, None, None),
            ItemState::Failed
        );
        assert_eq!(
            item_fate(QueueStatus::Canceled, None, None),
            ItemState::Unknown
        );
    }

    #[test]
    fn completed_apply_reports_renamed_with_the_applied_filename() {
        let applied = receipt(
            OperationDirection::Apply,
            OperationStage::Complete,
            "/dest/2024-03-01 Contract.pdf",
        );
        assert_eq!(
            item_fate(QueueStatus::Completed, Some(&applied), Some("proposal.pdf")),
            ItemState::Done {
                outcome: DoneOutcome::Renamed,
                result_filename: Some("2024-03-01 Contract.pdf".into()),
            }
        );
        // No usable destination leaf: fall back to the proposal filename.
        let rootward = receipt(OperationDirection::Apply, OperationStage::Complete, "/");
        assert_eq!(
            item_fate(
                QueueStatus::Completed,
                Some(&rootward),
                Some("proposal.pdf")
            ),
            ItemState::Done {
                outcome: DoneOutcome::Renamed,
                result_filename: Some("proposal.pdf".into()),
            }
        );
    }

    #[test]
    fn completed_without_finished_apply_reports_kept_original() {
        let kept = ItemState::Done {
            outcome: DoneOutcome::KeptOriginal,
            result_filename: None,
        };
        assert_eq!(item_fate(QueueStatus::Completed, None, Some("x.pdf")), kept);
        let unfinished = receipt(
            OperationDirection::Apply,
            OperationStage::Published,
            "/dest/renamed.pdf",
        );
        assert_eq!(
            item_fate(QueueStatus::Completed, Some(&unfinished), None),
            kept
        );
        let undone = receipt(
            OperationDirection::Undo,
            OperationStage::Complete,
            "/intake/original.pdf",
        );
        assert_eq!(item_fate(QueueStatus::Completed, Some(&undone), None), kept);
    }

    #[test]
    fn provider_strings_match_the_wire_contract() {
        for (kind, expected) in [
            (CloudProviderKind::OneDrivePersonal, "onedrive_personal"),
            (CloudProviderKind::OneDriveBusiness, "onedrive_business"),
            (CloudProviderKind::SharePoint, "sharepoint"),
            (CloudProviderKind::NetworkShare, "network_share"),
        ] {
            assert_eq!(
                serde_json::to_value(CloudProviderDto::from(kind)).unwrap(),
                serde_json::Value::String(expected.into())
            );
            assert_eq!(
                kind.as_str(),
                expected,
                "the record format spells it the same way"
            );
        }
    }

    #[test]
    fn status_dto_serializes_camel_case_with_zeros_when_disabled() {
        let identity = MachineIdentity {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "Front desk".into(),
            user: "pat".into(),
        };
        let dto = status_dto(false, &identity, "", None, None, 1_755_850_000);
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["enabled"], false);
        assert_eq!(json["folder"], "");
        let verbatim = status_dto(
            true,
            &identity,
            r"\\?\C:\Users\pat\Scans",
            None,
            None,
            1_755_850_000,
        );
        assert_eq!(verbatim.folder, r"C:\Users\pat\Scans");
        assert_eq!(json["watching"], false);
        assert_eq!(json["machineId"], "0123456789abcdef0123456789abcdef");
        assert_eq!(json["machineName"], "Front desk");
        assert_eq!(json["cloud"], serde_json::Value::Null);
        assert_eq!(json["machines"], serde_json::json!([]));
        assert_eq!(json["heldForOthers"], 0);
        assert_eq!(json["unreadableFolders"], 0);
        assert_eq!(json["claimedByOthers"], 0);
        assert_eq!(json["processedHere"], 0);
        assert_eq!(json["lastScanAt"], serde_json::Value::Null);
        assert_eq!(json["error"], serde_json::Value::Null);
    }

    #[test]
    fn presence_is_active_within_the_window_of_now() {
        let now = 10_000;
        assert!(presence_active(now, now));
        assert!(presence_active(
            now - intern_intake::PRESENCE_ACTIVE_WINDOW_SECONDS,
            now
        ));
        assert!(!presence_active(
            now - intern_intake::PRESENCE_ACTIVE_WINDOW_SECONDS - 1,
            now
        ));
        // Clock skew across machines: a future stamp still counts as active.
        assert!(presence_active(now + 60, now));
    }
}

#[cfg(test)]
mod scheduler_tests {
    use super::{ExistingModelFilesDto, scheduler_actions};

    #[test]
    fn timer_recovers_but_does_not_drain_until_model_is_ready() {
        assert_eq!(scheduler_actions(true, false), (true, false));
        assert_eq!(scheduler_actions(false, false), (false, false));
        assert_eq!(scheduler_actions(false, true), (false, true));
        assert_eq!(scheduler_actions(true, true), (true, true));
    }

    #[test]
    fn existing_model_dto_is_exactly_one_backend_path() {
        let files: ExistingModelFilesDto = serde_json::from_value(serde_json::json!({
            "modelPath": "C:\\Models\\model.gguf"
        }))
        .unwrap();
        assert!(files.model_path.ends_with("model.gguf"));
        assert!(
            serde_json::from_value::<ExistingModelFilesDto>(serde_json::json!({
                "modelPath": { "name": "model.gguf" }
            }))
            .is_err()
        );
        // A projector path is not merely unused now; offering one must be an
        // error, so a stale caller cannot quietly ask for a file Intern will
        // never load.
        assert!(
            serde_json::from_value::<ExistingModelFilesDto>(serde_json::json!({
                "modelPath": "model.gguf",
                "projectorPath": "projector.gguf"
            }))
            .is_err()
        );
    }
}

#[cfg(test)]
mod history_tests {
    use std::path::PathBuf;

    use intern_core::{HistoryEntry, OperationDirection, OperationKind, OperationStage};

    use std::collections::HashMap;

    use super::{history_csv, history_entry_dto, iso8601_utc};

    fn entry(at: i64, original: &str, new: &str) -> HistoryEntry {
        HistoryEntry {
            receipt_id: 3,
            queue_item_id: 7,
            at,
            direction: OperationDirection::Apply,
            kind: OperationKind::Rename,
            stage: OperationStage::Complete,
            original_path: PathBuf::from(original),
            new_path: PathBuf::from(new),
        }
    }

    #[test]
    fn timestamps_render_as_iso_8601_utc_from_unix_seconds() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso8601_utc(1_713_173_696), "2024-04-15T09:34:56Z");
        assert_eq!(iso8601_utc(1_755_849_600), "2025-08-22T08:00:00Z");
        // Before the epoch still renders a real calendar date, not garbage.
        assert_eq!(iso8601_utc(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn history_csv_is_rfc_4180_with_quoting_only_where_needed() {
        let plain = entry(0, "C:\\drop\\scan.pdf", "C:\\filed\\2024 Agreement.pdf");
        let awkward = HistoryEntry {
            direction: OperationDirection::Undo,
            kind: OperationKind::VerifiedCopy,
            stage: OperationStage::RolledBack,
            ..entry(
                1_713_173_696,
                "C:\\drop\\comma, quote \" and\nnewline.pdf",
                "C:\\filed\\plain.pdf",
            )
        };

        let descriptions = HashMap::from([(
            7,
            "Lease agreement for a twelve-month term, beginning January 22, 2024.".to_owned(),
        )]);
        let csv = history_csv(&[awkward, plain], &descriptions);

        let mut lines = csv.split("\r\n");
        assert_eq!(
            lines.next(),
            Some("at,direction,kind,stage,originalPath,newPath,description")
        );
        assert_eq!(
            lines.next(),
            Some(
                "2024-04-15T09:34:56Z,undo,verified_copy,rolled_back,\
                 \"C:\\drop\\comma, quote \"\" and\nnewline.pdf\",C:\\filed\\plain.pdf,\
                 \"Lease agreement for a twelve-month term, beginning January 22, 2024.\""
            )
        );
        assert_eq!(
            lines.next(),
            Some(
                "1970-01-01T00:00:00Z,apply,rename,complete,C:\\drop\\scan.pdf,C:\\filed\\2024 Agreement.pdf,\
                 \"Lease agreement for a twelve-month term, beginning January 22, 2024.\""
            )
        );
        assert_eq!(lines.next(), Some(""));
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn history_dto_serializes_the_camel_case_wire_contract() {
        let json = serde_json::to_value(history_entry_dto(
            entry(
                1_713_173_696,
                "C:/drop/scan.pdf",
                "C:/filed/2024 Agreement.pdf",
            ),
            Some("A sentence.".to_owned()),
        ))
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "receiptId": "3",
                "queueItemId": "7",
                "at": 1_713_173_696i64,
                "direction": "apply",
                "kind": "rename",
                "stage": "complete",
                "originalPath": "C:/drop/scan.pdf",
                "newPath": "C:/filed/2024 Agreement.pdf",
                "description": "A sentence.",
            })
        );
        // Verbatim Windows paths are shown the way a person reads them.
        let json = serde_json::to_value(history_entry_dto(
            entry(
                0,
                r"\\?\C:\drop\scan.pdf",
                r"\\?\UNC\server\share\filed\a.pdf",
            ),
            None,
        ))
        .unwrap();
        assert_eq!(json["originalPath"], r"C:\drop\scan.pdf");
        assert_eq!(json["newPath"], r"\\server\share\filed\a.pdf");
        assert_eq!(json["description"], serde_json::Value::Null);
    }
}

#[cfg(test)]
mod duplicate_reason_tests {
    use std::path::PathBuf;

    use intern_core::{ErrorCode, QueueStatus};
    use intern_queue::PipelineItem;

    use super::queue_item_dto;

    fn duplicate_item(duplicate_of: Option<&str>) -> PipelineItem {
        PipelineItem {
            id: 7,
            source_path: PathBuf::from("C:/drop/copy.pdf"),
            source_hash: "hash".into(),
            status: QueueStatus::NeedsReview,
            processing_failures: 0,
            error_code: Some(ErrorCode::Duplicate),
            proposal: None,
            receipt: None,
            duplicate_of: duplicate_of.map(str::to_owned),
        }
    }

    #[test]
    fn duplicate_review_items_surface_the_filed_name_as_their_reason() {
        let dto = queue_item_dto(duplicate_item(Some("2024 - Filed Agreement.pdf"))).unwrap();
        assert_eq!(
            dto.reason.as_deref(),
            Some("Duplicate of 2024 - Filed Agreement.pdf")
        );
        assert_eq!(dto.error_code.as_deref(), Some("DUPLICATE"));

        // A cleared history leaves the flag without a referent: no fabricated
        // reason, and the item stays actionable through its error code.
        let stale = queue_item_dto(duplicate_item(None)).unwrap();
        assert_eq!(stale.reason, None);
        assert_eq!(stale.error_code.as_deref(), Some("DUPLICATE"));
    }
}
