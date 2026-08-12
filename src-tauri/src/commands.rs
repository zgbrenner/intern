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
    ServerOptions, SupervisedWorker, engine::wants_vision, prepare_worker_temp_root,
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
use intern_queue::{
    AnalyzerBoundary, AppSettings, ModelFailure, Pipeline, PipelineError, PipelineEventSink,
    PipelineItem, PipelineProgress, SettingsStore,
    paths::{
        canonical_file, canonical_folder, canonical_model_file, collect_supported_files,
        parse_item_id,
    },
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
    pub projector_path: String,
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
    /// Whether the currently running server has the vision projector loaded.
    vision_loaded: AtomicBool,
}

impl RuntimeModel {
    fn new(executable: PathBuf, model_directory: PathBuf) -> Self {
        Self {
            executable,
            model_directory,
            engine: RwLock::new(None),
            server: Mutex::new(None),
            vision_loaded: AtomicBool::new(false),
        }
    }

    fn installed(&self, manifest: &ModelManifest) -> bool {
        manifest.files.iter().all(|file| {
            validate_selected_file(&self.model_directory.join(&file.name), file).is_ok()
        })
    }

    fn start(&self, manifest: &ModelManifest) -> Result<(), CommandError> {
        self.start_mode(manifest, false)
    }

    /// Starts the local server, loading the vision projector only when asked.
    ///
    /// Text-only is the normal mode: essentially every business document has
    /// usable text, and the projector costs hundreds of megabytes of resident
    /// memory that a 16 GB laptop would rather keep.
    fn start_mode(&self, manifest: &ModelManifest, vision: bool) -> Result<(), CommandError> {
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
        let projector = vision
            .then(|| manifest.projector())
            .flatten()
            .map(|file| self.model_directory.join(&file.name));
        let server = LlamaServer::start(
            &self.executable,
            &self.model_directory.join(&model.name),
            projector.as_deref(),
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
        self.vision_loaded.store(vision, Ordering::SeqCst);
        Ok(())
    }

    /// Reloads the model with its vision projector the first time a document
    /// genuinely cannot be read as text, and leaves it loaded afterwards.
    fn ensure_vision(&self) -> Result<(), ModelFailure> {
        if self.vision_loaded.load(Ordering::SeqCst) {
            return Ok(());
        }
        let manifest =
            ModelManifest::embedded().map_err(|_| ModelFailure::fatal("MODEL_NOT_READY"))?;
        if manifest.projector().is_none() {
            return Ok(());
        }
        self.stop_runtime()?;
        self.start_mode(&manifest, true)
            .map_err(|error| ModelFailure::fatal(error.code))
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
        self.vision_loaded.store(false, Ordering::SeqCst);
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
        if wants_vision(source) {
            self.ensure_vision()?;
        }
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
        if ready {
            if let Ok(scheduler) = self.scheduler.lock() {
                if let Some(sender) = scheduler.as_ref() {
                    let _ = sender.send(SchedulerMessage::Wake);
                }
            }
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

enum SchedulerMessage {
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
                    if recover {
                        if let Err(error) = scheduled_pipeline.recover() {
                            let _ = app.emit(
                                "queue://changed",
                                serde_json::json!({ "error": error.code }),
                            );
                        }
                    }
                    if drain {
                        if let Err(error) = scheduled_pipeline.run_until_idle() {
                            let _ = app.emit(
                                "queue://changed",
                                serde_json::json!({ "error": error.code }),
                            );
                        }
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
        if let Ok(join) = self.join.get_mut() {
            if let Some(join) = join.take() {
                let _ = join.join();
            }
        }
    }
}

pub struct AppState {
    pipeline: Arc<Pipeline>,
    settings: SettingsStore,
    setup: Arc<SetupManager>,
    scheduler: PipelineScheduler,
    app: AppHandle,
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
        if runtime.installed(&manifest) {
            if let Err(error) = runtime.start_verified(&manifest, &CancellationToken::new()) {
                let installed_bytes = manifest.total_bytes();
                setup.set_state(SetupStatus::Failed, installed_bytes, Some(error.code));
            }
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
        let state = Self {
            pipeline,
            settings,
            setup,
            scheduler,
            app: app.clone(),
        };
        if matches!(state.setup.get()?.state, SetupStatus::Ready) {
            state.schedule()?;
        }
        Ok(state)
    }

    fn schedule(&self) -> Result<(), CommandError> {
        if self.setup.model_ready.load(Ordering::SeqCst) {
            self.scheduler.wake()?;
        }
        Ok(())
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
    if !settings.destination.trim().is_empty() {
        settings.destination = canonical_folder(Path::new(&settings.destination))?
            .to_string_lossy()
            .into_owned();
    }
    state.settings.save(&settings)?;
    Ok(())
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
    let projector_path = canonical_model_file(Path::new(&files.projector_path))?;
    state.setup.choose_existing(ExistingModelSelection {
        model_path,
        projector_path,
    })
}

#[tauri::command]
pub fn history_clear(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.pipeline.clear_history()?;
    Ok(())
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
    fn existing_model_dto_is_a_strict_pair_of_backend_paths() {
        let files: ExistingModelFilesDto = serde_json::from_value(serde_json::json!({
            "modelPath": "C:\\Models\\model.gguf",
            "projectorPath": "C:\\Models\\projector.gguf"
        }))
        .unwrap();
        assert!(files.model_path.ends_with("model.gguf"));
        assert!(files.projector_path.ends_with("projector.gguf"));
        assert!(
            serde_json::from_value::<ExistingModelFilesDto>(serde_json::json!({
                "modelPath": { "name": "model.gguf" },
                "projectorPath": "projector.gguf"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ExistingModelFilesDto>(serde_json::json!({
                "modelPath": "model.gguf",
                "projectorPath": "projector.gguf",
                "extra": true
            }))
            .is_err()
        );
    }
}
