use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
};

use intern_core::{
    ErrorCode, FileApplier, InternError, OperationDirection, OperationReceipt, OperationStage,
    QueueItem, QueueStatus, QueueStore, StdFileSystem, source_path_key,
};
use intern_engine::{
    DocumentAnalysis, DocumentSource, ExtractProgress, HouseRule, HouseStyle, ProposalStatus,
    RuleKind, ValidatedProposal, compose_filename,
    evidence::is_valid_iso_date,
    fingerprint::{self, NEAR_DUPLICATE_DISTANCE},
    lesson_from_edit, sanitize_folder_name,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::admission::{AdmissionGuard, AdmissionStage, LocalAdmission};
use crate::settings::{AppSettings, DestinationLayout, SettingsStore};

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

/// How long a request that has already missed its deadline is given to
/// notice its cancel before the queue stops waiting for it.
const MODEL_CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

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
        self.store
            .begin_applying(item.id, QueueStatus::Ready)
            .map_err(|error| match error.code() {
                // The queue is working on another document, so the rename is
                // early rather than wrong. Saying so distinctly is what lets
                // the caller wait instead of blaming the document.
                ErrorCode::StateConflict => PipelineError::new(
                    APPLY_DEFERRED,
                    "another document is being processed; the rename waits for the queue",
                ),
                _ => error.into(),
            })?;
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

/// A document the queue has just filed, as reported to whoever keeps records
/// beside filed documents (the description ledger and the shared filed index,
/// in the desktop app).
#[derive(Clone, Debug, PartialEq)]
pub struct FiledDocument {
    pub item_id: i64,
    /// Where the document was before the rename.
    pub source_path: PathBuf,
    /// The SHA-256 of the document's bytes, lowercase hex - the fingerprint
    /// the rename was verified against.
    pub source_hash: String,
    /// Where it is now - the receipt's destination, suffix and all.
    pub destination: PathBuf,
    /// The sentence that was applied: the model's, or the reviewer's edit.
    pub description: String,
    /// The validated facts behind the name.
    pub proposal: ValidatedProposal,
    /// Unix seconds when the apply completed.
    pub filed_at: i64,
    /// The text fingerprint the analysis carried, for a near-duplicate check
    /// on other machines. Absent for a text too short to fingerprint.
    pub text_fingerprint: Option<String>,
}

/// A filing the queue has just undone: the document is back at
/// `source_path`, and `destination` is empty again.
#[derive(Clone, Debug, PartialEq)]
pub struct UnfiledDocument {
    pub item_id: i64,
    pub source_path: PathBuf,
    pub source_hash: String,
    pub destination: PathBuf,
}

/// Where the queue reports filed documents to.
///
/// Filing has already succeeded when `filed` is called and cannot be undone
/// by it: an implementation that fails records its own failure and says so
/// elsewhere, and the rename stands. `unfiled` is the mirror image, called
/// after an undo has put the document back.
pub trait FilingSink: Send + Sync {
    fn filed(&self, document: &FiledDocument);
    fn unfiled(&self, document: &UnfiledDocument);
}

/// The default: nobody is listening.
struct NoFilingSink;

impl FilingSink for NoFilingSink {
    fn filed(&self, _document: &FiledDocument) {}
    fn unfiled(&self, _document: &UnfiledDocument) {}
}

/// Several listeners behind one sink, told in order. A sink cannot fail, so
/// none of them can keep the others from hearing.
pub struct FilingSinks(pub Vec<Arc<dyn FilingSink>>);

impl FilingSink for FilingSinks {
    fn filed(&self, document: &FiledDocument) {
        for sink in &self.0 {
            sink.filed(document);
        }
    }

    fn unfiled(&self, document: &UnfiledDocument) {
        for sink in &self.0 {
            sink.unfiled(document);
        }
    }
}

/// A filing the queue has no record of: one made by another machine sharing
/// an intake folder, or one this machine made before its history was cleared.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnownFiling {
    /// The name the content was filed under.
    pub filename: String,
    /// The machine that filed it, when it was not this one.
    pub filed_by: Option<String>,
}

impl KnownFiling {
    /// How the queue names the filing to a person: the filename, and the
    /// machine when there is one to name.
    pub fn describe(&self) -> String {
        match &self.filed_by {
            Some(machine) => format!("{} (filed from {machine})", self.filename),
            None => self.filename.clone(),
        }
    }
}

/// Where the queue asks whether a document's content has already been filed
/// somewhere its own history cannot see. Asked once per enqueued document,
/// before any analysis; a positive answer routes the document to review as
/// a duplicate, where "process anyway" is one click.
pub trait DuplicateOracle: Send + Sync {
    fn filed_elsewhere(&self, source_hash: &str, source_path: &Path) -> Option<KnownFiling>;
    /// A filing whose text fingerprint is within [`NEAR_DUPLICATE_DISTANCE`]
    /// of `fingerprint`: the closest one, when there is one.
    fn similar_elsewhere(&self, _fingerprint: u64) -> Option<SimilarFiling> {
        None
    }
}

/// A filing whose text is nearly the text of the document at hand.
#[derive(Clone, Debug, PartialEq)]
pub struct SimilarFiling {
    pub filing: KnownFiling,
    /// How many fingerprint bits apart the two texts are.
    pub distance: u32,
}

/// The default: the queue's own history is all there is.
struct NoDuplicateOracle;

impl DuplicateOracle for NoDuplicateOracle {
    fn filed_elsewhere(&self, _source_hash: &str, _source_path: &Path) -> Option<KnownFiling> {
        None
    }
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
    /// The reviewer's spellings applied when `filename` was composed. The
    /// analysis keeps the document's own words; these say how the name
    /// differs from them, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub house_rules: Vec<HouseRule>,
    /// The name a document with nearly this text was already filed under -
    /// a second scan, a re-export, a copy saved again - when there is one.
    /// Such a document waits for a person rather than being filed twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub near_duplicate_of: Option<String>,
    /// Whether a person has approved this name. An approval the queue was too
    /// busy to act on at once waits here, so the scheduler files the document
    /// when it is free even with automatic renaming switched off.
    #[serde(default)]
    pub approved: bool,
}

/// The review reason for a document whose text is nearly the text of one
/// already filed. The record's `near_duplicate_of` names that filing.
pub const NEAR_DUPLICATE: &str = "NEAR_DUPLICATE";

/// The review reason for a rename a person took back. The document is where
/// it started and waits for a decision; nothing files it again on its own.
pub const UNDONE: &str = "UNDONE";

impl ProposalRecord {
    /// The validated facts as the name carries them: the document's words,
    /// respelled the way the reviewer has taught Intern to.
    pub fn styled_proposal(&self) -> ValidatedProposal {
        HouseStyle::new(self.house_rules.clone())
            .apply(&self.analysis.proposal)
            .0
    }
}

/// How many times the same respelling must be made in review before Intern
/// applies it on its own. One edit is a decision about one document; the
/// second is a preference.
pub const EDITS_TO_LEARN: u32 = 2;

/// A spelling Intern has learned from review, and how settled it is.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LearnedRule {
    pub id: i64,
    pub kind: RuleKind,
    pub from: String,
    pub to: String,
    /// How many times a reviewer has made exactly this change.
    pub seen: u32,
    /// Whether Intern applies it: made often enough, or told to use it now.
    pub active: bool,
    /// Unix seconds of the latest edit that taught it.
    pub learned_at: i64,
}

