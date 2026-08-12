use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
};

use intern_core::{
    ErrorCode, FileApplier, InternError, OperationReceipt, QueueItem, QueueStatus, QueueStore,
    StdFileSystem,
};
use intern_engine::{DocumentAnalysis, DocumentSource, ExtractProgress, ProposalStatus};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::settings::{AppSettings, SettingsStore};

const LEASE_RENEWAL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);
const LEASE_RENEWAL_ATTEMPTS: usize = 3;

pub struct LeaseKeeper {
    stop: Arc<(Mutex<bool>, Condvar)>,
    failure: Arc<Mutex<Option<PipelineError>>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl LeaseKeeper {
    pub fn start(
        store: Arc<QueueStore>,
        item_id: i64,
        interval: std::time::Duration,
    ) -> PipelineResult<Self> {
        Self::start_with_cancel(store, item_id, interval, Arc::new(|| {}))
    }

    fn start_with_cancel(
        store: Arc<QueueStore>,
        item_id: i64,
        interval: std::time::Duration,
        cancel: Arc<dyn Fn() + Send + Sync>,
    ) -> PipelineResult<Self> {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let failure = Arc::new(Mutex::new(None));
        let thread_stop = Arc::clone(&stop);
        let thread_failure = Arc::clone(&failure);
        let join = std::thread::Builder::new()
            .name(format!("intern-lease-{item_id}"))
            .spawn(move || {
                let (lock, wake) = &*thread_stop;
                let mut stopped = match lock.lock() {
                    Ok(value) => value,
                    Err(_) => return,
                };
                loop {
                    if *stopped {
                        return;
                    }
                    let waited = match wake.wait_timeout(stopped, interval) {
                        Ok(value) => value,
                        Err(_) => return,
                    };
                    stopped = waited.0;
                    if *stopped {
                        return;
                    }
                    drop(stopped);
                    let mut renewed = false;
                    for attempt in 0..LEASE_RENEWAL_ATTEMPTS {
                        match store.renew_lease(item_id) {
                            Ok(_) => {
                                renewed = true;
                                break;
                            }
                            Err(error) => {
                                let terminal = error.code() == ErrorCode::StateConflict
                                    || attempt + 1 == LEASE_RENEWAL_ATTEMPTS;
                                if terminal {
                                    if let Ok(mut failure) = thread_failure.lock() {
                                        *failure = Some(PipelineError::from(error));
                                    }
                                    cancel();
                                    return;
                                }
                                let retry_delay = interval.min(std::time::Duration::from_secs(2));
                                stopped = match lock.lock() {
                                    Ok(value) => value,
                                    Err(_) => return,
                                };
                                let waited = match wake.wait_timeout(stopped, retry_delay) {
                                    Ok(value) => value,
                                    Err(_) => return,
                                };
                                stopped = waited.0;
                                if *stopped {
                                    return;
                                }
                                drop(stopped);
                            }
                        }
                    }
                    if !renewed {
                        return;
                    }
                    stopped = match lock.lock() {
                        Ok(value) => value,
                        Err(_) => return,
                    };
                }
            })
            .map_err(|_| {
                PipelineError::new("STATE_CONFLICT", "lease renewal thread could not start")
            })?;
        Ok(Self {
            stop,
            failure,
            join: Some(join),
        })
    }

    pub fn check(&self) -> PipelineResult<()> {
        match self.failure.lock() {
            Ok(failure) => failure.clone().map_or(Ok(()), Err),
            Err(_) => Err(PipelineError::new(
                "STATE_CONFLICT",
                "lease renewal state is unavailable",
            )),
        }
    }

    pub fn stop_and_check(mut self) -> PipelineResult<()> {
        self.stop_thread()?;
        self.check()
    }

    fn stop_thread(&mut self) -> PipelineResult<()> {
        let (lock, wake) = &*self.stop;
        let mut stopped = lock.lock().map_err(|_| {
            PipelineError::new("STATE_CONFLICT", "lease renewal stop state is unavailable")
        })?;
        *stopped = true;
        wake.notify_all();
        drop(stopped);
        if self.join.take().is_some_and(|join| join.join().is_err()) {
            return Err(PipelineError::new(
                "STATE_CONFLICT",
                "lease renewal thread did not terminate cleanly",
            ));
        }
        Ok(())
    }
}

impl Drop for LeaseKeeper {
    fn drop(&mut self) {
        let _ = self.stop_thread();
    }
}

pub const PARSER_TIMEOUT_SECONDS: u64 = 30 * 60;
pub const MODEL_TIMEOUT_SECONDS: u64 = 15 * 60;

/// The extraction boundary, as the queue sees it.
pub use intern_engine::DocumentExtractor as WorkerBoundary;
/// Extraction failures, as the queue sees them.
pub use intern_engine::ExtractFailure as WorkerFailure;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PipelineError {
    pub code: String,
    pub message: String,
}

impl PipelineError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PipelineError {}
pub type PipelineResult<T> = Result<T, PipelineError>;

impl From<InternError> for PipelineError {
    fn from(error: InternError) -> Self {
        Self::new(error.code().as_str(), error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelFailure {
    pub code: String,
    pub retryable: bool,
}

impl ModelFailure {
    pub fn retryable(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            retryable: true,
        }
    }
    pub fn fatal(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            retryable: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineProgress {
    pub item_id: i64,
    pub stage: String,
    pub current: usize,
    pub total: Option<usize>,
}

/// The document-understanding boundary, as the queue sees it.
///
/// The queue knows nothing about distillation, prompts, or models; it hands
/// over extracted pages and receives a finished proposal. Swapping the engine
/// out, or driving it from a CLI or a watched folder instead, changes nothing
/// on this side of the line.
pub trait AnalyzerBoundary: Send + Sync {
    fn analyze(
        &self,
        source: &DocumentSource,
        extension: &str,
        existing_names: &[&str],
    ) -> Result<DocumentAnalysis, ModelFailure>;
    fn recover(&self, _failure: &ModelFailure) -> Result<(), ModelFailure> {
        Ok(())
    }
    fn cancel(&self) -> Result<(), ModelFailure> {
        Err(ModelFailure::fatal("MODEL_CANCEL_UNAVAILABLE"))
    }
    fn shutdown(&self) -> Result<(), ModelFailure> {
        Ok(())
    }
}

pub trait FileActions: Send + Sync {
    fn fingerprint(&self, path: &Path) -> PipelineResult<String>;
    fn apply(&self, item: &QueueItem, destination: &Path) -> PipelineResult<()>;
    fn undo(&self, item: &QueueItem, receipt: &OperationReceipt) -> PipelineResult<()>;
    fn reconcile(&self, item: &QueueItem) -> PipelineResult<()>;
}

pub struct CoreFileActions {
    store: Arc<QueueStore>,
    applier: FileApplier,
}

impl CoreFileActions {
    pub fn local(store: Arc<QueueStore>) -> Self {
        Self {
            applier: FileApplier::new(Arc::new(StdFileSystem), Arc::clone(&store)),
            store,
        }
    }
}

impl FileActions for CoreFileActions {
    fn fingerprint(&self, path: &Path) -> PipelineResult<String> {
        self.applier.fingerprint(path).map_err(Into::into)
    }

    fn apply(&self, item: &QueueItem, destination: &Path) -> PipelineResult<()> {
        self.store.begin_applying(item.id, QueueStatus::Ready)?;
        let lease = LeaseKeeper::start(Arc::clone(&self.store), item.id, LEASE_RENEWAL_INTERVAL)?;
        let result = self
            .applier
            .apply(item.id, &item.source_path, destination, &item.source_hash);
        lease.stop_and_check()?;
        match result {
            Ok(receipt) => {
                self.store.complete_apply(item.id, receipt.id)?;
                Ok(())
            }
            Err(error) => {
                if error.receipt().is_none() {
                    let _ = self.applier.reconcile(item.id);
                }
                Err(error.into())
            }
        }
    }

    fn undo(&self, item: &QueueItem, receipt: &OperationReceipt) -> PipelineResult<()> {
        self.store.begin_applying(item.id, QueueStatus::Completed)?;
        let lease = LeaseKeeper::start(Arc::clone(&self.store), item.id, LEASE_RENEWAL_INTERVAL)?;
        let result = self.applier.undo(item.id, receipt);
        lease.stop_and_check()?;
        match result {
            Ok(undo_receipt) => {
                self.store.complete_undo(item.id, undo_receipt.id)?;
                Ok(())
            }
            Err(error) => {
                if error.receipt().is_none() {
                    let _ = self.applier.reconcile(item.id);
                }
                Err(error.into())
            }
        }
    }

    fn reconcile(&self, item: &QueueItem) -> PipelineResult<()> {
        self.applier
            .reconcile(item.id)
            .map(|_| ())
            .map_err(Into::into)
    }
}

pub trait PipelineEventSink: Send + Sync {
    fn queue_changed(&self);
    fn progress(&self, progress: PipelineProgress);
}

/// What the queue stores about one proposal.
///
/// `analysis` is exactly what the engine produced and never changes; `filename`
/// and `description` are what will actually be applied, and a human edit
/// changes only those. Keeping them apart means the evidence a reviewer sees
/// still belongs to the model's own answer.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalRecord {
    pub analysis: DocumentAnalysis,
    pub status: ProposalStatus,
    pub filename: String,
    pub description: String,
    pub reasons: Vec<String>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PipelineItem {
    pub id: i64,
    pub source_path: PathBuf,
    pub source_hash: String,
    pub status: QueueStatus,
    pub processing_failures: u32,
    pub error_code: Option<ErrorCode>,
    pub proposal: Option<ProposalRecord>,
    pub receipt: Option<OperationReceipt>,
}

pub struct Pipeline {
    store: Arc<QueueStore>,
    repository: PipelineRepository,
    worker: Arc<dyn WorkerBoundary>,
    model: Arc<dyn AnalyzerBoundary>,
    files: Arc<dyn FileActions>,
    events: Arc<dyn PipelineEventSink>,
    settings: SettingsStore,
    paused: AtomicBool,
    active_item: AtomicI64,
    shutting_down: AtomicBool,
    model_timeout: std::time::Duration,
    lease_renewal_interval: std::time::Duration,
    run_lock: Mutex<()>,
}

#[derive(Clone, Copy)]
enum LeasePhase {
    Extracting,
    Analyzing,
}

impl Pipeline {
    pub fn open(
        database: impl AsRef<Path>,
        worker: Arc<dyn WorkerBoundary>,
        model: Arc<dyn AnalyzerBoundary>,
        files: Arc<dyn FileActions>,
        events: Arc<dyn PipelineEventSink>,
        settings: SettingsStore,
    ) -> PipelineResult<Self> {
        let database = database.as_ref();
        let store = Arc::new(QueueStore::open(database)?);
        let repository = PipelineRepository::open(database)?;
        Ok(Self {
            store,
            repository,
            worker,
            model,
            files,
            events,
            settings,
            paused: AtomicBool::new(false),
            active_item: AtomicI64::new(0),
            shutting_down: AtomicBool::new(false),
            model_timeout: std::time::Duration::from_secs(MODEL_TIMEOUT_SECONDS),
            lease_renewal_interval: LEASE_RENEWAL_INTERVAL,
            run_lock: Mutex::new(()),
        })
    }

    pub fn with_local_files(
        database: impl AsRef<Path>,
        worker: Arc<dyn WorkerBoundary>,
        model: Arc<dyn AnalyzerBoundary>,
        events: Arc<dyn PipelineEventSink>,
        settings: SettingsStore,
    ) -> PipelineResult<Self> {
        let database = database.as_ref();
        let store = Arc::new(QueueStore::open(database)?);
        let repository = PipelineRepository::open(database)?;
        let files = Arc::new(CoreFileActions::local(Arc::clone(&store)));
        Ok(Self {
            store,
            repository,
            worker,
            model,
            files,
            events,
            settings,
            paused: AtomicBool::new(false),
            active_item: AtomicI64::new(0),
            shutting_down: AtomicBool::new(false),
            model_timeout: std::time::Duration::from_secs(MODEL_TIMEOUT_SECONDS),
            lease_renewal_interval: LEASE_RENEWAL_INTERVAL,
            run_lock: Mutex::new(()),
        })
    }

    #[doc(hidden)]
    pub fn with_model_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.model_timeout = timeout;
        self
    }

    #[doc(hidden)]
    pub fn with_lease_renewal_interval(mut self, interval: std::time::Duration) -> Self {
        self.lease_renewal_interval = interval;
        self
    }

    pub fn enqueue_files(&self, paths: &[PathBuf]) -> PipelineResult<Vec<QueueItem>> {
        let mut queued = Vec::with_capacity(paths.len());
        for path in paths {
            let fingerprint = self.files.fingerprint(path)?;
            queued.push(self.store.enqueue(path, &fingerprint)?);
        }
        if !queued.is_empty() {
            self.events.queue_changed();
        }
        Ok(queued)
    }

    pub fn list(&self) -> PipelineResult<Vec<PipelineItem>> {
        self.store
            .list()?
            .into_iter()
            .map(|item| {
                let proposal = self.repository.load_proposal(item.id)?;
                let receipt = self.store.load_receipt(item.id)?;
                Ok(PipelineItem {
                    id: item.id,
                    source_path: item.source_path,
                    source_hash: item.source_hash,
                    status: item.status,
                    processing_failures: item.processing_failures,
                    error_code: item.error_code,
                    proposal,
                    receipt,
                })
            })
            .collect()
    }

    pub fn run_until_idle(&self) -> PipelineResult<()> {
        let _run = self
            .run_lock
            .lock()
            .map_err(|_| PipelineError::new("STATE_CONFLICT", "pipeline lock is unavailable"))?;
        self.apply_pending_automatic_ready()?;
        while !self.paused.load(Ordering::SeqCst) {
            if !self.run_next_inner()? {
                break;
            }
        }
        Ok(())
    }

    pub fn run_next(&self) -> PipelineResult<()> {
        let _run = self
            .run_lock
            .lock()
            .map_err(|_| PipelineError::new("STATE_CONFLICT", "pipeline lock is unavailable"))?;
        if !self.paused.load(Ordering::SeqCst) {
            self.apply_pending_automatic_ready()?;
            let _ = self.run_next_inner()?;
        }
        Ok(())
    }

    fn run_next_inner(&self) -> PipelineResult<bool> {
        let Some(item) = self.store.claim_next()? else {
            return Ok(false);
        };
        self.active_item.store(item.id, Ordering::SeqCst);
        let request_id = format!("queue-{}-{}", item.id, item.processing_failures + 1);
        let phase = Arc::new(Mutex::new(LeasePhase::Extracting));
        let cancel_phase = Arc::clone(&phase);
        let cancel_worker = Arc::clone(&self.worker);
        let cancel_model = Arc::clone(&self.model);
        let cancel_request_id = request_id.clone();
        let lease = match LeaseKeeper::start_with_cancel(
            Arc::clone(&self.store),
            item.id,
            self.lease_renewal_interval,
            Arc::new(move || match cancel_phase.lock().map(|phase| *phase) {
                Ok(LeasePhase::Extracting) => {
                    let _ = cancel_worker.cancel(&cancel_request_id);
                }
                Ok(LeasePhase::Analyzing) => {
                    let _ = cancel_model.cancel();
                }
                Err(_) => {
                    let _ = cancel_worker.shutdown();
                    let _ = cancel_model.shutdown();
                }
            }),
        ) {
            Ok(lease) => lease,
            Err(error) => {
                self.active_item.store(0, Ordering::SeqCst);
                self.store
                    .record_processing_failure(item.id, ErrorCode::IoError)?;
                self.events.queue_changed();
                return Err(error);
            }
        };
        self.events.queue_changed();
        let mut forward_progress = |progress: ExtractProgress| {
            self.events.progress(PipelineProgress {
                item_id: item.id,
                stage: progress.stage,
                current: progress.current,
                total: progress.total,
            });
        };
        let source =
            match self
                .worker
                .extract(&request_id, &item.source_path, &mut forward_progress)
            {
                Ok(source) => source,
                Err(error) => {
                    self.active_item.store(0, Ordering::SeqCst);
                    if self.shutting_down.load(Ordering::SeqCst) {
                        return Err(PipelineError::new(
                            "SHUTTING_DOWN",
                            "pipeline is shutting down",
                        ));
                    }
                    if let Err(lease_error) = lease.check() {
                        self.paused.store(true, Ordering::SeqCst);
                        self.events.queue_changed();
                        return Err(lease_error);
                    }
                    if error.canceled {
                        self.events.queue_changed();
                        return Ok(true);
                    }
                    let restart_failed = error.crashed
                        && item.processing_failures == 0
                        && self.worker.restart().is_err();
                    self.store
                        .record_processing_failure(item.id, ErrorCode::IoError)?;
                    if restart_failed {
                        if let Some(reclaimed) = self.store.claim_next()? {
                            if reclaimed.id == item.id {
                                self.store
                                    .record_processing_failure(item.id, ErrorCode::IoError)?;
                            }
                        }
                    }
                    self.events.queue_changed();
                    return Ok(true);
                }
            };
        self.ensure_lease(&lease)?;
        if self.paused.load(Ordering::SeqCst) {
            lease.stop_and_check()?;
            self.store
                .transition(item.id, QueueStatus::Extracting, QueueStatus::Queued, None)?;
            self.active_item.store(0, Ordering::SeqCst);
            self.events.queue_changed();
            return Ok(true);
        }
        self.store.transition(
            item.id,
            QueueStatus::Extracting,
            QueueStatus::Analyzing,
            None,
        )?;
        *phase
            .lock()
            .map_err(|_| PipelineError::new("STATE_CONFLICT", "lease phase is unavailable"))? =
            LeasePhase::Analyzing;
        self.events.queue_changed();
        let extension = item
            .source_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_owned();
        let existing = existing_names(item.source_path.parent().unwrap_or_else(|| Path::new(".")));
        let analysis = match self.analyze_with_deadline(&source, &extension, &existing) {
            Ok(analysis) => analysis,
            Err(error) => {
                self.active_item.store(0, Ordering::SeqCst);
                if self.shutting_down.load(Ordering::SeqCst) {
                    return Err(PipelineError::new(
                        "SHUTTING_DOWN",
                        "pipeline is shutting down",
                    ));
                }
                if let Err(lease_error) = lease.check() {
                    self.paused.store(true, Ordering::SeqCst);
                    self.events.queue_changed();
                    return Err(lease_error);
                }
                if self.store.list()?.iter().any(|candidate| {
                    candidate.id == item.id && candidate.status == QueueStatus::Canceled
                }) {
                    self.events.queue_changed();
                    return Ok(true);
                }
                self.store
                    .record_processing_failure(item.id, model_error_code(&error))?;
                if matches!(
                    error.code.as_str(),
                    "MODEL_CANCEL_FAILED"
                        | "MODEL_RECOVERY_FAILED"
                        | "MODEL_REQUEST_FAILED"
                        | "MODEL_RESPONSE_INVALID"
                ) {
                    self.paused.store(true, Ordering::SeqCst);
                }
                self.events.queue_changed();
                return Ok(true);
            }
        };
        self.ensure_lease(&lease)?;
        let record = ProposalRecord {
            status: analysis.status,
            filename: analysis.filename.clone(),
            description: analysis.description.clone(),
            reasons: analysis
                .review_reasons
                .iter()
                .map(|reason| reason.as_str().to_owned())
                .collect(),
            analysis,
            revision: 1,
        };
        let next = match record.status {
            ProposalStatus::Ready => QueueStatus::Ready,
            ProposalStatus::NeedsReview => QueueStatus::NeedsReview,
        };
        lease.stop_and_check()?;
        self.repository.save_initial_and_transition(
            item.id,
            &record,
            next,
            self.store.session_id(),
        )?;
        let ready_item = self
            .store
            .list()?
            .into_iter()
            .find(|candidate| candidate.id == item.id)
            .ok_or_else(|| {
                PipelineError::new(
                    "ITEM_NOT_FOUND",
                    "queue item disappeared after proposal storage",
                )
            })?;
        self.active_item.store(0, Ordering::SeqCst);
        self.events.queue_changed();
        if next == QueueStatus::Ready {
            let settings = match self.settings.load() {
                Ok(settings) => settings,
                Err(error) => {
                    self.repository.mark_needs_review(item.id, &error.code)?;
                    self.events.queue_changed();
                    return Ok(true);
                }
            };
            if settings.automatic_rename && !self.paused.load(Ordering::SeqCst) {
                let _ = self.apply_if_unchanged(&ready_item, &record.filename, &settings);
            }
        }
        Ok(true)
    }

    fn apply_pending_automatic_ready(&self) -> PipelineResult<()> {
        if self.paused.load(Ordering::SeqCst) {
            return Ok(());
        }
        let ready = self
            .list()?
            .into_iter()
            .filter(|item| item.status == QueueStatus::Ready)
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return Ok(());
        }
        let settings = self.settings.load()?;
        if !settings.automatic_rename {
            return Ok(());
        }
        for item in ready {
            if self.paused.load(Ordering::SeqCst) {
                break;
            }
            let Some(proposal) = item.proposal else {
                self.repository
                    .mark_needs_review(item.id, "PROPOSAL_MISSING")?;
                continue;
            };
            let queue_item = self
                .store
                .list()?
                .into_iter()
                .find(|candidate| candidate.id == item.id)
                .ok_or_else(|| PipelineError::new("ITEM_NOT_FOUND", "queue item does not exist"))?;
            let _ = self.apply_if_unchanged(&queue_item, &proposal.filename, &settings);
        }
        Ok(())
    }

    fn ensure_lease(&self, lease: &LeaseKeeper) -> PipelineResult<()> {
        if let Err(error) = lease.check() {
            self.active_item.store(0, Ordering::SeqCst);
            self.paused.store(true, Ordering::SeqCst);
            self.events.queue_changed();
            return Err(error);
        }
        Ok(())
    }

    fn analyze_with_deadline(
        &self,
        source: &DocumentSource,
        extension: &str,
        existing_names: &[String],
    ) -> Result<DocumentAnalysis, ModelFailure> {
        let model = Arc::clone(&self.model);
        let request_model = Arc::clone(&model);
        let source = source.clone();
        let extension = extension.to_owned();
        let existing_names = existing_names.to_vec();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let join = std::thread::Builder::new()
            .name("intern-model-request".into())
            .spawn(move || {
                let existing = existing_names
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                let result = match request_model.analyze(&source, &extension, &existing) {
                    Err(error) if error.retryable => request_model
                        .recover(&error)
                        .and_then(|()| request_model.analyze(&source, &extension, &existing)),
                    result => result,
                };
                let _ = sender.send(result);
            })
            .map_err(|_| ModelFailure::fatal("MODEL_REQUEST_FAILED"))?;
        match receiver.recv_timeout(self.model_timeout) {
            Ok(result) => {
                let _ = join.join();
                result
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let canceled = model.cancel();
                let _ = receiver.recv();
                let _ = join.join();
                match canceled {
                    Ok(()) => Err(ModelFailure::fatal("MODEL_TIMEOUT")),
                    Err(_) => Err(ModelFailure::fatal("MODEL_CANCEL_FAILED")),
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = join.join();
                Err(ModelFailure::fatal("MODEL_REQUEST_FAILED"))
            }
        }
    }

    fn apply_if_unchanged(
        &self,
        item: &QueueItem,
        filename: &str,
        settings: &AppSettings,
    ) -> PipelineResult<()> {
        let fingerprint = match self.files.fingerprint(&item.source_path) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                self.repository.mark_needs_review(item.id, &error.code)?;
                self.events.queue_changed();
                return Err(error);
            }
        };
        if fingerprint != item.source_hash {
            self.repository.mark_needs_review(item.id, "FILE_CHANGED")?;
            self.events.queue_changed();
            return Ok(());
        }
        let destination_root = if settings.destination.trim().is_empty() {
            item.source_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        } else {
            PathBuf::from(&settings.destination)
        };
        if let Err(error) = self.files.apply(item, &destination_root.join(filename)) {
            // Core file operations journal ambiguous failures in Applying. Try to settle
            // them now; the scheduler also retries reconciliation periodically.
            let _ = self.files.reconcile(item);
            if self
                .store
                .list()?
                .iter()
                .any(|current| current.id == item.id && current.status == QueueStatus::Ready)
            {
                self.repository.mark_needs_review(item.id, &error.code)?;
            }
            self.events.queue_changed();
            return Err(error);
        }
        self.events.queue_changed();
        Ok(())
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        self.events.queue_changed();
    }
    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
        self.events.queue_changed();
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn shutdown(&self) -> PipelineResult<()> {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.paused.store(true, Ordering::SeqCst);
        let worker = self.worker.shutdown().map_err(worker_error);
        let model = self.model.shutdown().map_err(|error| {
            PipelineError::new(error.code, "local model process could not be stopped")
        });
        self.events.queue_changed();
        worker.and(model)
    }

    pub fn cancel(&self, id: i64) -> PipelineResult<()> {
        let item = self
            .store
            .list()?
            .into_iter()
            .find(|item| item.id == id)
            .ok_or_else(|| PipelineError::new("ITEM_NOT_FOUND", "queue item does not exist"))?;
        if item.status == QueueStatus::NeedsReview
            && item.error_code == Some(ErrorCode::SourceDeleteFailed)
        {
            return Err(PipelineError::new(
                "RECONCILIATION_REQUIRED",
                "retry the verified source deletion or resolve the files manually",
            ));
        }
        match item.status {
            QueueStatus::Queued
            | QueueStatus::Extracting
            | QueueStatus::Analyzing
            | QueueStatus::Ready
            | QueueStatus::NeedsReview => {
                self.store
                    .transition(id, item.status, QueueStatus::Canceled, None)?;
            }
            QueueStatus::Canceled => {}
            QueueStatus::Applying => {
                return Err(PipelineError::new(
                    "STATE_CONFLICT",
                    "an atomic file operation cannot be canceled",
                ));
            }
            QueueStatus::Failed | QueueStatus::Completed => {
                return Err(PipelineError::new(
                    "INVALID_TRANSITION",
                    "item cannot be canceled",
                ));
            }
        }
        if self.active_item.load(Ordering::SeqCst) == id {
            match item.status {
                QueueStatus::Extracting => {
                    let request_id = format!("queue-{}-{}", id, item.processing_failures + 1);
                    self.worker.cancel(&request_id).map_err(worker_error)?;
                }
                QueueStatus::Analyzing => self.model.cancel().map_err(|error| {
                    PipelineError::new(error.code, "local model request could not be canceled")
                })?,
                _ => {}
            }
        }
        self.events.queue_changed();
        Ok(())
    }

    pub fn retry(&self, id: i64) -> PipelineResult<()> {
        let item = self
            .store
            .list()?
            .into_iter()
            .find(|item| item.id == id)
            .ok_or_else(|| PipelineError::new("ITEM_NOT_FOUND", "queue item does not exist"))?;
        if item.status == QueueStatus::NeedsReview
            && item.error_code == Some(ErrorCode::SourceDeleteFailed)
        {
            let claimed = self.store.claim_deferred_reconciliation(id)?;
            let result = self.files.reconcile(&claimed);
            self.events.queue_changed();
            return result;
        }
        match item.status {
            QueueStatus::Failed => {
                self.store.manual_retry(id)?;
            }
            QueueStatus::Canceled => self.repository.retry_canceled(id)?,
            _ => {
                return Err(PipelineError::new(
                    "INVALID_TRANSITION",
                    "only failed or canceled items can be retried",
                ));
            }
        }
        self.repository.delete_proposal(id)?;
        self.events.queue_changed();
        Ok(())
    }

    pub fn remove(&self, id: i64) -> PipelineResult<()> {
        self.reject_deferred_reconciliation_mutation(id)?;
        self.repository.remove_item(id)?;
        self.events.queue_changed();
        Ok(())
    }

    pub fn approve(&self, id: i64, filename: &str, description: &str) -> PipelineResult<()> {
        let filename = validate_leaf_filename(filename)?;
        let item = self
            .store
            .list()?
            .into_iter()
            .find(|item| item.id == id)
            .ok_or_else(|| PipelineError::new("ITEM_NOT_FOUND", "queue item does not exist"))?;
        if item.error_code == Some(ErrorCode::SourceDeleteFailed) {
            return Err(PipelineError::new(
                "RECONCILIATION_REQUIRED",
                "retry the verified source deletion or resolve the files manually",
            ));
        }
        let source_extension = item
            .source_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let approved_extension = Path::new(&filename)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if source_extension.is_empty() || !source_extension.eq_ignore_ascii_case(approved_extension)
        {
            return Err(PipelineError::new(
                "NAME_INVALID",
                "approved filename must preserve the source extension",
            ));
        }
        if !matches!(item.status, QueueStatus::NeedsReview | QueueStatus::Ready) {
            return Err(PipelineError::new(
                "INVALID_TRANSITION",
                "proposal is not reviewable",
            ));
        }
        self.repository
            .approve_user_edit(id, item.status, &filename, description)?;
        let ready = self
            .store
            .list()?
            .into_iter()
            .find(|candidate| candidate.id == id)
            .unwrap();
        let settings = match self.settings.load() {
            Ok(settings) => settings,
            Err(error) => {
                self.repository.mark_needs_review(id, &error.code)?;
                self.events.queue_changed();
                return Err(error);
            }
        };
        self.apply_if_unchanged(&ready, &filename, &settings)
    }

    pub fn keep_original(&self, id: i64) -> PipelineResult<()> {
        let item = self
            .store
            .list()?
            .into_iter()
            .find(|item| item.id == id)
            .ok_or_else(|| PipelineError::new("ITEM_NOT_FOUND", "queue item does not exist"))?;
        if item.error_code == Some(ErrorCode::SourceDeleteFailed) {
            return Err(PipelineError::new(
                "RECONCILIATION_REQUIRED",
                "retry the verified source deletion or resolve the files manually",
            ));
        }
        self.store.complete_keep_original(id, item.status)?;
        self.events.queue_changed();
        Ok(())
    }

    pub fn undo(&self, id: i64) -> PipelineResult<()> {
        let item = self
            .store
            .list()?
            .into_iter()
            .find(|item| item.id == id)
            .ok_or_else(|| PipelineError::new("ITEM_NOT_FOUND", "queue item does not exist"))?;
        if item.status != QueueStatus::Completed {
            return Err(PipelineError::new(
                "INVALID_TRANSITION",
                "only completed operations can be undone",
            ));
        }
        let receipt = self.store.load_receipt(id)?.ok_or_else(|| {
            PipelineError::new(
                "STATE_CONFLICT",
                "completed item has no durable operation receipt",
            )
        })?;
        self.files.undo(&item, &receipt)?;
        self.events.queue_changed();
        Ok(())
    }

    pub fn clear_history(&self) -> PipelineResult<usize> {
        let removed = self.store.clear_terminal()?;
        self.events.queue_changed();
        Ok(removed)
    }

    fn reject_deferred_reconciliation_mutation(&self, id: i64) -> PipelineResult<()> {
        let item = self
            .store
            .list()?
            .into_iter()
            .find(|item| item.id == id)
            .ok_or_else(|| PipelineError::new("ITEM_NOT_FOUND", "queue item does not exist"))?;
        if item.status == QueueStatus::NeedsReview
            && item.error_code == Some(ErrorCode::SourceDeleteFailed)
        {
            return Err(PipelineError::new(
                "RECONCILIATION_REQUIRED",
                "retry the verified source deletion or resolve the files manually",
            ));
        }
        Ok(())
    }

    pub fn recover(&self) -> PipelineResult<()> {
        let interrupted = self
            .store
            .list()?
            .into_iter()
            .filter(|item| {
                matches!(
                    item.status,
                    QueueStatus::Extracting | QueueStatus::Analyzing
                )
            })
            .map(|item| item.id)
            .collect::<Vec<_>>();
        self.store.recover_interrupted()?;
        for id in interrupted {
            if self
                .store
                .list()?
                .iter()
                .any(|item| item.id == id && item.status == QueueStatus::Queued)
            {
                self.repository.record_recovered_failure(id)?;
            }
        }
        for item in self
            .store
            .list()?
            .into_iter()
            .filter(|item| item.status == QueueStatus::Applying)
        {
            // A still-running app may own the ambiguous operation. Its file boundary can
            // reconcile under that lease without waiting for its own session to go stale.
            if self.files.reconcile(&item).is_err() {
                if let Ok(claimed) = self.store.claim_applying_reconciliation(item.id) {
                    let _ = self.files.reconcile(&claimed);
                }
            }
        }
        self.events.queue_changed();
        Ok(())
    }
}

struct PipelineRepository {
    connection: Mutex<Connection>,
}

impl PipelineRepository {
    fn open(path: &Path) -> PipelineResult<Self> {
        let mut connection = Connection::open(path).map_err(database_error)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(database_error)?;
        connection
            .execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(database_error)?;
        let legacy_exists = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'pipeline_proposals')",
            [], |row| row.get::<_, bool>(0),
        ).map_err(database_error)?;
        if legacy_exists {
            let transaction = connection.transaction().map_err(database_error)?;
            transaction
                .execute(
                    "INSERT OR REPLACE INTO proposals(queue_item_id, proposal_json, created_at)
                 SELECT queue_item_id, record_json, unixepoch() FROM pipeline_proposals",
                    [],
                )
                .map_err(database_error)?;
            transaction
                .execute("DROP TABLE pipeline_proposals", [])
                .map_err(database_error)?;
            transaction.commit().map_err(database_error)?;
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn save_initial_and_transition(
        &self,
        id: i64,
        record: &ProposalRecord,
        next: QueueStatus,
        session_id: &str,
    ) -> PipelineResult<()> {
        let json = serde_json::to_string(record)
            .map_err(|_| PipelineError::new("INVALID_DATA", "proposal could not be stored"))?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(database_error)?;
        let changed = transaction
            .execute(
                "UPDATE queue_items SET status = ?1, owner_session = NULL, lease_expires_at = NULL,
             error_code = NULL, updated_at = unixepoch()
             WHERE id = ?2 AND status = 'analyzing' AND owner_session = ?3",
                params![queue_status_text(next), id, session_id],
            )
            .map_err(database_error)?;
        if changed != 1 {
            return Err(PipelineError::new(
                "STATE_CONFLICT",
                "analyzing proposal transition compare-and-swap failed",
            ));
        }
        transaction.execute(
            "INSERT INTO proposals(queue_item_id, proposal_json, created_at) VALUES (?1, ?2, unixepoch())
             ON CONFLICT(queue_item_id) DO UPDATE SET proposal_json = excluded.proposal_json, created_at = excluded.created_at",
            params![id, json],
        ).map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    fn load_proposal(&self, id: i64) -> PipelineResult<Option<ProposalRecord>> {
        let json = self
            .lock()?
            .query_row(
                "SELECT proposal_json FROM proposals WHERE queue_item_id = ?1",
                params![id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(database_error)?;
        json.map(|value| {
            serde_json::from_str(&value)
                .map_err(|_| PipelineError::new("INVALID_DATA", "stored proposal is invalid"))
        })
        .transpose()
    }

    fn delete_proposal(&self, id: i64) -> PipelineResult<()> {
        self.lock()?
            .execute(
                "DELETE FROM proposals WHERE queue_item_id = ?1",
                params![id],
            )
            .map_err(database_error)?;
        Ok(())
    }

    fn mark_needs_review(&self, id: i64, reason: &str) -> PipelineResult<()> {
        let mut record = self
            .load_proposal(id)?
            .ok_or_else(|| PipelineError::new("INVALID_DATA", "proposal is missing"))?;
        record.status = ProposalStatus::NeedsReview;
        if !record.reasons.iter().any(|entry| entry == reason) {
            record.reasons.push(reason.to_owned());
        }
        record.revision += 1;
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(database_error)?;
        let changed = transaction.execute(
            "UPDATE queue_items SET status = 'needs_review', updated_at = unixepoch() WHERE id = ?1 AND status = 'ready'",
            params![id],
        ).map_err(database_error)?;
        if changed != 1 {
            return Err(PipelineError::new(
                "STATE_CONFLICT",
                "ready item changed before review",
            ));
        }
        let json = serde_json::to_string(&record)
            .map_err(|_| PipelineError::new("INVALID_DATA", "proposal could not be stored"))?;
        transaction.execute("UPDATE proposals SET proposal_json = ?1, created_at = unixepoch() WHERE queue_item_id = ?2", params![json, id]).map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    fn retry_canceled(&self, id: i64) -> PipelineResult<()> {
        let changed = self.lock()?.execute(
            "UPDATE queue_items SET status = 'queued', processing_failures = 0, error_code = NULL,
             owner_session = NULL, lease_expires_at = NULL, updated_at = unixepoch()
             WHERE id = ?1 AND status = 'canceled'",
            params![id],
        ).map_err(database_error)?;
        if changed == 1 {
            Ok(())
        } else {
            Err(PipelineError::new(
                "STATE_CONFLICT",
                "canceled retry compare-and-swap failed",
            ))
        }
    }

    fn record_recovered_failure(&self, id: i64) -> PipelineResult<()> {
        let changed = self
            .lock()?
            .execute(
                "UPDATE queue_items
             SET processing_failures = processing_failures + 1,
                 status = CASE WHEN processing_failures + 1 >= 2 THEN 'failed' ELSE 'queued' END,
                 error_code = 'IO_ERROR', updated_at = unixepoch()
             WHERE id = ?1 AND status = 'queued' AND owner_session IS NULL",
                params![id],
            )
            .map_err(database_error)?;
        if changed == 1 {
            Ok(())
        } else {
            Err(PipelineError::new(
                "STATE_CONFLICT",
                "recovered item changed before failure accounting",
            ))
        }
    }

    fn remove_item(&self, id: i64) -> PipelineResult<()> {
        let changed = self.lock()?.execute(
            "DELETE FROM queue_items WHERE id = ?1 AND status NOT IN ('extracting', 'analyzing', 'applying')",
            params![id],
        ).map_err(database_error)?;
        if changed == 1 {
            Ok(())
        } else {
            Err(PipelineError::new(
                "STATE_CONFLICT",
                "active or missing item cannot be removed",
            ))
        }
    }

    fn approve_user_edit(
        &self,
        id: i64,
        expected: QueueStatus,
        filename: &str,
        description: &str,
    ) -> PipelineResult<()> {
        let mut record = self
            .load_proposal(id)?
            .ok_or_else(|| PipelineError::new("INVALID_DATA", "proposal is missing"))?;
        record.filename = filename.to_owned();
        record.description = description.trim().to_owned();
        record.status = ProposalStatus::Ready;
        record.reasons.clear();
        record.revision += 1;
        let json = serde_json::to_string(&record)
            .map_err(|_| PipelineError::new("INVALID_DATA", "proposal could not be stored"))?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(database_error)?;
        let changed = transaction.execute(
            "UPDATE queue_items SET status = 'ready', error_code = NULL, updated_at = unixepoch()
             WHERE id = ?1 AND status = ?2",
            params![id, queue_status_text(expected)],
        ).map_err(database_error)?;
        if changed != 1 {
            return Err(PipelineError::new(
                "STATE_CONFLICT",
                "review item changed before approval",
            ));
        }
        let updated = transaction.execute(
            "UPDATE proposals SET proposal_json = ?1, created_at = unixepoch() WHERE queue_item_id = ?2",
            params![json, id],
        ).map_err(database_error)?;
        if updated != 1 {
            return Err(PipelineError::new("INVALID_DATA", "proposal is missing"));
        }
        transaction.commit().map_err(database_error)
    }

    fn lock(&self) -> PipelineResult<std::sync::MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| {
            PipelineError::new(
                "DATABASE_UNAVAILABLE",
                "pipeline database lock is unavailable",
            )
        })
    }
}

fn model_error_code(error: &ModelFailure) -> ErrorCode {
    if error.code == "MODEL_RESPONSE_INVALID" {
        ErrorCode::ModelOutputInvalid
    } else {
        ErrorCode::IoError
    }
}

fn worker_error(error: WorkerFailure) -> PipelineError {
    PipelineError::new(error.code, "parser worker request failed")
}

fn database_error(_: rusqlite::Error) -> PipelineError {
    PipelineError::new("DATABASE_UNAVAILABLE", "pipeline database operation failed")
}

fn queue_status_text(status: QueueStatus) -> &'static str {
    match status {
        QueueStatus::Queued => "queued",
        QueueStatus::Extracting => "extracting",
        QueueStatus::Analyzing => "analyzing",
        QueueStatus::Ready => "ready",
        QueueStatus::NeedsReview => "needs_review",
        QueueStatus::Failed => "failed",
        QueueStatus::Canceled => "canceled",
        QueueStatus::Applying => "applying",
        QueueStatus::Completed => "completed",
    }
}

fn existing_names(directory: &Path) -> Vec<String> {
    fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect()
}

fn validate_leaf_filename(value: &str) -> PipelineResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 512 || Path::new(trimmed).components().count() != 1
        || trimmed.contains('/') || trimmed.contains('\\') || matches!(trimmed, "." | "..")
        || trimmed.ends_with(' ') || trimmed.ends_with('.')
        || trimmed.chars().any(|character| {
            character.is_control()
                || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
                || matches!(character as u32, 0x061c | 0x200e..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
        })
    {
        return Err(PipelineError::new("NAME_INVALID", "filename must be one nonblank path component"));
    }
    Ok(trimmed.to_owned())
}
