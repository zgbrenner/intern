use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use intern_core::{ErrorCode, OperationReceipt, QueueItem, QueueStatus};
use intern_engine::{
    AnalysisTelemetry, DateRole, DigestBudget, DocumentAnalysis, DocumentSource, Evidence,
    ExtractProgress, ModelProposal, PageOrigin, ParserWarning, PartyRelation, ProposalStatus,
    SourcePage, distill, engine::finish, validate,
};
use intern_queue::{
    pipeline::{
        AnalyzerBoundary, DuplicateOracle, FileActions, FiledDocument, FilingSink, KnownFiling,
        ModelFailure, Pipeline, PipelineError, PipelineEventSink, PipelineProgress,
        UnfiledDocument, WorkerBoundary, WorkerFailure,
    },
    settings::{AppSettings, DestinationLayout, SettingsStore},
};
use rusqlite::Connection;
use tempfile::tempdir;

/// Runs the real distillation, validation, and naming over a canned model
/// reply, so queue tests still exercise the production evidence rules instead
/// of a stub that always agrees with itself.
fn analyze_locally(
    source: &DocumentSource,
    proposal: ModelProposal,
    extension: &str,
    existing_names: &[&str],
) -> DocumentAnalysis {
    let digest = distill(source, DigestBudget::default());
    let outcome = validate(proposal, &digest);
    finish(
        outcome,
        &digest,
        extension,
        existing_names,
        AnalysisTelemetry::default(),
    )
}

#[derive(Default)]
struct RecordingEvents {
    changed: AtomicUsize,
    progress: Mutex<Vec<PipelineProgress>>,
}

/// Remembers every filed-document report and every retraction, in order.
#[derive(Default)]
struct RecordingFiling {
    filed: Mutex<Vec<FiledDocument>>,
    unfiled: Mutex<Vec<UnfiledDocument>>,
}

impl FilingSink for RecordingFiling {
    fn filed(&self, document: &FiledDocument) {
        self.filed.lock().unwrap().push(document.clone());
    }
    fn unfiled(&self, document: &UnfiledDocument) {
        self.unfiled.lock().unwrap().push(document.clone());
    }
}

/// What teammates have filed, keyed by content hash - the shared filed index
/// as the queue sees it.
#[derive(Default)]
struct TeammateFilings {
    known: Mutex<HashMap<String, KnownFiling>>,
    asked: Mutex<Vec<(String, PathBuf)>>,
}

impl DuplicateOracle for TeammateFilings {
    fn filed_elsewhere(&self, source_hash: &str, source_path: &Path) -> Option<KnownFiling> {
        self.asked
            .lock()
            .unwrap()
            .push((source_hash.to_owned(), source_path.to_path_buf()));
        self.known.lock().unwrap().get(source_hash).cloned()
    }
}

impl PipelineEventSink for RecordingEvents {
    fn queue_changed(&self) {
        self.changed.fetch_add(1, Ordering::SeqCst);
    }
    fn progress(&self, progress: PipelineProgress) {
        self.progress.lock().unwrap().push(progress);
    }
}

struct FakeWorker {
    responses: Mutex<VecDeque<Result<DocumentSource, WorkerFailure>>>,
    active: AtomicUsize,
    maximum_active: AtomicUsize,
    cancellations: AtomicUsize,
    restarts: AtomicUsize,
    gate: (Mutex<bool>, Condvar),
}

impl FakeWorker {
    fn new(responses: Vec<Result<DocumentSource, WorkerFailure>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            active: AtomicUsize::new(0),
            maximum_active: AtomicUsize::new(0),
            cancellations: AtomicUsize::new(0),
            restarts: AtomicUsize::new(0),
            gate: (Mutex::new(false), Condvar::new()),
        }
    }

    fn blocking(response: Result<DocumentSource, WorkerFailure>) -> Self {
        let worker = Self::new(vec![response]);
        *worker.gate.0.lock().unwrap() = true;
        worker
    }
}

impl WorkerBoundary for FakeWorker {
    fn extract(
        &self,
        _request_id: &str,
        _path: &Path,
        _progress: &mut dyn FnMut(ExtractProgress),
    ) -> Result<DocumentSource, WorkerFailure> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum_active.fetch_max(active, Ordering::SeqCst);
        let (lock, wake) = &self.gate;
        let mut blocked = lock.lock().unwrap();
        while *blocked && self.cancellations.load(Ordering::SeqCst) == 0 {
            blocked = wake.wait(blocked).unwrap();
        }
        drop(blocked);
        self.active.fetch_sub(1, Ordering::SeqCst);
        if self.cancellations.load(Ordering::SeqCst) > 0 {
            return Err(WorkerFailure::canceled());
        }
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(WorkerFailure::new("PARSE_FAILED", false, false)))
    }

    fn cancel(&self, _request_id: &str) -> Result<(), WorkerFailure> {
        self.cancellations.fetch_add(1, Ordering::SeqCst);
        self.gate.1.notify_all();
        Ok(())
    }

    fn restart(&self) -> Result<(), WorkerFailure> {
        self.restarts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn shutdown(&self) -> Result<(), WorkerFailure> {
        self.cancellations.fetch_add(1, Ordering::SeqCst);
        self.gate.1.notify_all();
        Ok(())
    }
}

struct FakeModel {
    responses: Mutex<VecDeque<Result<ModelProposal, ModelFailure>>>,
    calls: AtomicUsize,
}

impl FakeModel {
    fn new(responses: Vec<Result<ModelProposal, ModelFailure>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            calls: AtomicUsize::new(0),
        }
    }
}

impl AnalyzerBoundary for FakeModel {
    fn analyze(
        &self,
        source: &DocumentSource,
        extension: &str,
        existing_names: &[&str],
    ) -> Result<DocumentAnalysis, ModelFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let proposal = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(ModelFailure::fatal("MODEL_RESPONSE_INVALID")))?;
        Ok(analyze_locally(source, proposal, extension, existing_names))
    }
}

struct GatedSuccessModel {
    calls: AtomicUsize,
    gate: (Mutex<bool>, Condvar),
}

struct RecoveringModel {
    responses: Mutex<VecDeque<Result<ModelProposal, ModelFailure>>>,
    calls: AtomicUsize,
    recoveries: AtomicUsize,
    recovery_succeeds: bool,
}

impl RecoveringModel {
    fn new(responses: Vec<Result<ModelProposal, ModelFailure>>, recovery_succeeds: bool) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            calls: AtomicUsize::new(0),
            recoveries: AtomicUsize::new(0),
            recovery_succeeds,
        }
    }
}

impl AnalyzerBoundary for RecoveringModel {
    fn analyze(
        &self,
        source: &DocumentSource,
        extension: &str,
        existing_names: &[&str],
    ) -> Result<DocumentAnalysis, ModelFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let proposal = self.responses.lock().unwrap().pop_front().unwrap()?;
        Ok(analyze_locally(source, proposal, extension, existing_names))
    }

    fn recover(&self, _failure: &ModelFailure) -> Result<(), ModelFailure> {
        self.recoveries.fetch_add(1, Ordering::SeqCst);
        if self.recovery_succeeds {
            Ok(())
        } else {
            Err(ModelFailure::fatal("MODEL_RECOVERY_FAILED"))
        }
    }
}

impl GatedSuccessModel {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            gate: (Mutex::new(true), Condvar::new()),
        }
    }

    fn release(&self) {
        *self.gate.0.lock().unwrap() = false;
        self.gate.1.notify_all();
    }
}

