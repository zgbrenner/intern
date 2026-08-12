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

use intern_app::{
    pipeline::{
        FileActions, ModelBoundary, ModelFailure, ParsedDocument, Pipeline, PipelineError,
        PipelineEventSink, PipelineProgress, WorkerBoundary, WorkerFailure,
    },
    settings::{AppSettings, SettingsStore},
};
use intern_core::{
    DateKind, ErrorCode, Evidence, ExtractedDocument, ModelProposal, OperationReceipt,
    ParserWarning, ProposalStatus, QueueItem, QueueStatus,
};
use rusqlite::Connection;
use tempfile::tempdir;

#[derive(Default)]
struct RecordingEvents {
    changed: AtomicUsize,
    progress: Mutex<Vec<PipelineProgress>>,
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
    responses: Mutex<VecDeque<Result<ParsedDocument, WorkerFailure>>>,
    active: AtomicUsize,
    maximum_active: AtomicUsize,
    cancellations: AtomicUsize,
    restarts: AtomicUsize,
    gate: (Mutex<bool>, Condvar),
}

impl FakeWorker {
    fn new(responses: Vec<Result<ParsedDocument, WorkerFailure>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            active: AtomicUsize::new(0),
            maximum_active: AtomicUsize::new(0),
            cancellations: AtomicUsize::new(0),
            restarts: AtomicUsize::new(0),
            gate: (Mutex::new(false), Condvar::new()),
        }
    }

    fn blocking(response: Result<ParsedDocument, WorkerFailure>) -> Self {
        let worker = Self::new(vec![response]);
        *worker.gate.0.lock().unwrap() = true;
        worker
    }
}

impl WorkerBoundary for FakeWorker {
    fn parse(
        &self,
        _request_id: &str,
        _path: &Path,
        _progress: &mut dyn FnMut(PipelineProgress),
    ) -> Result<ParsedDocument, WorkerFailure> {
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

impl ModelBoundary for FakeModel {
    fn propose(
        &self,
        _document: &intern_app::model::client::DocumentInput,
    ) -> Result<ModelProposal, ModelFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(ModelFailure::fatal("MODEL_RESPONSE_INVALID")))
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

impl ModelBoundary for RecoveringModel {
    fn propose(
        &self,
        _document: &intern_app::model::client::DocumentInput,
    ) -> Result<ModelProposal, ModelFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.responses.lock().unwrap().pop_front().unwrap()
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

impl ModelBoundary for GatedSuccessModel {
    fn propose(
        &self,
        _document: &intern_app::model::client::DocumentInput,
    ) -> Result<ModelProposal, ModelFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (lock, wake) = &self.gate;
        let mut blocked = lock.lock().unwrap();
        while *blocked {
            blocked = wake.wait(blocked).unwrap();
        }
        Ok(proposal(0.94, false))
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
        // Generous deadline: the poll exits as soon as cancel starts, but a
        // cold, loaded CI runner can take well over two seconds to get there.
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

impl ModelBoundary for BlockingModel {
    fn propose(
        &self,
        _document: &intern_app::model::client::DocumentInput,
    ) -> Result<ModelProposal, ModelFailure> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 1 {
            let (lock, wake) = &self.request_release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
            return Err(ModelFailure::fatal("MODEL_REQUEST_CANCELED"));
        }
        Ok(proposal(0.94, false))
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

fn parsed(text: &str) -> ParsedDocument {
    ParsedDocument {
        extracted: ExtractedDocument {
            text: text.to_owned(),
            parser_warnings: vec![],
        },
        image: None,
    }
}

fn proposal(confidence: f32, needs_review: bool) -> ModelProposal {
    ModelProposal {
        document_date: Some("2024-04-12".into()),
        date_kind: Some(DateKind::Signed),
        document_type: Some("Employment Agreement".into()),
        filename_subject: Some("John Smith".into()),
        parties: vec!["John Smith".into(), "Acme Corporation".into()],
        description: "Employment agreement between John Smith and Acme Corporation.".into(),
        confidence,
        needs_review,
        review_reasons: if needs_review {
            vec!["uncertain".into()]
        } else {
            vec![]
        },
        evidence: Evidence {
            date: Some("signed April 12, 2024".into()),
            document_type: Some("Employment Agreement".into()),
            subject: Some("John Smith".into()),
            parties: vec!["John Smith".into(), "Acme Corporation".into()],
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
    document.extracted.parser_warnings.push(ParserWarning {
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
    let pipeline = Pipeline::with_local_files(
        temp.path().join("real-queue.sqlite3"),
        worker,
        model,
        Arc::new(RecordingEvents::default()),
        settings,
    )
    .unwrap();
    pipeline.enqueue_files(&[path.clone()]).unwrap();

    pipeline.run_until_idle().unwrap();

    let completed = pipeline.list().unwrap().pop().unwrap();
    assert_eq!(completed.status, QueueStatus::Completed);
    let receipt = completed.receipt.unwrap();
    assert!(!receipt.source.exists());
    assert!(receipt.destination.exists());

    pipeline.undo(completed.id).unwrap();

    assert_eq!(pipeline.list().unwrap()[0].status, QueueStatus::Ready);
    assert!(path.exists());
    assert!(!receipt.destination.exists());
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