impl LearnedRule {
    pub fn rule(&self) -> HouseRule {
        HouseRule::new(self.kind, self.from.clone(), self.to.clone())
    }
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
    /// For an item flagged DUPLICATE: the name its content is already filed
    /// under (the completed apply's destination leaf, or the completed item's
    /// original filename for keep-original completions). `None` once the
    /// completed row is gone, e.g. after the history was cleared.
    pub duplicate_of: Option<String>,
}

pub struct Pipeline {
    store: Arc<QueueStore>,
    repository: PipelineRepository,
    worker: Arc<dyn WorkerBoundary>,
    model: Arc<dyn AnalyzerBoundary>,
    files: Arc<dyn FileActions>,
    events: Arc<dyn PipelineEventSink>,
    filing: Arc<dyn FilingSink>,
    duplicates: Arc<dyn DuplicateOracle>,
    admission: Arc<dyn AdmissionGuard>,
    settings: SettingsStore,
    paused: AtomicBool,
    active_item: AtomicI64,
    shutting_down: AtomicBool,
    model_timeout: std::time::Duration,
    lease_renewal_interval: std::time::Duration,
    run_lock: Mutex<()>,
}

/// Which waiting renames one scheduler pass applies.
#[derive(Clone, Copy, Eq, PartialEq)]
enum ReadyScope {
    /// Everything owed: every ready document when automatic renaming is on,
    /// and every approval still waiting. What opens a drain.
    Everything,
    /// Only the approvals the queue was too busy to act on when they were
    /// made. What runs between documents.
    ApprovalsOnly,
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
            filing: Arc::new(NoFilingSink),
            duplicates: Arc::new(NoDuplicateOracle),
            admission: Arc::new(LocalAdmission),
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
            filing: Arc::new(NoFilingSink),
            duplicates: Arc::new(NoDuplicateOracle),
            admission: Arc::new(LocalAdmission),
            settings,
            paused: AtomicBool::new(false),
            active_item: AtomicI64::new(0),
            shutting_down: AtomicBool::new(false),
            model_timeout: std::time::Duration::from_secs(MODEL_TIMEOUT_SECONDS),
            lease_renewal_interval: LEASE_RENEWAL_INTERVAL,
            run_lock: Mutex::new(()),
        })
    }

    /// Reports every completed rename (and every undo of one) to `sink`.
    #[must_use]
    pub fn with_filing_sink(mut self, sink: Arc<dyn FilingSink>) -> Self {
        self.filing = sink;
        self
    }

    /// Asks `oracle` about every enqueued document whose content the queue's
    /// own history has not already filed.
    #[must_use]
    pub fn with_duplicate_oracle(mut self, oracle: Arc<dyn DuplicateOracle>) -> Self {
        self.duplicates = oracle;
        self
    }

    #[must_use]
    pub fn with_admission_guard(mut self, guard: Arc<dyn AdmissionGuard>) -> Self {
        self.admission = guard;
        self
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
            let verified = self.admission.authorize(path, AdmissionStage::Enqueue)?;
            let fingerprint = self.files.fingerprint(path)?;
            if verified.as_ref().is_some_and(|hash| hash != &fingerprint) {
                return Err(PipelineError::new(
                    "FILE_CHANGED",
                    "The file changed after Microsoft verified its uploader.",
                ));
            }
            let mut item = self.store.enqueue(path, &fingerprint)?;
            if item.status == QueueStatus::Queued {
                item = self.flag_if_completed_duplicate(item)?;
            }
            if item.status == QueueStatus::Queued {
                item = self.flag_if_filed_elsewhere(item)?;
            }
            queued.push(item);
        }
        if !queued.is_empty() {
            self.events.queue_changed();
        }
        Ok(queued)
    }

    /// Flags a just-queued item whose content is already filed as completed.
    ///
    /// Enqueue holds no run lock, so the flag is a compare-and-swap on the
    /// Queued status: if the scheduler claimed the item between the lookup and
    /// the transition, the claim wins and the item analyzes normally.
    fn flag_if_completed_duplicate(&self, item: QueueItem) -> PipelineResult<QueueItem> {
        let duplicate = self
            .store
            .find_completed_duplicate(&item.source_hash, &source_path_key(&item.source_path))?;
        if duplicate.is_none() {
            return Ok(item);
        }
        self.flag_duplicate(item)
    }

    /// Flags a just-queued item whose content the duplicate oracle knows to be
    /// filed already - by a teammate, typically. Same compare-and-swap as the
    /// local check.
    fn flag_if_filed_elsewhere(&self, item: QueueItem) -> PipelineResult<QueueItem> {
        if self
            .duplicates
            .filed_elsewhere(&item.source_hash, &item.source_path)
            .is_none()
        {
            return Ok(item);
        }
        self.flag_duplicate(item)
    }

    fn flag_duplicate(&self, item: QueueItem) -> PipelineResult<QueueItem> {
        match self.store.transition(
            item.id,
            QueueStatus::Queued,
            QueueStatus::NeedsReview,
            Some(ErrorCode::Duplicate),
        ) {
            Ok(flagged) => Ok(flagged),
            Err(error) if error.code() == ErrorCode::StateConflict => Ok(item),
            Err(error) => Err(error.into()),
        }
    }

    pub fn list(&self) -> PipelineResult<Vec<PipelineItem>> {
        self.store
            .list()?
            .into_iter()
            .map(|item| self.pipeline_item(item))
            .collect()
    }

    /// The newest item enqueued from `path` - as given, or as it
    /// canonicalizes now - with its proposal and receipt, or `None` when the
    /// queue has never seen the path.
    ///
    /// A path that no longer canonicalizes (the apply already renamed it away)
    /// still matches as given, which is what lets a finished intake document
    /// report its fate instead of `Unknown`.
    pub fn find_by_source_path(&self, path: &Path) -> PipelineResult<Option<PipelineItem>> {
        let canonical = fs::canonicalize(path).ok();
        let mut candidates = vec![path];
        if let Some(canonical) = canonical.as_deref()
            && canonical != path
        {
            candidates.push(canonical);
        }
        self.store
            .find_newest_by_source_path(&candidates)?
            .map(|item| self.pipeline_item(item))
            .transpose()
    }

    fn pipeline_item(&self, item: QueueItem) -> PipelineResult<PipelineItem> {
        let proposal = self.repository.load_proposal(item.id)?;
        let receipt = self.store.load_receipt(item.id)?;
        let duplicate_of = if item.status == QueueStatus::NeedsReview
            && item.error_code == Some(ErrorCode::Duplicate)
        {
            self.store
                .find_completed_duplicate(&item.source_hash, &source_path_key(&item.source_path))?
                .map(|duplicate| {
                    duplicate.filed_as.unwrap_or_else(|| {
                        duplicate
                            .source_path
                            .file_name()
                            .unwrap_or(duplicate.source_path.as_os_str())
                            .to_string_lossy()
                            .into_owned()
                    })
                })
                .or_else(|| {
                    self.duplicates
                        .filed_elsewhere(&item.source_hash, &item.source_path)
                        .map(|known| known.describe())
                })
        } else {
            None
        };
        Ok(PipelineItem {
            id: item.id,
            source_path: item.source_path,
            source_hash: item.source_hash,
            status: item.status,
            processing_failures: item.processing_failures,
            error_code: item.error_code,
            proposal,
            receipt,
            duplicate_of,
        })
    }

    /// Every document the queue has filed and not undone: completed items
    /// whose latest receipt is a finished apply, with the sentence and facts
    /// that were applied. What a records keeper replays when it is switched
    /// on after documents were already filed.
    pub fn filed_documents(&self) -> PipelineResult<Vec<FiledDocument>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|item| item.status == QueueStatus::Completed)
            .filter_map(|item| {
                let receipt = item.receipt?;
                let proposal = item.proposal?;
                filed_document(
                    item.id,
                    &item.source_hash,
                    &receipt,
                    &proposal,
                    receipt_time(&receipt),
                )
            })
            .collect())
    }

    pub fn run_until_idle(&self) -> PipelineResult<()> {
        let _run = self
            .run_lock
            .lock()
            .map_err(|_| PipelineError::new("STATE_CONFLICT", "pipeline lock is unavailable"))?;
        self.apply_pending_ready(ReadyScope::Everything)?;
        while !self.paused.load(Ordering::SeqCst) {
            if !self.run_next_inner()? {
                break;
            }
            // Between documents, not only before the first: an approval made
            // while the queue was working could not be applied then, and this
            // is the next moment the store will let it through.
            self.apply_pending_ready(ReadyScope::ApprovalsOnly)?;
        }
        Ok(())
    }

    pub fn run_next(&self) -> PipelineResult<()> {
        let _run = self
            .run_lock
            .lock()
            .map_err(|_| PipelineError::new("STATE_CONFLICT", "pipeline lock is unavailable"))?;
        if !self.paused.load(Ordering::SeqCst) {
            self.apply_pending_ready(ReadyScope::Everything)?;
            let _ = self.run_next_inner()?;
        }
        Ok(())
    }

    fn run_next_inner(&self) -> PipelineResult<bool> {
        let Some(item) = self.store.claim_next()? else {
            return Ok(false);
        };
        if self.authorize_item(&item, AdmissionStage::Extract).is_err() {
            self.store.transition(
                item.id,
                QueueStatus::Extracting,
                QueueStatus::NeedsReview,
                Some(ErrorCode::UploaderUnverified),
            )?;
            self.events.queue_changed();
            return Ok(true);
        }
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
                    // A worker that will not come back cannot read this
                    // document on a second attempt either, so the failure is
                    // counted twice and the document fails now rather than
                    // stalling the queue again. Counted against this item by
                    // id: reclaiming through the queue would claim whichever
                    // document is next in line, which is not always this one,
                    // and leave that one extracting under nobody's lease.
                    if restart_failed {
                        self.repository.record_recovered_failure(item.id)?;
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
        if self.authorize_item(&item, AdmissionStage::Analyze).is_err() {
            lease.stop_and_check()?;
            self.store.transition(
                item.id,
                QueueStatus::Analyzing,
                QueueStatus::NeedsReview,
                Some(ErrorCode::UploaderUnverified),
            )?;
            self.active_item.store(0, Ordering::SeqCst);
            self.events.queue_changed();
            return Ok(true);
        }
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
                // Failures that would repeat for every document - a model
                // that cannot be reached, a key that was refused - pause the
                // queue rather than fail the backlog one item at a time.
                if matches!(
                    error.code.as_str(),
                    "MODEL_CANCEL_FAILED"
                        | "MODEL_RECOVERY_FAILED"
                        | "MODEL_REQUEST_FAILED"
                        | "MODEL_RESPONSE_INVALID"
                        | "HOSTED_MODEL_MISCONFIGURED"
                        | "HOSTED_MODEL_UNAUTHORIZED"
                        | "HOSTED_MODEL_UNREACHABLE"
                        | "HOSTED_MODEL_RATE_LIMITED"
                ) {
                    self.paused.store(true, Ordering::SeqCst);
                }
                self.events.queue_changed();
                return Ok(true);
            }
        };
        self.ensure_lease(&lease)?;
        // The document's words, respelled the way review has taught Intern
        // to. Applied here, after validation, so the evidence stayed the
        // document's and only the name is the reviewer's.
        let (styled, house_rules) = self.repository.active_style()?.apply(&analysis.proposal);
        let filename = self.compose_for_target(&item.source_path, &styled, &extension, &existing);
        // The exact-bytes check ran before analysis. This one needs the text
        // and the date, so it runs after: a second scan, a re-export, or a
        // copy saved again with new metadata says what a filed document
        // says, and is not filed on its own.
        let near_duplicate_of = self.near_duplicate_of(item.id, &analysis);
        let mut reasons = analysis
            .review_reasons
            .iter()
            .map(|reason| reason.as_str().to_owned())
            .collect::<Vec<_>>();
        let mut status = analysis.status;
        if near_duplicate_of.is_some() {
            reasons.push(NEAR_DUPLICATE.to_owned());
            status = ProposalStatus::NeedsReview;
        }
        let record = ProposalRecord {
            status,
            filename,
            description: analysis.description.clone(),
            reasons,
            analysis,
            revision: 1,
            house_rules,
            near_duplicate_of,
            approved: false,
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
        self.admission
            .processed(&item.source_path, &item.source_hash);
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

    /// Applies the renames the scheduler owes: every ready document when
    /// automatic renaming is on, and every document a person approved while
    /// the queue was busy elsewhere, whether it is on or not.
    fn apply_pending_ready(&self, scope: ReadyScope) -> PipelineResult<()> {
        if self.paused.load(Ordering::SeqCst) {
            return Ok(());
        }
        let ready = self
            .store
            .list()?
            .into_iter()
            .filter(|item| item.status == QueueStatus::Ready)
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return Ok(());
        }
        let settings = self.settings.load()?;
        let automatic = settings.automatic_rename && scope == ReadyScope::Everything;
        for item in ready {
            if self.paused.load(Ordering::SeqCst) {
                break;
            }
            let Some(proposal) = self.repository.load_proposal(item.id)? else {
                if automatic {
                    self.repository
                        .mark_needs_review(item.id, "PROPOSAL_MISSING")?;
                }
                continue;
            };
            if !automatic && !proposal.approved {
                continue;
            }
            let _ = self.apply_if_unchanged(&item, &proposal.filename, &settings);
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

    /// The filing whose text this analysis nearly repeats, if any: first the
    /// queue's own history, then whatever the duplicate oracle knows from
    /// other machines.
    fn near_duplicate_of(&self, item_id: i64, analysis: &DocumentAnalysis) -> Option<String> {
        let fingerprint = fingerprint::decode(analysis.text_fingerprint.as_deref()?)?;
        let date = analysis.proposal.document_date.clone().or_else(|| {
            analysis
                .model_proposal
                .as_ref()
                .and_then(|reply| reply.document_date.clone())
        });
        let local = self
            .repository
            .find_similar(fingerprint, item_id)
            .unwrap_or_default()
            .into_iter()
            .find(|similar| {
                same_document(similar.distance, &similar.filing.filename, date.as_deref())
            })
            .map(|similar| similar.filing.describe());
        local.or_else(|| {
            self.duplicates
                .similar_elsewhere(fingerprint)
                .filter(|similar| {
                    same_document(similar.distance, &similar.filing.filename, date.as_deref())
                })
                .map(|similar| similar.filing.describe())
        })
    }

    /// The name a proposal will be applied under. The engine composed its
    /// name against the source folder, which is the only folder it knows;
    /// the name that is actually applied must not collide in the folder the
    /// document is going to. Without readable settings the source folder's
    /// names (`fallback`) stand in.
    fn compose_for_target(
        &self,
        source_path: &Path,
        proposal: &ValidatedProposal,
        extension: &str,
        fallback: &[String],
    ) -> String {
        let named = compose_filename(proposal, extension, &[]).value;
        let existing = match self.settings.load() {
            Ok(settings) => existing_names(&target_folder(
                &settings,
                source_path,
                &proposal_as_applied(proposal, &named),
            )),
            Err(_) => fallback.to_vec(),
        };
        compose_filename(
            proposal,
            extension,
            &existing.iter().map(String::as_str).collect::<Vec<_>>(),
        )
        .value
    }

    /// The spellings review has taught Intern, newest first.
    pub fn learned_rules(&self) -> PipelineResult<Vec<LearnedRule>> {
        self.repository.list_rules()
    }

    /// Stop applying a learned spelling. Names already applied keep it;
    /// documents still waiting go back to the document's own words.
    pub fn forget_rule(&self, id: i64) -> PipelineResult<()> {
        if !self.repository.forget_rule(id)? {
            return Err(PipelineError::new(
                "RULE_NOT_FOUND",
                "learned spelling does not exist",
            ));
        }
        self.restyle_waiting(None)
    }

    /// Apply a learned spelling from now on without waiting for a second
    /// edit, including to documents still waiting.
    pub fn use_rule(&self, id: i64) -> PipelineResult<()> {
        if !self.repository.use_rule(id)? {
            return Err(PipelineError::new(
                "RULE_NOT_FOUND",
                "learned spelling does not exist",
            ));
        }
        self.restyle_waiting(None)
    }

    /// Recomposes the proposed name of every document still waiting under
    /// the spellings now in force, so a rule that just changed shows in the
    /// queue at once rather than only on the next document.
    ///
    /// `approved` names the document whose name a person has just typed, if
    /// any. That name is theirs and is never recomposed: composing it again
    /// from the validated facts would throw away everything the facts do not
    /// carry, the date they typed in most of all.
    fn restyle_waiting(&self, approved: Option<i64>) -> PipelineResult<()> {
        let style = self.repository.active_style()?;
        let mut changed = false;
        for item in self.store.list()? {
            if !matches!(item.status, QueueStatus::NeedsReview | QueueStatus::Ready)
                || approved == Some(item.id)
            {
                continue;
            }
            let Some(mut record) = self.repository.load_proposal(item.id)? else {
                continue;
            };
            let (styled, house_rules) = style.apply(&record.analysis.proposal);
            if house_rules == record.house_rules {
                continue;
            }
            let extension = item
                .source_path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_owned();
            let existing =
                existing_names(item.source_path.parent().unwrap_or_else(|| Path::new(".")));
            record.filename =
                self.compose_for_target(&item.source_path, &styled, &extension, &existing);
            record.house_rules = house_rules;
            record.revision += 1;
            self.repository.replace_proposal(item.id, &record)?;
            changed = true;
        }
        if changed {
            self.events.queue_changed();
        }
        Ok(())
    }

    /// What an approved edit teaches, if anything: a respelled party or
    /// type, remembered, and applied on its own once the same change has
    /// been made twice. A spelling Intern itself applied that the reviewer
    /// changed again is a change of mind about the document's word, not
    /// about Intern's; restoring the document's own spelling retracts the
    /// rule.
    fn learn_from_edit(
        &self,
        id: i64,
        record: &ProposalRecord,
        extension: &str,
        approved: &str,
    ) -> PipelineResult<()> {
        let Some(lesson) = lesson_from_edit(
            &record.styled_proposal(),
            extension,
            &record.filename,
            approved,
        ) else {
            return Ok(());
        };
        let lesson = match record.house_rules.iter().find(|applied| {
            applied.kind == lesson.kind && HouseRule::key(&applied.to) == lesson.from_key()
        }) {
            Some(applied) => HouseRule::new(lesson.kind, applied.from.clone(), lesson.to),
            None => lesson,
        };
        if HouseRule::key(&lesson.to) == lesson.from_key() {
            self.repository.forget_rule_for(lesson.kind, &lesson.from)?;
        } else if lesson.is_meaningful() {
            self.repository.learn(&lesson)?;
        }
        self.restyle_waiting(Some(id))
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
                // A request that has already missed its deadline is given a
                // little longer to notice the cancel, and then left to finish
                // on its own. Waiting on it without a deadline meant one model
                // that honoured neither its deadline nor its cancel stopped
                // the queue for as long as the process lived.
                if receiver
                    .recv_timeout(self.model_timeout.min(MODEL_CANCEL_GRACE))
                    .is_ok()
                {
                    let _ = join.join();
                }
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

    fn authorize_item(&self, item: &QueueItem, stage: AdmissionStage) -> PipelineResult<()> {
        let verified = self.admission.authorize(&item.source_path, stage)?;
        if verified
            .as_ref()
            .is_some_and(|hash| hash != &item.source_hash)
        {
            return Err(PipelineError::new(
                "UPLOADER_UNVERIFIED",
                "The current file is not the version whose uploader was verified. Retry after verification.",
            ));
        }
        Ok(())
    }

    fn apply_if_unchanged(
        &self,
        item: &QueueItem,
        filename: &str,
        settings: &AppSettings,
    ) -> PipelineResult<()> {
        if let Err(error) = self.authorize_item(item, AdmissionStage::Apply) {
            let _ = self
                .repository
                .mark_needs_review(item.id, "UPLOADER_UNVERIFIED");
            self.events.queue_changed();
            return Err(error);
        }
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
        let proposal = self.repository.load_proposal(item.id)?;
        // The name to apply is the one the record holds now, not the one the
        // caller read a moment ago. Nothing holds the queue still between a
        // scheduler pass deciding what to file and the file operation itself,
        // and an edit approved in that moment is the name the person expects
        // to see on the document.
        let filename = proposal
            .as_ref()
            .map_or(filename, |record| record.filename.as_str());
        if leading_date(filename).is_none() {
            self.repository.mark_needs_review(item.id, DATE_REQUIRED)?;
            self.events.queue_changed();
            return Ok(());
        }
        let target = match proposal.as_ref() {
            Some(record) => target_folder(
                settings,
                &item.source_path,
                &proposal_as_applied(&record.styled_proposal(), filename),
            ),
            None => destination_root(settings, &item.source_path),
        };
        // A layout subfolder exists only once a document is filed into it; a
        // folder that cannot be created is the same failure a missing
        // destination would be, and is reported the same way.
        if let Err(error) = fs::create_dir_all(&target) {
            let failure = PipelineError::new(
                "DESTINATION_UNAVAILABLE",
                format!("the destination folder could not be created ({error})"),
            );
            self.repository.mark_needs_review(item.id, &failure.code)?;
            self.events.queue_changed();
            return Err(failure);
        }
        if let Err(error) = self.files.apply(item, &target.join(filename)) {
            if error.code == APPLY_DEFERRED {
                // Nothing is wrong with this document: the queue was busy with
                // another one. The name a person approved is durable in the
                // proposal, so the item stays ready and the scheduler applies
                // it between documents rather than sending it to review.
                self.events.queue_changed();
                return Ok(());
            }
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
            } else {
                self.report_settled(item.id);
            }
            self.events.queue_changed();
            return Err(error);
        }
        self.report_filed(item);
        self.events.queue_changed();
        Ok(())
    }

    /// Tells the filing sink about a rename that just completed. Read back
    /// from the store rather than assumed: the receipt carries the destination
    /// the applier actually chose, suffix and all, and the proposal carries
    /// the sentence a reviewer may have edited.
    fn report_filed(&self, item: &QueueItem) {
        let Ok(Some(receipt)) = self.store.load_receipt(item.id) else {
            return;
        };
        let Ok(Some(proposal)) = self.repository.load_proposal(item.id) else {
            return;
        };
        if let Some(document) =
            filed_document(item.id, &item.source_hash, &receipt, &proposal, unix_now())
        {
            if let Some(fingerprint) = document
                .text_fingerprint
                .as_deref()
                .and_then(fingerprint::decode)
            {
                let filed_name = document
                    .destination
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let _ = self
                    .repository
                    .remember_fingerprint(item.id, fingerprint, &filed_name);
            }
            self.filing.filed(&document);
        }
    }

    /// Reports an operation a reconciliation finished rather than the call
    /// that started it.
    ///
    /// The applier journals an ambiguous apply or undo and settles it
    /// afterwards - on the next retry, on the next recovery pass, or right
    /// here - and nobody used to be told: a document filed that way was never
    /// described and never remembered as a filing, so a second scan of it was
    /// filed all over again, and a document put back that way was still
    /// remembered as filed. What is reported is read from the store, so it is
    /// the same work whichever call finished the operation.
    fn report_settled(&self, item_id: i64) {
        let Ok(items) = self.store.list() else {
            return;
        };
        let Some(item) = items.into_iter().find(|candidate| candidate.id == item_id) else {
            return;
        };
        let Ok(Some(receipt)) = self.store.load_receipt(item_id) else {
            return;
        };
        if receipt.stage != OperationStage::Complete {
            return;
        }
        match receipt.direction {
            OperationDirection::Apply if item.status == QueueStatus::Completed => {
                self.report_filed(&item);
            }
            OperationDirection::Undo if item.status != QueueStatus::Completed => {
                // An undo returns the item to ready, which is the state the
                // scheduler files from: with automatic renaming on it would
                // apply the same name again within the minute and undo the
                // person's undo. Taking a decision back is a decision, so the
                // document waits for the next one.
                let _ = self.repository.mark_needs_review(item_id, UNDONE);
                let _ = self.repository.forget_fingerprint(item_id);
                // An undo receipt reads the other way round: it moved the
                // document from the name it was filed under back to where it
                // started, and the vacated name is what a records keeper knows
                // it by.
                self.filing.unfiled(&UnfiledDocument {
                    item_id,
                    source_path: item.source_path.clone(),
                    source_hash: item.source_hash.clone(),
                    destination: receipt.source.clone(),
                });
                if let Ok(settings) = self.settings.load() {
                    prune_empty_layout_folders(
                        &destination_root(&settings, &item.source_path),
                        &receipt.source,
                        settings.destination_layout,
                    );
                }
            }
            _ => {}
        }
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
            self.report_settled(id);
            self.events.queue_changed();
            return result;
        }
        if item.status == QueueStatus::NeedsReview
            && item.error_code == Some(ErrorCode::UploaderUnverified)
        {
            // The denial happened before any analysis, so there is no
            // proposal to review and nothing for a person to approve: the
            // only useful thing Retry can mean is "the file is verified now,
            // read it". Same compare-and-swap as the duplicate shortcut.
            self.repository.retry_unverified(id)?;
            self.events.queue_changed();
            return Ok(());
        }
        if item.status == QueueStatus::NeedsReview && item.error_code == Some(ErrorCode::Duplicate)
        {
            // "Process anyway": the duplicate flag was set before any
            // analysis, so clearing it simply returns the item to the queue
            // for a normal run.
            self.store.retry_duplicate(id)?;
            self.events.queue_changed();
            return Ok(());
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
        if leading_date(&filename).is_none() {
            return Err(PipelineError::new(
                DATE_REQUIRED,
                "the filename must start with the document's date as YYYY-MM-DD",
            ));
        }
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
        if let Err(error) = self.authorize_item(&item, AdmissionStage::Apply) {
            let _ = self
                .repository
                .mark_needs_review(item.id, "UPLOADER_UNVERIFIED");
            self.events.queue_changed();
            return Err(error);
        }
        let proposed = self.repository.load_proposal(id)?;
        self.repository
            .approve_user_edit(id, item.status, &filename, description)?;
        // A preference store, not a filing step: a lesson that cannot be
        // written must not stop the rename that was just approved.
        if let Some(record) = proposed.as_ref() {
            let _ = self.learn_from_edit(id, record, source_extension, &filename);
        }
        let ready = self
            .store
            .list()?
            .into_iter()
            .find(|candidate| candidate.id == id)
            .ok_or_else(|| {
                PipelineError::new("ITEM_NOT_FOUND", "queue item disappeared during approval")
            })?;
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
        // An undo the applier journalled can be finished by a reconciliation
        // even when the call itself reports a failure, so what settles this is
        // the store rather than the return value.
        let outcome = self.files.undo(&item, &receipt);
        self.report_settled(id);
        self.events.queue_changed();
        outcome
    }

    pub fn clear_history(&self) -> PipelineResult<usize> {
        let removed = self.store.clear_terminal()?;
        self.events.queue_changed();
        Ok(removed)
    }

    /// Abandons everything still waiting, for the folder that was chosen by
    /// mistake. Renames already applied keep their receipts, and anything
    /// awaiting a human decision stays put.
    pub fn discard_waiting(&self) -> PipelineResult<usize> {
        let removed = self.store.discard_queued()?;
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
            if self.files.reconcile(&item).is_err()
                && let Ok(claimed) = self.store.claim_applying_reconciliation(item.id)
            {
                let _ = self.files.reconcile(&claimed);
            }
            self.report_settled(item.id);
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
            .execute_batch(
                "PRAGMA foreign_keys=ON;
                 CREATE TABLE IF NOT EXISTS house_rules (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   kind TEXT NOT NULL,
                   from_key TEXT NOT NULL,
                   from_value TEXT NOT NULL,
                   to_value TEXT NOT NULL,
                   seen INTEGER NOT NULL DEFAULT 1,
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL,
                   UNIQUE(kind, from_key)
                 );
                 CREATE TABLE IF NOT EXISTS fingerprints (
                   queue_item_id INTEGER PRIMARY KEY REFERENCES queue_items(id) ON DELETE CASCADE,
                   fingerprint INTEGER NOT NULL,
                   filed_name TEXT NOT NULL,
                   filed_at INTEGER NOT NULL
                 );",
            )
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

    /// Stores a record over the one a queue item already has.
    fn replace_proposal(&self, id: i64, record: &ProposalRecord) -> PipelineResult<()> {
        let json = serde_json::to_string(record)
            .map_err(|_| PipelineError::new("INVALID_DATA", "proposal could not be stored"))?;
        let updated = self
            .lock()?
            .execute(
                "UPDATE proposals SET proposal_json = ?1 WHERE queue_item_id = ?2",
                params![json, id],
            )
            .map_err(database_error)?;
        if updated != 1 {
            return Err(PipelineError::new("INVALID_DATA", "proposal is missing"));
        }
        Ok(())
    }

    fn list_rules(&self) -> PipelineResult<Vec<LearnedRule>> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT id, kind, from_value, to_value, seen, updated_at FROM house_rules
                 ORDER BY updated_at DESC, id DESC",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(database_error)?;
        let mut rules = Vec::new();
        for row in rows {
            let (id, kind, from, to, seen, learned_at) = row.map_err(database_error)?;
            let Some(kind) = RuleKind::parse(&kind) else {
                continue;
            };
            rules.push(LearnedRule {
                id,
                kind,
                from,
                to,
                seen,
                active: seen >= EDITS_TO_LEARN,
                learned_at,
            });
        }
        Ok(rules)
    }

    /// The rules in force: learned often enough, or told to be used.
    fn active_style(&self) -> PipelineResult<HouseStyle> {
        Ok(HouseStyle::new(
            self.list_rules()?
                .into_iter()
                .filter(|rule| rule.active)
                .map(|rule| rule.rule())
                .collect(),
        ))
    }

    /// Records one respelling. The same change again counts it up; a
    /// different spelling for the same word starts the count over.
    fn learn(&self, rule: &HouseRule) -> PipelineResult<LearnedRule> {
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO house_rules(kind, from_key, from_value, to_value, seen, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 1, unixepoch(), unixepoch())
                 ON CONFLICT(kind, from_key) DO UPDATE SET
                   seen = CASE WHEN to_value = excluded.to_value THEN seen + 1 ELSE 1 END,
                   to_value = excluded.to_value,
                   from_value = excluded.from_value,
                   updated_at = unixepoch()",
                params![rule.kind.as_str(), rule.from_key(), rule.from, rule.to],
            )
            .map_err(database_error)?;
        connection
            .query_row(
                "SELECT id, from_value, to_value, seen, updated_at FROM house_rules
                 WHERE kind = ?1 AND from_key = ?2",
                params![rule.kind.as_str(), rule.from_key()],
                |row| {
                    Ok(LearnedRule {
                        id: row.get(0)?,
                        kind: rule.kind,
                        from: row.get(1)?,
                        to: row.get(2)?,
                        seen: row.get(3)?,
                        active: row.get::<_, u32>(3)? >= EDITS_TO_LEARN,
                        learned_at: row.get(4)?,
                    })
                },
            )
            .map_err(database_error)
    }

    /// Keeps the text fingerprint of a document just filed, under the name
    /// it was filed as, so a later document saying the same thing can be
    /// told so. Cleared with the item when the history is cleared.
    fn remember_fingerprint(
        &self,
        item_id: i64,
        fingerprint: u64,
        filed_name: &str,
    ) -> PipelineResult<()> {
        self.lock()?
            .execute(
                "INSERT INTO fingerprints(queue_item_id, fingerprint, filed_name, filed_at)
                 VALUES (?1, ?2, ?3, unixepoch())
                 ON CONFLICT(queue_item_id) DO UPDATE SET
                   fingerprint = excluded.fingerprint,
                   filed_name = excluded.filed_name,
                   filed_at = excluded.filed_at",
                params![item_id, fingerprint as i64, filed_name],
            )
            .map_err(database_error)?;
        Ok(())
    }

    fn forget_fingerprint(&self, item_id: i64) -> PipelineResult<()> {
        self.lock()?
            .execute(
                "DELETE FROM fingerprints WHERE queue_item_id = ?1",
                params![item_id],
            )
            .map_err(database_error)?;
        Ok(())
    }

    /// The filings within the near-duplicate distance of `fingerprint`,
    /// closest first, other than `except_item`'s own.
    ///
    /// All of them, not only the closest. Last year's renewal of an agreement
    /// can be nearer in text than this year's second scan of it is, and
    /// answering with that one alone hid the filing the document really
    /// repeats behind a date that says "another document".
    fn find_similar(
        &self,
        fingerprint: u64,
        except_item: i64,
    ) -> PipelineResult<Vec<SimilarFiling>> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT fingerprint, filed_name FROM fingerprints WHERE queue_item_id <> ?1")
            .map_err(database_error)?;
        let rows = statement
            .query_map(params![except_item], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
            })
            .map_err(database_error)?;
        let mut similar = Vec::new();
        for row in rows {
            let (stored, filed_name) = row.map_err(database_error)?;
            let distance = fingerprint::hamming(fingerprint, stored);
            if distance <= NEAR_DUPLICATE_DISTANCE {
                similar.push(SimilarFiling {
                    filing: KnownFiling {
                        filename: filed_name,
                        filed_by: None,
                    },
                    distance,
                });
            }
        }
        similar.sort_by_key(|candidate| candidate.distance);
        Ok(similar)
    }

    fn forget_rule_for(&self, kind: RuleKind, from: &str) -> PipelineResult<()> {
        self.lock()?
            .execute(
                "DELETE FROM house_rules WHERE kind = ?1 AND from_key = ?2",
                params![kind.as_str(), HouseRule::key(from)],
            )
            .map_err(database_error)?;
        Ok(())
    }

    fn forget_rule(&self, id: i64) -> PipelineResult<bool> {
        let removed = self
            .lock()?
            .execute("DELETE FROM house_rules WHERE id = ?1", params![id])
            .map_err(database_error)?;
        Ok(removed == 1)
    }

    fn use_rule(&self, id: i64) -> PipelineResult<bool> {
        let changed = self
            .lock()?
            .execute(
                "UPDATE house_rules SET seen = MAX(seen, ?2), updated_at = unixepoch() WHERE id = ?1",
                params![id, EDITS_TO_LEARN],
            )
            .map_err(database_error)?;
        Ok(changed == 1)
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
        // A document waiting for a person is no longer a document waiting to
        // be filed, whatever was approved before.
        record.approved = false;
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

    /// Returns a document denied at admission to the queue, for the person
    /// who has since had its uploader verified. The compare-and-swap covers
    /// the error code as well as the status, so only that denial takes the
    /// shortcut and a concurrent decision on the item makes it fail closed.
    fn retry_unverified(&self, id: i64) -> PipelineResult<()> {
        let changed = self.lock()?.execute(
            "UPDATE queue_items SET status = 'queued', processing_failures = 0, error_code = NULL,
             owner_session = NULL, lease_expires_at = NULL, updated_at = unixepoch()
             WHERE id = ?1 AND status = 'needs_review' AND error_code = 'UPLOADER_UNVERIFIED'",
            params![id],
        ).map_err(database_error)?;
        if changed == 1 {
            Ok(())
        } else {
            Err(PipelineError::new(
                "STATE_CONFLICT",
                "unverified retry compare-and-swap failed",
            ))
        }
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

    /// Counts one processing failure against an item that is back in the
    /// queue and owned by nobody - interrupted by a crash, or given up on
    /// because the worker that was reading it cannot be restarted - and
    /// fails it once it has failed twice.
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
        record.approved = true;
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

/// The folder a document is filed into: the destination (or, with none set,
/// the document's own folder) plus the layout's subfolder for its facts.
pub fn target_folder(
    settings: &AppSettings,
    source_path: &Path,
    proposal: &ValidatedProposal,
) -> PathBuf {
    let root = destination_root(settings, source_path);
    match layout_subfolder(settings.destination_layout, proposal) {
        Some(subfolder) => root.join(subfolder),
        None => root,
    }
}

fn destination_root(settings: &AppSettings, source_path: &Path) -> PathBuf {
    if settings.destination.trim().is_empty() {
        source_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    } else {
        PathBuf::from(settings.destination.trim())
    }
}

/// The subfolder a layout puts a document in, relative to the destination.
///
/// A document missing the fact a layout keys on goes in a named catch-all
/// ("Undated", "Unsorted") rather than the root, so the root stays a set of
/// folders and a person can see what still needs a hand.
pub fn layout_subfolder(
    layout: DestinationLayout,
    proposal: &ValidatedProposal,
) -> Option<PathBuf> {
    let year = || {
        proposal
            .document_date
            .as_deref()
            .and_then(|date| date.get(..4))
            .filter(|year| year.bytes().all(|byte| byte.is_ascii_digit()))
            .map(str::to_owned)
            .unwrap_or_else(|| "Undated".to_owned())
    };
    let kind = || {
        proposal
            .document_type
            .as_deref()
            .and_then(sanitize_folder_name)
            .unwrap_or_else(|| "Unsorted".to_owned())
    };
    match layout {
        DestinationLayout::Flat => None,
        DestinationLayout::Year => Some(PathBuf::from(year())),
        DestinationLayout::YearType => Some(PathBuf::from(year()).join(kind())),
        DestinationLayout::Type => Some(PathBuf::from(kind())),
        DestinationLayout::Party => Some(PathBuf::from(
            proposal
                .parties
                .first()
                .and_then(|party| sanitize_folder_name(party))
                .unwrap_or_else(|| "Unsorted".to_owned()),
        )),
    }
}

/// After an undo, removes the layout folders the vacated document was alone
/// in, walking up from its folder to (but never including) the destination
/// root. A folder holding anything else is left where it is; so is a folder
/// outside the root.
///
/// Never more folders than `layout` itself creates, either. A destination
/// changed to a folder that contains the old one puts everything ever filed
/// "inside the root", and walking up until the root then emptied out the
/// previous destination - somebody's filing, not Intern's scaffolding.
fn prune_empty_layout_folders(root: &Path, vacated: &Path, layout: DestinationLayout) {
    let mut remaining = match layout {
        DestinationLayout::Flat => 0,
        DestinationLayout::Year | DestinationLayout::Type | DestinationLayout::Party => 1,
        DestinationLayout::YearType => 2,
    };
    let mut folder = vacated.parent();
    while let Some(current) = folder {
        if remaining == 0 || current == root || !current.starts_with(root) {
            break;
        }
        remaining -= 1;
        let empty = fs::read_dir(current).is_ok_and(|mut entries| entries.next().is_none());
        if !empty || fs::remove_dir(current).is_err() {
            break;
        }
        folder = current.parent();
    }
}

/// The filed-document report for a completed apply receipt, or `None` when
/// the receipt is not one (an undo, or an apply that never completed).
fn filed_document(
    item_id: i64,
    source_hash: &str,
    receipt: &OperationReceipt,
    proposal: &ProposalRecord,
    filed_at: i64,
) -> Option<FiledDocument> {
    (receipt.direction == OperationDirection::Apply && receipt.stage == OperationStage::Complete)
        .then(|| FiledDocument {
            item_id,
            source_path: receipt.source.clone(),
            source_hash: source_hash.to_owned(),
            destination: receipt.destination.clone(),
            description: proposal.description.clone(),
            proposal: proposal_as_applied(
                &proposal.styled_proposal(),
                &receipt
                    .destination
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            filed_at,
            text_fingerprint: proposal.analysis.text_fingerprint.clone(),
        })
}

/// Whether a fingerprint match is one document filed twice, or two
/// documents that share their words: this month's statement and last
/// month's differ in a date and a few figures, which a fingerprint barely
/// sees. Dates settle it when both sides have one; without a date on one
/// side only a near-identical text may say duplicate.
fn same_document(distance: u32, filed_name: &str, date: Option<&str>) -> bool {
    if distance > NEAR_DUPLICATE_DISTANCE {
        return false;
    }
    match (leading_date(filed_name), date) {
        (Some(filed), Some(this)) => filed == this,
        _ => distance <= 1,
    }
}

/// The validated facts as the applied name carries them. A reviewer who
/// types a date, or accepts the one the model read, puts it in the filename
/// and nowhere else; the layout folder and the description record must
/// follow that date, not the one validation withheld.
pub fn proposal_as_applied(proposal: &ValidatedProposal, filename: &str) -> ValidatedProposal {
    let mut applied = proposal.clone();
    if let Some(date) = leading_date(filename) {
        applied.document_date = Some(date.to_owned());
    }
    applied
}

/// When a completed rename happened, as best the receipt can say: the
/// destination's modification time is the rename itself on most filesystems,
/// and a missing file (deleted since) falls back to now.
fn receipt_time(receipt: &OperationReceipt) -> i64 {
    fs::metadata(&receipt.destination)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(unix_now, |duration| duration.as_secs() as i64)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

fn model_error_code(error: &ModelFailure) -> ErrorCode {
    match error.code.as_str() {
        "MODEL_RESPONSE_INVALID" => ErrorCode::ModelOutputInvalid,
        "HOSTED_MODEL_REFUSED" => ErrorCode::ModelDeclined,
        _ => ErrorCode::IoError,
    }
}

/// The review reason and error code for a rename that carries no date.
pub const DATE_REQUIRED: &str = "DATE_REQUIRED";

/// What an apply reports when the queue is busy with another document. Not a
/// failure of the rename: the item stays ready and the scheduler applies it
/// as soon as it is free.
const APPLY_DEFERRED: &str = "APPLY_DEFERRED";

/// The date a filename begins with - `YYYY-MM-DD`, a real calendar date,
/// standing on its own before whatever follows - or `None`.
///
/// Every rename must carry one. A name without a date sorts nowhere and
/// says nothing about when, so the engine never proposes one as ready, and
/// a person approving a name types the date in (or takes the one the model
/// suggested) rather than filing an undated document.
pub fn leading_date(filename: &str) -> Option<&str> {
    let date = filename.get(..10)?;
    if !is_valid_iso_date(date) {
        return None;
    }
    let follows_cleanly = filename[10..]
        .chars()
        .next()
        .is_none_or(|next| !next.is_alphanumeric());
    follows_cleanly.then_some(date)
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

#[cfg(test)]
mod date_gate_tests {
    use intern_engine::{Evidence, PartyRelation, ValidatedProposal};

    use super::{leading_date, proposal_as_applied};

    #[test]
    fn the_applied_name_lends_its_date_to_the_facts_but_never_takes_one_away() {
        let withheld = ValidatedProposal {
            document_type: Some("Invoice".into()),
            document_date: None,
            date_role: None,
            parties: Vec::new(),
            party_relation: PartyRelation::None,
            description: "An invoice.".into(),
            confidence: 0.8,
            evidence: Evidence::default(),
        };
        assert_eq!(
            proposal_as_applied(&withheld, "2026-03-02 Invoice.pdf")
                .document_date
                .as_deref(),
            Some("2026-03-02")
        );
        let dated = ValidatedProposal {
            document_date: Some("2026-01-01".into()),
            ..withheld.clone()
        };
        assert_eq!(
            proposal_as_applied(&dated, "2026-03-02 Invoice.pdf")
                .document_date
                .as_deref(),
            Some("2026-03-02"),
            "a reviewer's date wins over the model's"
        );
        assert_eq!(
            proposal_as_applied(&dated, "Invoice.pdf")
                .document_date
                .as_deref(),
            Some("2026-01-01"),
            "a name without a date changes nothing"
        );
    }

    #[test]
    fn a_filename_carries_a_date_only_when_it_starts_with_a_real_one() {
        assert_eq!(
            leading_date("2026-03-02 Invoice from Acme.pdf"),
            Some("2026-03-02")
        );
        assert_eq!(leading_date("2026-03-02.pdf"), Some("2026-03-02"));
        assert_eq!(leading_date("2026-03-02-invoice.pdf"), Some("2026-03-02"));
        assert_eq!(leading_date("2026-03-02"), Some("2026-03-02"));
        assert_eq!(leading_date("Invoice from Acme.pdf"), None);
        assert_eq!(leading_date("Invoice 2026-03-02 from Acme.pdf"), None);
        assert_eq!(
            leading_date("2026-02-30 Invoice.pdf"),
            None,
            "not a real day"
        );
        assert_eq!(
            leading_date("2026-03-021 Invoice.pdf"),
            None,
            "digits run on"
        );
        assert_eq!(leading_date("2026-03-0"), None);
        assert_eq!(leading_date(""), None);
    }
}

#[cfg(test)]
mod sink_tests {
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    use intern_engine::{Evidence, PartyRelation, ValidatedProposal};

    use super::{FiledDocument, FilingSink, FilingSinks, KnownFiling, UnfiledDocument};

    #[derive(Default)]
    struct Heard(Mutex<Vec<String>>);

    impl FilingSink for Heard {
        fn filed(&self, document: &FiledDocument) {
            self.0
                .lock()
                .unwrap()
                .push(format!("filed {}", document.item_id));
        }
        fn unfiled(&self, document: &UnfiledDocument) {
            self.0
                .lock()
                .unwrap()
                .push(format!("unfiled {}", document.item_id));
        }
    }

    #[test]
    fn every_sink_behind_the_fan_out_hears_every_report_in_order() {
        let first = Arc::new(Heard::default());
        let second = Arc::new(Heard::default());
        let sinks = FilingSinks(vec![first.clone(), second.clone()]);
        sinks.filed(&FiledDocument {
            item_id: 4,
            source_path: PathBuf::from("/in/a.pdf"),
            source_hash: "hash".into(),
            destination: PathBuf::from("/out/a.pdf"),
            description: "A sentence.".into(),
            proposal: ValidatedProposal {
                document_type: None,
                document_date: None,
                date_role: None,
                parties: Vec::new(),
                party_relation: PartyRelation::Between,
                description: "A sentence.".into(),
                confidence: 0.5,
                evidence: Evidence::default(),
            },
            filed_at: 1,
            text_fingerprint: None,
        });
        sinks.unfiled(&UnfiledDocument {
            item_id: 4,
            source_path: PathBuf::from("/in/a.pdf"),
            source_hash: "hash".into(),
            destination: PathBuf::from("/out/a.pdf"),
        });
        for sink in [&first, &second] {
            assert_eq!(
                *sink.0.lock().unwrap(),
                vec!["filed 4".to_string(), "unfiled 4".to_string()]
            );
        }
    }

    #[test]
    fn a_known_filing_names_the_machine_only_when_there_is_one_to_name() {
        let teammate = KnownFiling {
            filename: "2026-03-02 Agreement.pdf".into(),
            filed_by: Some("Front desk".into()),
        };
        assert_eq!(
            teammate.describe(),
            "2026-03-02 Agreement.pdf (filed from Front desk)"
        );
        let own = KnownFiling {
            filename: "2026-03-02 Agreement.pdf".into(),
            filed_by: None,
        };
        assert_eq!(own.describe(), "2026-03-02 Agreement.pdf");
    }
}

#[cfg(test)]
mod layout_tests {
    use std::path::{Path, PathBuf};

    use intern_engine::{DateRole, Evidence, PartyRelation, ValidatedProposal};

    use super::{layout_subfolder, prune_empty_layout_folders, target_folder};
    use crate::settings::{AppSettings, DestinationLayout};

    fn proposal(date: Option<&str>, kind: Option<&str>, parties: &[&str]) -> ValidatedProposal {
        ValidatedProposal {
            document_type: kind.map(str::to_owned),
            document_date: date.map(str::to_owned),
            date_role: date.map(|_| DateRole::Effective),
            parties: parties.iter().map(|party| (*party).to_owned()).collect(),
            party_relation: PartyRelation::Between,
            description: "A description.".into(),
            confidence: 0.9,
            evidence: Evidence::default(),
        }
    }

    #[test]
    fn each_layout_derives_its_folder_from_the_documents_facts() {
        let full = proposal(
            Some("2026-04-01"),
            Some("Statement of Work"),
            &["Ridgeline Cartography LLC", "Vistage Worldwide, Inc."],
        );
        assert_eq!(layout_subfolder(DestinationLayout::Flat, &full), None);
        assert_eq!(
            layout_subfolder(DestinationLayout::Year, &full),
            Some(PathBuf::from("2026"))
        );
        assert_eq!(
            layout_subfolder(DestinationLayout::YearType, &full),
            Some(PathBuf::from("2026").join("Statement of Work"))
        );
        assert_eq!(
            layout_subfolder(DestinationLayout::Type, &full),
            Some(PathBuf::from("Statement of Work"))
        );
        assert_eq!(
            layout_subfolder(DestinationLayout::Party, &full),
            Some(PathBuf::from("Ridgeline Cartography LLC"))
        );
    }

    #[test]
    fn a_missing_fact_goes_to_a_named_catch_all_never_the_root() {
        let bare = proposal(None, None, &[]);
        assert_eq!(
            layout_subfolder(DestinationLayout::Year, &bare),
            Some(PathBuf::from("Undated"))
        );
        assert_eq!(
            layout_subfolder(DestinationLayout::YearType, &bare),
            Some(PathBuf::from("Undated").join("Unsorted"))
        );
        assert_eq!(
            layout_subfolder(DestinationLayout::Party, &bare),
            Some(PathBuf::from("Unsorted"))
        );
        // Hostile characters in a fact never reach a folder name.
        let hostile = proposal(Some("2026-04-01"), Some("Invoice: 3/4 <draft>"), &["CON"]);
        assert_eq!(
            layout_subfolder(DestinationLayout::Type, &hostile),
            Some(PathBuf::from("Invoice 34 draft"))
        );
        assert_eq!(
            layout_subfolder(DestinationLayout::Party, &hostile),
            Some(PathBuf::from("_CON"))
        );
    }

    #[test]
    fn the_target_folder_falls_back_to_the_documents_own_folder_without_a_destination() {
        let mut settings = AppSettings {
            destination_layout: DestinationLayout::Year,
            ..AppSettings::default()
        };
        let source = Path::new("/inbox/scan.pdf");
        let proposal = proposal(Some("2026-04-01"), None, &[]);
        assert_eq!(
            target_folder(&settings, source, &proposal),
            PathBuf::from("/inbox").join("2026")
        );
        settings.destination = "/filed".into();
        assert_eq!(
            target_folder(&settings, source, &proposal),
            PathBuf::from("/filed").join("2026")
        );
    }

    /// The destination can be changed to a folder that contains the old one.
    /// Everything under the old destination is then "inside the root", and
    /// walking up until the root emptied out the previous destination itself,
    /// which is somebody's filing rather than Intern's scaffolding. Only as
    /// many folders as the layout in force could have made are ever removed.
    #[test]
    fn pruning_never_reaches_above_the_folders_the_layout_makes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let previous_destination = root.join("filed");
        let vacated = previous_destination
            .join("2026")
            .join("Invoice")
            .join("a.pdf");
        std::fs::create_dir_all(vacated.parent().unwrap()).unwrap();
        prune_empty_layout_folders(&root, &vacated, DestinationLayout::YearType);
        assert!(!previous_destination.join("2026").exists());
        assert!(
            previous_destination.exists(),
            "the folder that used to be the destination is not Intern's to remove"
        );
    }

    #[test]
    fn pruning_stops_at_the_root_and_at_the_first_folder_with_contents() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("filed");
        let vacated = root.join("2026").join("Invoice").join("a.pdf");
        std::fs::create_dir_all(vacated.parent().unwrap()).unwrap();
        prune_empty_layout_folders(&root, &vacated, DestinationLayout::YearType);
        assert!(!root.join("2026").exists());
        assert!(root.exists());

        let sibling = root.join("2025").join("Invoice").join("b.pdf");
        std::fs::create_dir_all(sibling.parent().unwrap()).unwrap();
        std::fs::write(&sibling, b"x").unwrap();
        let vacated = root.join("2025").join("Notice").join("c.pdf");
        std::fs::create_dir_all(vacated.parent().unwrap()).unwrap();
        prune_empty_layout_folders(&root, &vacated, DestinationLayout::YearType);
        assert!(!root.join("2025").join("Notice").exists());
        assert!(
            root.join("2025").join("Invoice").exists(),
            "a year folder with another document in it stays"
        );

        // A path outside the root is never touched.
        let elsewhere = temp.path().join("elsewhere").join("d.pdf");
        std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
        prune_empty_layout_folders(&root, &elsewhere, DestinationLayout::YearType);
        assert!(elsewhere.parent().unwrap().exists());
    }
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