impl AnalyzerBoundary for GatedSuccessModel {
    fn analyze(
        &self,
        source: &DocumentSource,
        extension: &str,
        existing_names: &[&str],
    ) -> Result<DocumentAnalysis, ModelFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (lock, wake) = &self.gate;
        let mut blocked = lock.lock().unwrap();
        while *blocked {
            blocked = wake.wait(blocked).unwrap();
        }
        Ok(analyze_locally(
            source,
            proposal(0.94, false),
            extension,
            existing_names,
        ))
    }
}

struct BlockingModel {
    calls: AtomicUsize,
    cancel_started: AtomicBool,
    cancel_succeeds: bool,
    request_release: (Mutex<bool>, Condvar),
    cancel_release: (Mutex<bool>, Condvar),
}

impl BlockingModel {
    fn new(cancel_succeeds: bool) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            cancel_started: AtomicBool::new(false),
            cancel_succeeds,
            request_release: (Mutex::new(false), Condvar::new()),
            cancel_release: (Mutex::new(false), Condvar::new()),
        }
    }

    fn wait_for_cancel(&self) {
        // Thirty seconds, not two. The loop exits as soon as the flag is set, so a
        // healthy run is no slower; the deadline only decides how much scheduling
        // delay on a loaded runner counts as a failure, and two seconds of it is
        // ordinary rather than broken.
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.cancel_started.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(self.cancel_started.load(Ordering::SeqCst));
    }

    fn release_request(&self) {
        *self.request_release.0.lock().unwrap() = true;
        self.request_release.1.notify_all();
    }

    fn release_cancel(&self) {
        *self.cancel_release.0.lock().unwrap() = true;
        self.cancel_release.1.notify_all();
    }
}

impl AnalyzerBoundary for BlockingModel {
    fn analyze(
        &self,
        source: &DocumentSource,
        extension: &str,
        existing_names: &[&str],
    ) -> Result<DocumentAnalysis, ModelFailure> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 1 {
            let (lock, wake) = &self.request_release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
            return Err(ModelFailure::fatal("MODEL_REQUEST_CANCELED"));
        }
        Ok(analyze_locally(
            source,
            proposal(0.94, false),
            extension,
            existing_names,
        ))
    }

    fn cancel(&self) -> Result<(), ModelFailure> {
        self.cancel_started.store(true, Ordering::SeqCst);
        if self.cancel_succeeds {
            self.release_request();
        }
        let (lock, wake) = &self.cancel_release;
        let mut released = lock.lock().unwrap();
        while !*released {
            released = wake.wait(released).unwrap();
        }
        if self.cancel_succeeds {
            Ok(())
        } else {
            Err(ModelFailure::fatal("MODEL_CANCEL_FAILED"))
        }
    }
}

#[derive(Default)]
struct FakeFiles {
    hashes: Mutex<HashMap<PathBuf, String>>,
    applies: Mutex<Vec<i64>>,
    reconciles: Mutex<Vec<i64>>,
    fail_next_apply: AtomicUsize,
}

impl FakeFiles {
    fn trust(&self, path: &Path, hash: &str) {
        self.hashes
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), hash.to_owned());
    }

    fn forget(&self, path: &Path) {
        self.hashes.lock().unwrap().remove(path);
    }
    fn fail_next_apply(&self) {
        self.fail_next_apply.store(1, Ordering::SeqCst);
    }
}

impl FileActions for FakeFiles {
    fn fingerprint(&self, path: &Path) -> Result<String, PipelineError> {
        self.hashes
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| PipelineError::new("FILE_MISSING", "source file is unavailable"))
    }

    fn apply(&self, item: &QueueItem, _destination: &Path) -> Result<(), PipelineError> {
        if self
            .fail_next_apply
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(PipelineError::new("MOVE_FAILED", "injected apply failure"));
        }
        self.applies.lock().unwrap().push(item.id);
        Ok(())
    }

    fn undo(&self, _item: &QueueItem, _receipt: &OperationReceipt) -> Result<(), PipelineError> {
        Ok(())
    }

    fn reconcile(&self, item: &QueueItem) -> Result<(), PipelineError> {
        self.reconciles.lock().unwrap().push(item.id);
        Ok(())
    }
}

fn parsed(text: &str) -> DocumentSource {
    DocumentSource::from_pages(vec![SourcePage::new(1, text, PageOrigin::Native)])
}

fn proposal(confidence: f32, needs_review: bool) -> ModelProposal {
    ModelProposal {
        document_type: Some("Employment Agreement".into()),
        document_date: Some("2024-04-12".into()),
        date_role: Some(DateRole::Execution),
        parties: vec!["John Smith".into(), "Acme Corporation".into()],
        party_relation: PartyRelation::Between,
        description:
            "Employment agreement between John Smith and Acme Corporation covering duties, salary, and term."
                .into(),
        confidence,
        needs_review,
        evidence: Evidence {
            date: Some("signed April 12, 2024".into()),
            document_type: Some("Employment Agreement".into()),
            parties: vec!["by John Smith and Acme Corporation".into()],
        },
    }
}

fn source(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, name.as_bytes()).unwrap();
    path.canonicalize().unwrap()
}

fn pipeline(
    root: &Path,
    worker: Arc<FakeWorker>,
    model: Arc<FakeModel>,
    files: Arc<FakeFiles>,
    settings: AppSettings,
) -> Pipeline {
    let settings_store = SettingsStore::new(root.join("settings.json"));
    settings_store.save(&settings).unwrap();
    Pipeline::open(
        root.join("queue.sqlite3"),
        worker,
        model,
        files,
        Arc::new(RecordingEvents::default()),
        settings_store,
    )
    .unwrap()
}

