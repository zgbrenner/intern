use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use intern_core::{ModelProposal, OperationDirection, OperationStage, QueueStatus};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{
    model::{
        client::{DocumentInput, ModelClient},
        download::{
            CancellationToken, Downloader, ReqwestHttpTransport, SetupProgress, SystemDiskSpace,
        },
        manifest::ModelManifest,
        server::LlamaServer,
    },
    paths::{canonical_file, canonical_folder, collect_supported_files, parse_item_id},
    pipeline::{
        ModelBoundary, ModelFailure, Pipeline, PipelineError, PipelineEventSink, PipelineItem,
        PipelineProgress,
    },
    settings::{AppSettings, SettingsStore},
    worker::SupervisedWorker,
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

impl From<crate::model::ModelError> for CommandError {
    fn from(error: crate::model::ModelError) -> Self {
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
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceDto {
    date: Option<String>,
    r#type: Option<String>,
    parties: Option<String>,
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
    client: RwLock<Option<ModelClient>>,
    server: Mutex<Option<LlamaServer>>,
}

impl RuntimeModel {
    fn new(executable: PathBuf, model_directory: PathBuf) -> Self {
        Self {
            executable,
            model_directory,
            client: RwLock::new(None),
            server: Mutex::new(None),
        }
    }

    fn installed(&self, manifest: &ModelManifest) -> bool {
        manifest.files.iter().all(|file| {
            crate::model::download::validate_selected_file(
                &self.model_directory.join(&file.name),
                file,
            )
            .is_ok()
        })
    }

    fn start(&self, manifest: &ModelManifest) -> Result<(), CommandError> {
        if !self.installed(manifest) {
            return Err(CommandError {
                code: "MODEL_NOT_READY".into(),
                message: "model files are not installed".into(),
            });
        }
        let server = LlamaServer::start(
            &self.executable,
            &self.model_directory.join(&manifest.files[0].name),
            &self.model_directory.join(&manifest.files[1].name),
            Duration::from_secs(120),
        )?;
        let client = ModelClient::new(&server.completion_endpoint(), server.api_key().to_owned())?;
        *self.client.write().map_err(|_| CommandError {
            code: "MODEL_NOT_READY".into(),
            message: "model state is unavailable".into(),
        })? = Some(client);
        *self.server.lock().map_err(|_| CommandError {
            code: "MODEL_NOT_READY".into(),
            message: "model process state is unavailable".into(),
        })? = Some(server);
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
                .map(|mut server| {
                    server
                        .stop()
                        .map_err(|_| ModelFailure::fatal("MODEL_CANCEL_FAILED"))
                })
                .unwrap_or(Ok(()))
        };
        *self
            .client
            .write()
            .map_err(|_| ModelFailure::fatal("MODEL_CANCEL_FAILED"))? = None;
        stop_result
    }
}

impl ModelBoundary for RuntimeModel {
    fn propose(&self, document: &DocumentInput) -> Result<ModelProposal, ModelFailure> {
        let client = self
            .client
            .read()
            .map_err(|_| ModelFailure::fatal("MODEL_NOT_READY"))?;
        let client = client
            .as_ref()
            .ok_or_else(|| ModelFailure::fatal("MODEL_NOT_READY"))?;
        client
            .propose(document)
            .map_err(|error| ModelFailure::fatal(error.code().as_str()))
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
    running: AtomicBool,
    scheduler: Mutex<Option<std::sync::mpsc::Sender<SchedulerMessage>>>,
    model_ready: Arc<AtomicBool>,
}

impl SetupManager {
    fn new(app: AppHandle, runtime: Arc<RuntimeModel>, manifest: &ModelManifest) -> Self {
        let total_bytes = manifest.files.iter().map(|file| file.size).sum();
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
            running: AtomicBool::new(false),
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
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let manager = Arc::clone(self);
        std::thread::Builder::new()
            .name("intern-model-setup".into())
            .spawn(move || {
                manager.set_state(SetupStatus::Downloading, 0, None);
                let result = manager.download_and_start();
                match result {
                    Ok(total) => manager.set_state(SetupStatus::Ready, total, None),
                    Err(error) => manager.set_state(SetupStatus::Failed, 0, Some(error.code)),
                }
                manager.running.store(false, Ordering::SeqCst);
            })
            .map_err(|_| {
                self.running.store(false, Ordering::SeqCst);
                CommandError {
                    code: "SETUP_UNAVAILABLE".into(),
                    message: "setup thread could not start".into(),
                }
            })?;
        Ok(())
    }

    fn download_and_start(&self) -> Result<u64, CommandError> {
        let manifest = ModelManifest::embedded()?;
        let downloader = Downloader::new(ReqwestHttpTransport::new()?, SystemDiskSpace);
        let cancellation = CancellationToken::new();
        let total = manifest.files.iter().map(|file| file.size).sum::<u64>();
        let mut completed_before = 0;
        for file in &manifest.files {
            let offset = completed_before;
            downloader.download(
                file,
                &self.runtime.model_directory,
                &cancellation,
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
        self.runtime.start(&manifest)?;
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
            if let Err(error) = runtime.start(&manifest) {
                setup.set_state(SetupStatus::Failed, 0, Some(error.code));
            }
        }
        let settings = SettingsStore::new(data.join("settings.json"));
        let pipeline = Arc::new(Pipeline::with_local_files(
            data.join("queue.sqlite3"),
            Arc::new(SupervisedWorker::new(
                executable_directory.join(worker_name),
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
pub fn history_clear(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.pipeline.clear_history()?;
    Ok(())
}

fn queue_item_dto(item: PipelineItem) -> Result<QueueItemDto, CommandError> {
    let proposal = item.proposal.as_ref();
    let evidence = proposal.map(|record| EvidenceDto {
        date: record.outcome.proposal.evidence.date.clone(),
        r#type: record.outcome.proposal.evidence.document_type.clone(),
        parties: (!record.outcome.proposal.parties.is_empty())
            .then(|| record.outcome.proposal.parties.join("; ")),
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
        confidence: proposal.map(|record| record.outcome.proposal.confidence),
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
    })
}

#[cfg(test)]
mod scheduler_tests {
    use super::scheduler_actions;

    #[test]
    fn timer_recovers_but_does_not_drain_until_model_is_ready() {
        assert_eq!(scheduler_actions(true, false), (true, false));
        assert_eq!(scheduler_actions(false, false), (false, false));
        assert_eq!(scheduler_actions(false, true), (false, true));
        assert_eq!(scheduler_actions(true, true), (true, true));
    }
}
