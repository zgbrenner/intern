use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use intern_core::{OperationDirection, OperationStage, QueueStatus};
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
    AnalyzerBoundary, AppSettings, ModelFailure, Pipeline, PipelineError, PipelineEventSink,
    PipelineItem, PipelineProgress, SettingsStore,
    paths::{
        SUPPORTED_EXTENSIONS, canonical_file, canonical_folder, canonical_model_file,
        collect_supported_files, parse_item_id,
    },
};

use crate::intake::{
    CloudLocationDto, IntakeStatusDto, PipelineIntakeHost, classify_folder, now_unix, status_dto,
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
        let pipeline = Arc::new(Pipeline::with_local_files(
            data.join("queue.sqlite3"),
            Arc::new(SupervisedWorker::with_temp_root(
                executable_directory.join(worker_name),
                worker_temp_root,
            )),
            runtime,
            Arc::new(TauriPipelineEvents { app: app.clone() }),
            settings.clone(),
        )?);
        pipeline.recover()?;
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
            .clone();
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
}

fn intake_state_conflict() -> CommandError {
    CommandError {
        code: "STATE_CONFLICT".into(),
        message: "intake state is unavailable".into(),
    }
}

#[tauri::command]
pub fn queue_list(state: State<'_, AppState>) -> Result<Vec<QueueItemDto>, CommandError> {
    state
        .pipeline
        .list()?
        .into_iter()
        .map(queue_item_dto)
        .collect()
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

#[tauri::command]
pub fn settings_get(state: State<'_, AppState>) -> Result<AppSettings, CommandError> {
    state.settings.load().map_err(Into::into)
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
    state.settings.save(&settings)?;
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

#[tauri::command]
pub fn intake_status(state: State<'_, AppState>) -> Result<IntakeStatusDto, CommandError> {
    state.intake_status_dto()
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
            source_path: receipt.source.to_string_lossy().into_owned(),
            destination_path: receipt.destination.to_string_lossy().into_owned(),
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
            .map(|record| record.reasons.join(", ")),
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

    use super::validate_intake_settings;
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
        ] {
            assert_eq!(
                serde_json::to_value(CloudProviderDto::from(kind)).unwrap(),
                serde_json::Value::String(expected.into())
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
        assert_eq!(json["watching"], false);
        assert_eq!(json["machineId"], "0123456789abcdef0123456789abcdef");
        assert_eq!(json["machineName"], "Front desk");
        assert_eq!(json["cloud"], serde_json::Value::Null);
        assert_eq!(json["machines"], serde_json::json!([]));
        assert_eq!(json["heldForOthers"], 0);
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