#[test]
fn mixed_queue_is_sequential_and_ready_review_are_evidence_gated() {
    let temp = tempdir().unwrap();
    let first = source(temp.path(), "first.pdf");
    let second = source(temp.path(), "second.pdf");
    let worker = Arc::new(FakeWorker::new(vec![
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
        Ok(parsed(
            "Employment Agreement mentioning Acme Corporation only.",
        )),
    ]));
    let model = Arc::new(FakeModel::new(vec![
        Ok(proposal(0.94, false)),
        Ok(proposal(0.94, false)),
    ]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&first, "first-hash");
    files.trust(&second, "second-hash");
    let pipeline = pipeline(
        temp.path(),
        Arc::clone(&worker),
        model,
        Arc::clone(&files),
        AppSettings::default(),
    );

    pipeline.enqueue_files(&[first, second]).unwrap();
    pipeline.run_until_idle().unwrap();

    let items = pipeline.list().unwrap();
    assert_eq!(worker.maximum_active.load(Ordering::SeqCst), 1);
    assert_eq!(items[0].status, QueueStatus::Ready);
    assert_eq!(items[1].status, QueueStatus::NeedsReview);
    assert_eq!(
        items[0].proposal.as_ref().unwrap().status,
        ProposalStatus::Ready
    );
    assert_eq!(
        items[1].proposal.as_ref().unwrap().status,
        ProposalStatus::NeedsReview
    );
}

#[test]
fn retryable_malformed_model_output_retries_once_only() {
    let temp = tempdir().unwrap();
    let path = source(temp.path(), "retry.pdf");
    let worker = Arc::new(FakeWorker::new(vec![Ok(parsed(
        "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
    ))]));
    let model = Arc::new(FakeModel::new(vec![
        Err(ModelFailure::retryable("MODEL_RESPONSE_INVALID")),
        Ok(proposal(0.94, false)),
    ]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&path, "retry-hash");
    let pipeline = pipeline(
        temp.path(),
        worker,
        model.clone(),
        files,
        AppSettings::default(),
    );

    pipeline.enqueue_files(&[path]).unwrap();
    pipeline.run_until_idle().unwrap();

    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    assert_eq!(pipeline.list().unwrap()[0].status, QueueStatus::Ready);
}

#[test]
fn crashed_model_endpoint_is_recovered_once_before_the_only_retry() {
    let temp = tempdir().unwrap();
    let path = source(temp.path(), "model-crash.pdf");
    let worker = Arc::new(FakeWorker::new(vec![Ok(parsed(
        "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
    ))]));
    let model = Arc::new(RecoveringModel::new(
        vec![
            Err(ModelFailure::retryable("MODEL_REQUEST_FAILED")),
            Ok(proposal(0.94, false)),
        ],
        true,
    ));
    let files = Arc::new(FakeFiles::default());
    files.trust(&path, "model-hash");
    let settings = SettingsStore::new(temp.path().join("settings.json"));
    settings.save(&AppSettings::default()).unwrap();
    let pipeline = Pipeline::open(
        temp.path().join("queue.sqlite3"),
        worker,
        model.clone(),
        files,
        Arc::new(RecordingEvents::default()),
        settings,
    )
    .unwrap();
    pipeline.enqueue_files(&[path]).unwrap();

    pipeline.run_until_idle().unwrap();

    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    assert_eq!(model.recoveries.load(Ordering::SeqCst), 1);
    assert_eq!(pipeline.list().unwrap()[0].status, QueueStatus::Ready);
}

#[test]
fn failed_model_recovery_pauses_before_retrying_or_draining_next_item() {
    let temp = tempdir().unwrap();
    let first = source(temp.path(), "model-crash.pdf");
    let second = source(temp.path(), "must-wait.pdf");
    let worker = Arc::new(FakeWorker::new(vec![Ok(parsed(
        "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
    ))]));
    let model = Arc::new(RecoveringModel::new(
        vec![Err(ModelFailure::retryable("MODEL_REQUEST_FAILED"))],
        false,
    ));
    let files = Arc::new(FakeFiles::default());
    files.trust(&first, "first-hash");
    files.trust(&second, "second-hash");
    let settings = SettingsStore::new(temp.path().join("settings.json"));
    settings.save(&AppSettings::default()).unwrap();
    let pipeline = Pipeline::open(
        temp.path().join("queue.sqlite3"),
        worker,
        model.clone(),
        files,
        Arc::new(RecordingEvents::default()),
        settings,
    )
    .unwrap();
    pipeline.enqueue_files(&[first, second]).unwrap();

    pipeline.run_until_idle().unwrap();

    assert!(pipeline.is_paused());
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.recoveries.load(Ordering::SeqCst), 1);
    assert_eq!(pipeline.list().unwrap()[1].status, QueueStatus::Queued);
}

#[test]
fn failed_item_does_not_block_the_next_and_worker_restarts_only_once() {
    let temp = tempdir().unwrap();
    let first = source(temp.path(), "crashes.pdf");
    let second = source(temp.path(), "continues.pdf");
    let worker = Arc::new(FakeWorker::new(vec![
        Err(WorkerFailure::crashed()),
        Err(WorkerFailure::crashed()),
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
    ]));
    let model = Arc::new(FakeModel::new(vec![Ok(proposal(0.94, false))]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&first, "crash-hash");
    files.trust(&second, "continue-hash");
    let pipeline = pipeline(
        temp.path(),
        Arc::clone(&worker),
        model,
        files,
        AppSettings::default(),
    );

    pipeline.enqueue_files(&[first, second]).unwrap();
    pipeline.run_until_idle().unwrap();

    let items = pipeline.list().unwrap();
    assert_eq!(worker.restarts.load(Ordering::SeqCst), 1);
    assert_eq!(items[0].status, QueueStatus::Failed);
    assert_eq!(items[0].error_code, Some(ErrorCode::IoError));
    assert_eq!(items[1].status, QueueStatus::Ready);
}

#[test]
fn pause_starts_no_item_and_cancel_interrupts_the_active_worker_request() {
    let temp = tempdir().unwrap();
    let paused_path = source(temp.path(), "paused.pdf");
    let cancel_path = source(temp.path(), "cancel.pdf");
    let worker = Arc::new(FakeWorker::blocking(Ok(parsed("unused"))));
    let model = Arc::new(FakeModel::new(vec![]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&paused_path, "paused-hash");
    files.trust(&cancel_path, "cancel-hash");
    let pipeline = Arc::new(pipeline(
        temp.path(),
        Arc::clone(&worker),
        model,
        files,
        AppSettings::default(),
    ));

    pipeline.enqueue_files(&[paused_path, cancel_path]).unwrap();
    pipeline.pause();
    pipeline.run_until_idle().unwrap();
    assert!(
        pipeline
            .list()
            .unwrap()
            .iter()
            .all(|item| item.status == QueueStatus::Queued)
    );

    pipeline.resume();
    let running = Arc::clone(&pipeline);
    let join = thread::spawn(move || running.run_next());
    while worker.active.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }
    let active_id = pipeline.list().unwrap()[0].id;
    pipeline.cancel(active_id).unwrap();
    join.join().unwrap().unwrap();

    assert_eq!(worker.cancellations.load(Ordering::SeqCst), 1);
    assert_eq!(pipeline.list().unwrap()[0].status, QueueStatus::Canceled);
}

#[test]
fn pause_during_extraction_returns_item_to_queue_before_analysis() {
    let temp = tempdir().unwrap();
    let path = source(temp.path(), "pause-extracting.pdf");
    let worker = Arc::new(FakeWorker::blocking(Ok(parsed(
        "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
    ))));
    let model = Arc::new(FakeModel::new(vec![Ok(proposal(0.94, false))]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&path, "pause-hash");
    let pipeline = Arc::new(pipeline(
        temp.path(),
        Arc::clone(&worker),
        Arc::clone(&model),
        files,
        AppSettings::default(),
    ));
    pipeline.enqueue_files(&[path]).unwrap();

    let running = Arc::clone(&pipeline);
    let join = thread::spawn(move || running.run_until_idle());
    while worker.active.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }
    pipeline.pause();
    *worker.gate.0.lock().unwrap() = false;
    worker.gate.1.notify_all();
    join.join().unwrap().unwrap();

    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(pipeline.list().unwrap()[0].status, QueueStatus::Queued);
}

#[test]
fn pause_during_analysis_prevents_automatic_apply_until_resume() {
    let temp = tempdir().unwrap();
    let path = source(temp.path(), "pause-analysis.pdf");
    let worker = Arc::new(FakeWorker::new(vec![Ok(parsed(
        "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
    ))]));
    let model = Arc::new(GatedSuccessModel::new());
    let files = Arc::new(FakeFiles::default());
    files.trust(&path, "pause-hash");
    let settings = SettingsStore::new(temp.path().join("settings.json"));
    settings
        .save(&AppSettings {
            automatic_rename: true,
            ..AppSettings::default()
        })
        .unwrap();
    let pipeline = Arc::new(
        Pipeline::open(
            temp.path().join("queue.sqlite3"),
            worker,
            model.clone(),
            files.clone(),
            Arc::new(RecordingEvents::default()),
            settings,
        )
        .unwrap(),
    );
    pipeline.enqueue_files(&[path]).unwrap();

    let running = Arc::clone(&pipeline);
    let join = thread::spawn(move || running.run_until_idle());
    while model.calls.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }
    pipeline.pause();
    model.release();
    join.join().unwrap().unwrap();

    assert_eq!(pipeline.list().unwrap()[0].status, QueueStatus::Ready);
    assert!(files.applies.lock().unwrap().is_empty());

    pipeline.resume();
    pipeline.run_until_idle().unwrap();
    assert_eq!(files.applies.lock().unwrap().len(), 1);
}

#[test]
fn automatic_apply_targets_only_ready_items_and_rechecks_source_fingerprint() {
    let temp = tempdir().unwrap();
    let ready = source(temp.path(), "ready.pdf");
    let review = source(temp.path(), "review.pdf");
    let changed = source(temp.path(), "changed.pdf");
    let worker = Arc::new(FakeWorker::new(vec![
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
    ]));
    let model = Arc::new(FakeModel::new(vec![
        Ok(proposal(0.94, false)),
        Ok(proposal(0.70, true)),
        Ok(proposal(0.94, false)),
    ]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&ready, "ready-hash");
    files.trust(&review, "review-hash");
    files.trust(&changed, "changed-hash");
    let pipeline = pipeline(
        temp.path(),
        worker,
        model,
        Arc::clone(&files),
        AppSettings {
            automatic_rename: true,
            ..AppSettings::default()
        },
    );
    pipeline
        .enqueue_files(&[ready.clone(), review, changed.clone()])
        .unwrap();
    files.trust(&changed, "mutated-after-ingest");

    pipeline.run_until_idle().unwrap();

    let items = pipeline.list().unwrap();
    let applies = files.applies.lock().unwrap();
    assert_eq!(applies.as_slice(), &[items[0].id]);
    assert_eq!(items[1].status, QueueStatus::NeedsReview);
    assert_eq!(items[2].status, QueueStatus::NeedsReview);
    assert!(
        items[2]
            .proposal
            .as_ref()
            .unwrap()
            .reasons
            .iter()
            .any(|reason| reason == "FILE_CHANGED")
    );
}

/// Every rename carries a date. A reviewer who strips it - or a document
/// whose date the model could not support - is told so at approval, and the
/// name is applied only once a date leads it.
#[test]
fn an_approved_name_without_a_leading_date_is_refused_until_one_is_added() {
    let temp = tempdir().unwrap();
    let review = source(temp.path(), "review.pdf");
    let worker = Arc::new(FakeWorker::new(vec![Ok(parsed(
        "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
    ))]));
    let model = Arc::new(FakeModel::new(vec![Ok(proposal(0.70, true))]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&review, "review-hash");
    let pipeline = pipeline(
        temp.path(),
        worker,
        model,
        Arc::clone(&files),
        AppSettings::default(),
    );
    pipeline.enqueue_files(&[review]).unwrap();
    pipeline.run_until_idle().unwrap();
    let item = pipeline.list().unwrap().pop().unwrap();
    assert_eq!(item.status, QueueStatus::NeedsReview);

    for undated in [
        "Employment Agreement with John Smith.pdf",
        "Employment Agreement 2024-04-12 with John Smith.pdf",
        "2024-04-1 Employment Agreement.pdf",
    ] {
        let refused = pipeline
            .approve(item.id, undated, "A sentence.")
            .unwrap_err();
        assert_eq!(refused.code, "DATE_REQUIRED", "{undated}");
    }
    let unchanged = pipeline.list().unwrap().pop().unwrap();
    assert_eq!(unchanged.status, QueueStatus::NeedsReview);
    assert!(files.applies.lock().unwrap().is_empty());

    pipeline
        .approve(
            item.id,
            "2024-04-12 Employment Agreement with John Smith.pdf",
            "A sentence.",
        )
        .unwrap();
    assert_eq!(files.applies.lock().unwrap().as_slice(), &[item.id]);
}

#[test]
fn fingerprint_failure_is_durable_and_does_not_stop_the_queue() {
    let temp = tempdir().unwrap();
    let first = source(temp.path(), "missing-after-ingest.pdf");
    let second = source(temp.path(), "still-processes.pdf");
    let worker = Arc::new(FakeWorker::new(vec![
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
    ]));
    let model = Arc::new(FakeModel::new(vec![
        Ok(proposal(0.94, false)),
        Ok(proposal(0.94, false)),
    ]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&first, "first-hash");
    files.trust(&second, "second-hash");
    let pipeline = pipeline(
        temp.path(),
        worker,
        model,
        Arc::clone(&files),
        AppSettings {
            automatic_rename: true,
            ..AppSettings::default()
        },
    );
    pipeline.enqueue_files(&[first.clone(), second]).unwrap();
    files.forget(&first);

    pipeline.run_until_idle().unwrap();

    let items = pipeline.list().unwrap();
    assert_eq!(items[0].status, QueueStatus::NeedsReview);
    assert!(
        items[0]
            .proposal
            .as_ref()
            .unwrap()
            .reasons
            .iter()
            .any(|reason| reason == "FILE_MISSING")
    );
    assert_eq!(files.applies.lock().unwrap().as_slice(), &[items[1].id]);
}

#[test]
fn apply_failure_is_durable_and_does_not_stop_the_queue() {
    let temp = tempdir().unwrap();
    let first = source(temp.path(), "apply-fails.pdf");
    let second = source(temp.path(), "apply-continues.pdf");
    let worker = Arc::new(FakeWorker::new(vec![
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
    ]));
    let model = Arc::new(FakeModel::new(vec![
        Ok(proposal(0.94, false)),
        Ok(proposal(0.94, false)),
    ]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&first, "first-hash");
    files.trust(&second, "second-hash");
    files.fail_next_apply();
    let pipeline = pipeline(
        temp.path(),
        worker,
        model,
        Arc::clone(&files),
        AppSettings {
            automatic_rename: true,
            ..AppSettings::default()
        },
    );
    pipeline.enqueue_files(&[first, second]).unwrap();

    pipeline.run_until_idle().unwrap();

    let items = pipeline.list().unwrap();
    assert_eq!(items[0].status, QueueStatus::NeedsReview);
    assert!(
        items[0]
            .proposal
            .as_ref()
            .unwrap()
            .reasons
            .iter()
            .any(|reason| reason == "MOVE_FAILED")
    );
    assert_eq!(files.applies.lock().unwrap().as_slice(), &[items[1].id]);
}

#[test]
fn settings_failure_is_recorded_for_each_item_while_the_queue_drains() {
    let temp = tempdir().unwrap();
    let first = source(temp.path(), "settings-first.pdf");
    let second = source(temp.path(), "settings-second.pdf");
    let worker = Arc::new(FakeWorker::new(vec![
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
    ]));
    let model = Arc::new(FakeModel::new(vec![
        Ok(proposal(0.94, false)),
        Ok(proposal(0.94, false)),
    ]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&first, "first-hash");
    files.trust(&second, "second-hash");
    let pipeline = pipeline(temp.path(), worker, model, files, AppSettings::default());
    pipeline.enqueue_files(&[first, second]).unwrap();
    std::fs::write(temp.path().join("settings.json"), b"not-json").unwrap();

    pipeline.run_until_idle().unwrap();

    let items = pipeline.list().unwrap();
    assert!(
        items
            .iter()
            .all(|item| item.status == QueueStatus::NeedsReview)
    );
    assert!(items.iter().all(|item| {
        item.proposal
            .as_ref()
            .unwrap()
            .reasons
            .iter()
            .any(|reason| reason == "SETTINGS_INVALID")
    }));
}

#[test]
fn field_affecting_parser_warning_prevents_ready() {
    let temp = tempdir().unwrap();
    let path = source(temp.path(), "warning.pdf");
    let mut document =
        parsed("Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.");
    document.parser_warnings.push(ParserWarning {
        code: "TEXT_TRUNCATED".into(),
        field_affecting: true,
    });
    let worker = Arc::new(FakeWorker::new(vec![Ok(document)]));
    let model = Arc::new(FakeModel::new(vec![Ok(proposal(0.94, false))]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&path, "warning-hash");
    let pipeline = pipeline(temp.path(), worker, model, files, AppSettings::default());
    pipeline.enqueue_files(&[path]).unwrap();

    pipeline.run_until_idle().unwrap();

    assert_eq!(pipeline.list().unwrap()[0].status, QueueStatus::NeedsReview);
}

#[test]
fn initial_proposal_and_ready_transition_commit_atomically() {
    let temp = tempdir().unwrap();
    let path = source(temp.path(), "atomic.pdf");
    let worker = Arc::new(FakeWorker::new(vec![Ok(parsed(
        "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
    ))]));
    let model = Arc::new(FakeModel::new(vec![Ok(proposal(0.94, false))]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&path, "atomic-hash");
    let pipeline = pipeline(temp.path(), worker, model, files, AppSettings::default());
    pipeline.enqueue_files(&[path]).unwrap();
    let database = temp.path().join("queue.sqlite3");
    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_ready BEFORE UPDATE OF status ON queue_items
         WHEN NEW.status = 'ready' BEGIN SELECT RAISE(ABORT, 'reject ready'); END;",
        )
        .unwrap();

    assert!(pipeline.run_until_idle().is_err());

    let connection = Connection::open(database).unwrap();
    let proposals: i64 = connection
        .query_row("SELECT COUNT(*) FROM proposals", [], |row| row.get(0))
        .unwrap();
    assert_eq!(proposals, 0);
}

#[test]
fn real_sqlite_and_core_file_actions_apply_then_undo_the_operation_receipt() {
    let temp = tempdir().unwrap();
    let path = source(temp.path(), "real-file.pdf");
    let worker = Arc::new(FakeWorker::new(vec![Ok(parsed(
        "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
    ))]));
    let model = Arc::new(FakeModel::new(vec![Ok(proposal(0.94, false))]));
    let settings = SettingsStore::new(temp.path().join("settings.json"));
    settings
        .save(&AppSettings {
            automatic_rename: true,
            ..AppSettings::default()
        })
        .unwrap();
    let filing = Arc::new(RecordingFiling::default());
    let pipeline = Pipeline::with_local_files(
        temp.path().join("real-queue.sqlite3"),
        worker,
        model,
        Arc::new(RecordingEvents::default()),
        settings,
    )
    .unwrap()
    .with_filing_sink(filing.clone());
    pipeline.enqueue_files(std::slice::from_ref(&path)).unwrap();
    assert!(
        pipeline.filed_documents().unwrap().is_empty(),
        "nothing is filed before the queue runs"
    );

    pipeline.run_until_idle().unwrap();

    let completed = pipeline.list().unwrap().pop().unwrap();
    assert_eq!(completed.status, QueueStatus::Completed);
    let receipt = completed.receipt.clone().unwrap();
    assert!(!receipt.source.exists());
    assert!(receipt.destination.exists());

    // The filing sink hears about the rename once it is complete, with the
    // destination the applier actually chose and the sentence that was applied.
    let filed = filing.filed.lock().unwrap().clone();
    assert_eq!(filed.len(), 1, "{filed:?}");
    assert_eq!(filed[0].item_id, completed.id);
    assert_eq!(filed[0].source_path, path);
    assert_eq!(filed[0].source_hash, completed.source_hash);
    assert_eq!(filed[0].source_hash.len(), 64, "the content hash, as hex");
    assert_eq!(filed[0].destination, receipt.destination);
    assert_eq!(
        filed[0].description,
        completed.proposal.as_ref().unwrap().description
    );
    assert_eq!(
        filed[0].proposal.document_date.as_deref(),
        Some("2024-04-12")
    );
    assert!(filed[0].filed_at > 0);
    // The same report is available on demand, for a records keeper that is
    // switched on after the fact.
    let replay = pipeline.filed_documents().unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].destination, receipt.destination);
    assert_eq!(replay[0].description, filed[0].description);

    // The original path no longer exists, and still finds its item; the
    // renamed path was never enqueued and finds nothing.
    let by_path = pipeline.find_by_source_path(&path).unwrap().unwrap();
    assert_eq!(by_path.id, completed.id);
    assert_eq!(by_path.status, QueueStatus::Completed);
    assert!(by_path.receipt.is_some());
    assert!(
        pipeline
            .find_by_source_path(&receipt.destination)
            .unwrap()
            .is_none()
    );

    pipeline.undo(completed.id).unwrap();

    assert_eq!(pipeline.list().unwrap()[0].status, QueueStatus::Ready);
    assert!(path.exists());
    assert!(!receipt.destination.exists());
    assert_eq!(
        filing.unfiled.lock().unwrap().clone(),
        vec![UnfiledDocument {
            item_id: completed.id,
            source_path: path.clone(),
            source_hash: completed.source_hash.clone(),
            destination: receipt.destination.clone(),
        }],
        "an undo retracts the report for the destination it vacated"
    );
    assert!(
        pipeline.filed_documents().unwrap().is_empty(),
        "an undone rename is no longer a filed document"
    );
    assert_eq!(
        pipeline.find_by_source_path(&path).unwrap().unwrap().status,
        QueueStatus::Ready
    );
}

/// A year of contracts should not become one folder of a thousand files. The
/// layout puts each document under its year and type, creates the folders as
/// documents arrive, and takes them away again when an undo empties them.
#[test]
fn a_layout_files_into_year_and_type_subfolders_and_undo_removes_the_empty_ones() {
    let temp = tempdir().unwrap();
    let inbox = temp.path().join("inbox");
    let filed = temp.path().join("filed");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::create_dir_all(&filed).unwrap();
    let path = source(&inbox, "scan.pdf");
    let worker = Arc::new(FakeWorker::new(vec![Ok(parsed(
        "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
    ))]));
    let model = Arc::new(FakeModel::new(vec![Ok(proposal(0.94, false))]));
    let settings = SettingsStore::new(temp.path().join("settings.json"));
    settings
        .save(&AppSettings {
            automatic_rename: true,
            destination: filed.to_string_lossy().into_owned(),
            destination_layout: DestinationLayout::YearType,
            ..AppSettings::default()
        })
        .unwrap();
    let filing = Arc::new(RecordingFiling::default());
    let pipeline = Pipeline::with_local_files(
        temp.path().join("queue.sqlite3"),
        worker,
        model,
        Arc::new(RecordingEvents::default()),
        settings,
    )
    .unwrap()
    .with_filing_sink(filing.clone());
    pipeline.enqueue_files(std::slice::from_ref(&path)).unwrap();

    pipeline.run_until_idle().unwrap();

    let completed = pipeline.list().unwrap().pop().unwrap();
    assert_eq!(completed.status, QueueStatus::Completed);
    let receipt = completed.receipt.clone().unwrap();
    let expected_folder = filed.join("2024").join("Employment Agreement");
    assert_eq!(
        receipt.destination.parent(),
        Some(expected_folder.as_path())
    );
    assert_eq!(
        receipt
            .destination
            .file_name()
            .and_then(|name| name.to_str()),
        Some("2024-04-12 Employment Agreement between John Smith and Acme Corporation.pdf")
    );
    assert!(receipt.destination.exists());
    assert_eq!(
        filing.filed.lock().unwrap()[0].destination,
        receipt.destination,
        "the records keeper hears the path with its subfolders"
    );

    pipeline.undo(completed.id).unwrap();

    assert!(path.exists());
    assert!(!receipt.destination.exists());
    assert!(
        !expected_folder.exists() && !filed.join("2024").exists(),
        "an undo takes the folders it emptied away again"
    );
    assert!(filed.exists(), "the destination itself is never removed");
}

/// A reviewer's date lives in the filename and nowhere else. The layout
/// folder and the filing report must follow it, not the date validation
/// withheld, or a document dated by hand lands in "Undated" with a record
/// that says it has no date.
#[test]
fn a_date_given_in_review_is_the_date_the_document_is_filed_under() {
    let temp = tempdir().unwrap();
    let inbox = temp.path().join("inbox");
    let filed = temp.path().join("filed");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::create_dir_all(&filed).unwrap();
    let path = source(&inbox, "scan.pdf");
    // The model's date is nowhere in the text, so validation withholds it.
    let worker = Arc::new(FakeWorker::new(vec![Ok(parsed(
        "Employment Agreement between John Smith and Acme Corporation covering duties, salary, and term.",
    ))]));
    let model = Arc::new(FakeModel::new(vec![Ok(proposal(0.94, false))]));
    let settings = SettingsStore::new(temp.path().join("settings.json"));
    settings
        .save(&AppSettings {
            destination: filed.to_string_lossy().into_owned(),
            destination_layout: DestinationLayout::Year,
            ..AppSettings::default()
        })
        .unwrap();
    let filing = Arc::new(RecordingFiling::default());
    let pipeline = Pipeline::with_local_files(
        temp.path().join("queue.sqlite3"),
        worker,
        model,
        Arc::new(RecordingEvents::default()),
        settings,
    )
    .unwrap()
    .with_filing_sink(filing.clone());
    pipeline.enqueue_files(std::slice::from_ref(&path)).unwrap();
    pipeline.run_until_idle().unwrap();

    let item = pipeline.list().unwrap().pop().unwrap();
    assert_eq!(item.status, QueueStatus::NeedsReview);
    let record = item.proposal.as_ref().unwrap();
    assert_eq!(record.analysis.proposal.document_date, None);
    assert!(
        record
            .reasons
            .iter()
            .any(|reason| reason == "DATE_UNSUPPORTED")
    );
    assert_eq!(
        record
            .analysis
            .model_proposal
            .as_ref()
            .and_then(|reply| reply.document_date.as_deref()),
        Some("2024-04-12"),
        "the model's reading is kept for the reviewer to accept"
    );

    pipeline
        .approve(
            item.id,
            "2025-01-15 Employment Agreement with John Smith.pdf",
            "Employment agreement between John Smith and Acme Corporation.",
        )
        .unwrap();

    let completed = pipeline.list().unwrap().pop().unwrap();
    assert_eq!(completed.status, QueueStatus::Completed);
    let receipt = completed.receipt.unwrap();
    assert_eq!(
        receipt.destination,
        filed
            .join("2025")
            .join("2025-01-15 Employment Agreement with John Smith.pdf"),
        "the year folder follows the date the reviewer gave"
    );
    let heard = filing.filed.lock().unwrap();
    assert_eq!(heard.len(), 1);
    assert_eq!(
        heard[0].proposal.document_date.as_deref(),
        Some("2025-01-15")
    );
    drop(heard);
    let replay = pipeline.filed_documents().unwrap();
    assert_eq!(
        replay[0].proposal.document_date.as_deref(),
        Some("2025-01-15")
    );
}

/// The name that will be applied must not collide in the folder the
/// document is going to; the engine only knows the source folder.
#[test]
fn proposed_names_avoid_collisions_in_the_destination_not_the_source() {
    let temp = tempdir().unwrap();
    let inbox = temp.path().join("inbox");
    let filed = temp.path().join("filed");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::create_dir_all(&filed).unwrap();
    let path = source(&inbox, "scan.pdf");
    std::fs::write(
        filed.join("2024-04-12 Employment Agreement between John Smith and Acme Corporation.pdf"),
        b"already filed",
    )
    .unwrap();
    let worker = Arc::new(FakeWorker::new(vec![Ok(parsed(
        "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
    ))]));
    let model = Arc::new(FakeModel::new(vec![Ok(proposal(0.94, false))]));
    let settings = SettingsStore::new(temp.path().join("settings.json"));
    settings
        .save(&AppSettings {
            destination: filed.to_string_lossy().into_owned(),
            ..AppSettings::default()
        })
        .unwrap();
    let pipeline = Pipeline::with_local_files(
        temp.path().join("queue.sqlite3"),
        worker,
        model,
        Arc::new(RecordingEvents::default()),
        settings,
    )
    .unwrap();
    pipeline.enqueue_files(std::slice::from_ref(&path)).unwrap();

    pipeline.run_until_idle().unwrap();

    let ready = pipeline.list().unwrap().pop().unwrap();
    assert_eq!(ready.status, QueueStatus::Ready);
    assert_eq!(
        ready.proposal.unwrap().filename,
        "2024-04-12 Employment Agreement between John Smith and Acme Corporation (2).pdf",
        "the reviewer is shown the name that will actually be used"
    );
}

#[test]
fn model_timeout_does_not_start_another_proposal_until_cancel_is_terminal() {
    let temp = tempdir().unwrap();
    let first = source(temp.path(), "timeout-first.pdf");
    let second = source(temp.path(), "timeout-second.pdf");
    let worker = Arc::new(FakeWorker::new(vec![
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
    ]));
    let model = Arc::new(BlockingModel::new(true));
    let files = Arc::new(FakeFiles::default());
    files.trust(&first, "first-hash");
    files.trust(&second, "second-hash");
    let pipeline = Arc::new(
        Pipeline::open(
            temp.path().join("queue.sqlite3"),
            worker,
            model.clone(),
            files,
            Arc::new(RecordingEvents::default()),
            SettingsStore::new(temp.path().join("settings.json")),
        )
        .unwrap()
        .with_model_timeout(Duration::from_millis(20)),
    );
    pipeline.enqueue_files(&[first, second]).unwrap();
    let running = Arc::clone(&pipeline);
    let join = thread::spawn(move || running.run_until_idle());

    model.wait_for_cancel();
    thread::sleep(Duration::from_millis(30));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    model.release_cancel();
    join.join().unwrap().unwrap();

    assert!(model.calls.load(Ordering::SeqCst) >= 3);
}

#[test]
fn failed_model_timeout_cancel_pauses_drain_until_request_is_terminal() {
    let temp = tempdir().unwrap();
    let first = source(temp.path(), "cancel-fails.pdf");
    let second = source(temp.path(), "must-not-start.pdf");
    let worker = Arc::new(FakeWorker::new(vec![Ok(parsed(
        "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
    ))]));
    let model = Arc::new(BlockingModel::new(false));
    let files = Arc::new(FakeFiles::default());
    files.trust(&first, "first-hash");
    files.trust(&second, "second-hash");
    let pipeline = Arc::new(
        Pipeline::open(
            temp.path().join("queue.sqlite3"),
            worker,
            model.clone(),
            files,
            Arc::new(RecordingEvents::default()),
            SettingsStore::new(temp.path().join("settings.json")),
        )
        .unwrap()
        .with_model_timeout(Duration::from_millis(20)),
    );
    pipeline.enqueue_files(&[first, second]).unwrap();
    let running = Arc::clone(&pipeline);
    let join = thread::spawn(move || running.run_until_idle());

    model.wait_for_cancel();
    model.release_cancel();
    thread::sleep(Duration::from_millis(30));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    model.release_request();
    join.join().unwrap().unwrap();

    assert!(pipeline.is_paused());
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn shutdown_cancels_blocking_worker_before_waiting_for_pipeline_exit() {
    let temp = tempdir().unwrap();
    let path = source(temp.path(), "shutdown.pdf");
    let worker = Arc::new(FakeWorker::blocking(Ok(parsed("unused"))));
    let model = Arc::new(FakeModel::new(vec![]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&path, "shutdown-hash");
    let pipeline = Arc::new(pipeline(
        temp.path(),
        Arc::clone(&worker),
        model,
        files,
        AppSettings::default(),
    ));
    pipeline.enqueue_files(&[path]).unwrap();
    let running = Arc::clone(&pipeline);
    let join = thread::spawn(move || running.run_next());
    while worker.active.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }

    let started = Instant::now();
    pipeline.shutdown().unwrap();
    join.join().unwrap().unwrap_err();

    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(worker.cancellations.load(Ordering::SeqCst), 1);
}

#[test]
fn lost_processing_lease_cancels_worker_and_pauses_before_more_work() {
    let temp = tempdir().unwrap();
    let path = source(temp.path(), "lease-loss.pdf");
    let worker = Arc::new(FakeWorker::blocking(Ok(parsed("unused"))));
    let model = Arc::new(FakeModel::new(vec![]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&path, "lease-hash");
    let pipeline = Arc::new(
        pipeline(
            temp.path(),
            Arc::clone(&worker),
            model,
            files,
            AppSettings::default(),
        )
        .with_lease_renewal_interval(Duration::from_millis(10)),
    );
    pipeline.enqueue_files(&[path]).unwrap();
    let running = Arc::clone(&pipeline);
    let join = thread::spawn(move || running.run_next());
    while worker.active.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }
    Connection::open(temp.path().join("queue.sqlite3"))
        .unwrap()
        .execute("DELETE FROM queue_sessions", [])
        .unwrap();

    let error = join.join().unwrap().unwrap_err();

    assert_eq!(error.code, "STATE_CONFLICT");
    assert!(pipeline.is_paused());
    assert_eq!(worker.cancellations.load(Ordering::SeqCst), 1);
}

#[test]
fn refiled_content_at_a_new_path_is_flagged_duplicate_and_retry_analyzes_it() {
    let temp = tempdir().unwrap();
    let original = source(temp.path(), "agreement.pdf");
    let worker = Arc::new(FakeWorker::new(vec![
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
        Ok(parsed(
            "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
        )),
    ]));
    let model = Arc::new(FakeModel::new(vec![
        Ok(proposal(0.94, false)),
        Ok(proposal(0.94, false)),
    ]));
    let settings = SettingsStore::new(temp.path().join("settings.json"));
    settings
        .save(&AppSettings {
            automatic_rename: true,
            ..AppSettings::default()
        })
        .unwrap();
    let pipeline = Pipeline::with_local_files(
        temp.path().join("queue.sqlite3"),
        worker,
        Arc::clone(&model) as Arc<dyn AnalyzerBoundary>,
        Arc::new(RecordingEvents::default()),
        settings,
    )
    .unwrap();
    pipeline.enqueue_files(&[original]).unwrap();
    pipeline.run_until_idle().unwrap();
    let completed = pipeline.list().unwrap().pop().unwrap();
    assert_eq!(completed.status, QueueStatus::Completed);
    let filed_name = completed
        .receipt
        .unwrap()
        .destination
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    // The same bytes arrive again under a different name in another folder.
    let copies = temp.path().join("copies");
    std::fs::create_dir(&copies).unwrap();
    std::fs::write(copies.join("copy-of-agreement.pdf"), "agreement.pdf").unwrap();
    let copy = copies.join("copy-of-agreement.pdf").canonicalize().unwrap();
    let added = pipeline.enqueue_files(&[copy]).unwrap().pop().unwrap();
    assert_eq!(added.status, QueueStatus::NeedsReview);
    assert_eq!(added.error_code, Some(ErrorCode::Duplicate));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    let flagged = pipeline
        .list()
        .unwrap()
        .into_iter()
        .find(|item| item.id == added.id)
        .unwrap();
    assert_eq!(flagged.duplicate_of.as_deref(), Some(filed_name.as_str()));

    // "Process anyway": retry clears the flag and the item analyzes normally.
    pipeline.retry(added.id).unwrap();
    let requeued = pipeline
        .list()
        .unwrap()
        .into_iter()
        .find(|item| item.id == added.id)
        .unwrap();
    assert_eq!(requeued.status, QueueStatus::Queued);
    assert_eq!(requeued.error_code, None);
    pipeline.run_until_idle().unwrap();
    let settled = pipeline
        .list()
        .unwrap()
        .into_iter()
        .find(|item| item.id == added.id)
        .unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    assert_eq!(settled.status, QueueStatus::Completed);
}

/// A teammate filed this content last week from the shared intake folder.
/// This machine's history has never seen it, so only the shared index knows;
/// the document goes to review before any analysis, naming the filing and
/// the machine, and "process anyway" still works.
#[test]
fn content_a_teammate_already_filed_is_flagged_before_analysis_and_names_their_machine() {
    let temp = tempdir().unwrap();
    let again = source(temp.path(), "agreement-again.pdf");
    let fresh = source(temp.path(), "fresh.pdf");
    let worker = Arc::new(FakeWorker::new(vec![]));
    let model = Arc::new(FakeModel::new(vec![]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&again, "teammate-hash");
    files.trust(&fresh, "fresh-hash");
    let teammates = Arc::new(TeammateFilings::default());
    teammates.known.lock().unwrap().insert(
        "teammate-hash".into(),
        KnownFiling {
            filename: "2024-04-12 Employment Agreement.pdf".into(),
            filed_by: Some("Front desk".into()),
        },
    );
    let pipeline = pipeline(
        temp.path(),
        worker,
        model.clone(),
        files,
        AppSettings::default(),
    )
    .with_duplicate_oracle(teammates.clone());

    let added = pipeline
        .enqueue_files(&[again.clone(), fresh.clone()])
        .unwrap();

    assert_eq!(added[0].status, QueueStatus::NeedsReview);
    assert_eq!(added[0].error_code, Some(ErrorCode::Duplicate));
    assert_eq!(
        added[1].status,
        QueueStatus::Queued,
        "unknown content is queued"
    );
    assert_eq!(added[1].error_code, None);
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        teammates.asked.lock().unwrap().clone(),
        vec![
            ("teammate-hash".to_string(), again.clone()),
            ("fresh-hash".to_string(), fresh.clone()),
        ],
        "asked once per document, with the hash and the path"
    );
    let flagged = pipeline
        .list()
        .unwrap()
        .into_iter()
        .find(|item| item.id == added[0].id)
        .unwrap();
    assert_eq!(
        flagged.duplicate_of.as_deref(),
        Some("2024-04-12 Employment Agreement.pdf (filed from Front desk)")
    );

    // "Process anyway" clears the flag; the oracle is not asked again.
    pipeline.retry(added[0].id).unwrap();
    let requeued = pipeline
        .list()
        .unwrap()
        .into_iter()
        .find(|item| item.id == added[0].id)
        .unwrap();
    assert_eq!(requeued.status, QueueStatus::Queued);
    assert_eq!(requeued.error_code, None);
    assert_eq!(requeued.duplicate_of, None);
    // Two enqueues, plus the one listing that named the referent while the
    // item was still flagged; a retried item is never asked about again.
    assert_eq!(teammates.asked.lock().unwrap().len(), 3);
}

#[test]
fn same_content_still_pending_does_not_flag_a_duplicate() {
    let temp = tempdir().unwrap();
    let first = source(temp.path(), "pending-a.pdf");
    let second = source(temp.path(), "pending-b.pdf");
    let worker = Arc::new(FakeWorker::new(vec![]));
    let model = Arc::new(FakeModel::new(vec![]));
    let files = Arc::new(FakeFiles::default());
    files.trust(&first, "shared-hash");
    files.trust(&second, "shared-hash");
    let pipeline = pipeline(temp.path(), worker, model, files, AppSettings::default());

    let added = pipeline.enqueue_files(&[first, second]).unwrap();

    assert_eq!(added.len(), 2);
    assert!(added.iter().all(|item| item.status == QueueStatus::Queued));
    assert!(added.iter().all(|item| item.error_code.is_none()));
}

#[test]
fn undone_completion_is_not_flagged_as_a_duplicate_on_re_add() {
    let temp = tempdir().unwrap();
    let original = source(temp.path(), "undone.pdf");
    let worker = Arc::new(FakeWorker::new(vec![Ok(parsed(
        "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
    ))]));
    let model = Arc::new(FakeModel::new(vec![Ok(proposal(0.94, false))]));
    let settings = SettingsStore::new(temp.path().join("settings.json"));
    settings
        .save(&AppSettings {
            automatic_rename: true,
            ..AppSettings::default()
        })
        .unwrap();
    let pipeline = Pipeline::with_local_files(
        temp.path().join("queue.sqlite3"),
        worker,
        model,
        Arc::new(RecordingEvents::default()),
        settings,
    )
    .unwrap();
    pipeline
        .enqueue_files(std::slice::from_ref(&original))
        .unwrap();
    pipeline.run_until_idle().unwrap();
    let completed = pipeline.list().unwrap().pop().unwrap();
    assert_eq!(completed.status, QueueStatus::Completed);

    pipeline.undo(completed.id).unwrap();
    assert_eq!(pipeline.list().unwrap()[0].status, QueueStatus::Ready);

    // The apply was undone, so the content is not filed anywhere: a new copy
    // must analyze normally instead of being flagged.
    std::fs::write(temp.path().join("undone-copy.pdf"), "undone.pdf").unwrap();
    let copy = temp.path().join("undone-copy.pdf").canonicalize().unwrap();
    let added = pipeline.enqueue_files(&[copy]).unwrap().pop().unwrap();
    assert_eq!(added.status, QueueStatus::Queued);
    assert_eq!(added.error_code, None);
}

#[test]
fn flagged_duplicates_support_keep_original_remove_and_cleared_history() {
    let temp = tempdir().unwrap();
    let original = source(temp.path(), "keeper.pdf");
    let worker = Arc::new(FakeWorker::new(vec![Ok(parsed(
        "Employment Agreement signed April 12, 2024 by John Smith and Acme Corporation.",
    ))]));
    let model = Arc::new(FakeModel::new(vec![Ok(proposal(0.94, false))]));
    let settings = SettingsStore::new(temp.path().join("settings.json"));
    settings
        .save(&AppSettings {
            automatic_rename: true,
            ..AppSettings::default()
        })
        .unwrap();
    let pipeline = Pipeline::with_local_files(
        temp.path().join("queue.sqlite3"),
        worker,
        model,
        Arc::new(RecordingEvents::default()),
        settings,
    )
    .unwrap();
    pipeline.enqueue_files(&[original]).unwrap();
    pipeline.run_until_idle().unwrap();
    assert_eq!(pipeline.list().unwrap()[0].status, QueueStatus::Completed);

    // Keep original completes the duplicate without touching the disk.
    std::fs::write(temp.path().join("copy-two.pdf"), "keeper.pdf").unwrap();
    let second = temp.path().join("copy-two.pdf").canonicalize().unwrap();
    let kept = pipeline
        .enqueue_files(std::slice::from_ref(&second))
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(kept.status, QueueStatus::NeedsReview);
    pipeline.keep_original(kept.id).unwrap();
    assert!(second.exists());
    let kept_row = pipeline
        .list()
        .unwrap()
        .into_iter()
        .find(|item| item.id == kept.id)
        .unwrap();
    assert_eq!(kept_row.status, QueueStatus::Completed);
    assert_eq!(kept_row.error_code, None);

    // The next copy points at the most recent completion; a keep-original one
    // has no filed name, so its own filename is used.
    std::fs::write(temp.path().join("copy-three.pdf"), "keeper.pdf").unwrap();
    let third = temp.path().join("copy-three.pdf").canonicalize().unwrap();
    let flagged = pipeline.enqueue_files(&[third]).unwrap().pop().unwrap();
    assert_eq!(flagged.status, QueueStatus::NeedsReview);
    assert_eq!(flagged.error_code, Some(ErrorCode::Duplicate));
    let listed = pipeline
        .list()
        .unwrap()
        .into_iter()
        .find(|item| item.id == flagged.id)
        .unwrap();
    assert_eq!(listed.duplicate_of.as_deref(), Some("copy-two.pdf"));

    // Clearing the history removes the completed rows; the stale flag still
    // lists without a referent and remains removable.
    assert_eq!(pipeline.clear_history().unwrap(), 2);
    let stale = pipeline
        .list()
        .unwrap()
        .into_iter()
        .find(|item| item.id == flagged.id)
        .unwrap();
    assert_eq!(stale.status, QueueStatus::NeedsReview);
    assert_eq!(stale.duplicate_of, None);
    pipeline.remove(flagged.id).unwrap();
    assert!(pipeline.list().unwrap().is_empty());

    // With no completed rows left a fresh copy simply queues for analysis.
    std::fs::write(temp.path().join("copy-four.pdf"), "keeper.pdf").unwrap();
    let fourth = temp.path().join("copy-four.pdf").canonicalize().unwrap();
    let requeued = pipeline.enqueue_files(&[fourth]).unwrap().pop().unwrap();
    assert_eq!(requeued.status, QueueStatus::Queued);
    assert_eq!(requeued.error_code, None);
}
