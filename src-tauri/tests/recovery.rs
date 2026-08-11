use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use intern_app::{
    pipeline::{
        FileActions, ModelBoundary, ModelFailure, ParsedDocument, Pipeline, PipelineError,
        LeaseKeeper, PipelineEventSink, PipelineProgress, WorkerBoundary, WorkerFailure,
    },
    settings::SettingsStore,
};
use intern_core::{
    ErrorCode, ModelProposal, OperationReceipt, QueueItem, QueueStatus, QueueStore,
};
use tempfile::tempdir;
use std::time::Duration;
use rusqlite::Connection;

struct IdleWorker;
impl WorkerBoundary for IdleWorker {
    fn parse(
        &self,
        _request_id: &str,
        _path: &Path,
        _progress: &mut dyn FnMut(PipelineProgress),
    ) -> Result<ParsedDocument, WorkerFailure> {
        Err(WorkerFailure::new("PARSE_FAILED", false, false))
    }
    fn cancel(&self, _request_id: &str) -> Result<(), WorkerFailure> { Ok(()) }
    fn restart(&self) -> Result<(), WorkerFailure> { Ok(()) }
}

struct IdleModel;
impl ModelBoundary for IdleModel {
    fn propose(&self, _document: &intern_app::model::client::DocumentInput) -> Result<ModelProposal, ModelFailure> {
        Err(ModelFailure::fatal("MODEL_NOT_READY"))
    }
}

#[derive(Default)]
struct RecoveryFiles { reconciled: Mutex<Vec<i64>> }
impl FileActions for RecoveryFiles {
    fn fingerprint(&self, _path: &Path) -> Result<String, PipelineError> { Ok("hash".into()) }
    fn apply(&self, _item: &QueueItem, _destination: &Path) -> Result<(), PipelineError> { Ok(()) }
    fn undo(&self, _item: &QueueItem, _receipt: &OperationReceipt) -> Result<(), PipelineError> { Ok(()) }
    fn reconcile(&self, item: &QueueItem) -> Result<(), PipelineError> {
        self.reconciled.lock().unwrap().push(item.id);
        Ok(())
    }
}

struct NoEvents;
impl PipelineEventSink for NoEvents {
    fn queue_changed(&self) {}
    fn progress(&self, _progress: PipelineProgress) {}
}

#[test]
fn interrupted_analyzing_is_requeued_with_an_incremented_failure_count() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("interrupted.sqlite3");
    let id;
    {
        let store = QueueStore::open(&database).unwrap();
        let item = store.enqueue(Path::new("interrupted.pdf"), "hash").unwrap();
        id = item.id;
        store.claim_next().unwrap().unwrap();
        store.transition(id, QueueStatus::Extracting, QueueStatus::Analyzing, None).unwrap();
    }
    let pipeline = Pipeline::open(
        &database,
        Arc::new(IdleWorker),
        Arc::new(IdleModel),
        Arc::new(RecoveryFiles::default()),
        Arc::new(NoEvents),
        SettingsStore::new(temp.path().join("settings.json")),
    ).unwrap();

    pipeline.recover().unwrap();

    let item = pipeline.list().unwrap().into_iter().find(|item| item.id == id).unwrap();
    assert_eq!(item.status, QueueStatus::Queued);
    assert_eq!(item.processing_failures, 1);
}

#[test]
fn recovery_requeues_interrupted_processing_and_reconciles_applying_without_resetting_it() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("queue.sqlite3");
    let processing_id;
    let applying_id;
    {
        let store = QueueStore::open(&database).unwrap();
        let processing = store.enqueue(Path::new("processing.pdf"), "hash-1").unwrap();
        processing_id = processing.id;
        store.claim_next().unwrap().unwrap();
        store.transition(processing.id, QueueStatus::Extracting, QueueStatus::Analyzing, None).unwrap();
        store.transition(processing.id, QueueStatus::Analyzing, QueueStatus::Failed, Some(ErrorCode::IoError)).unwrap();
        store.manual_retry(processing.id).unwrap();
        store.claim_next().unwrap().unwrap();

        let applying = store.enqueue(Path::new("applying.pdf"), "hash-2").unwrap();
        applying_id = applying.id;
        // Release the active processing lease before constructing a durable Applying epoch.
        store.transition(processing.id, QueueStatus::Extracting, QueueStatus::Canceled, None).unwrap();
        store.claim_next().unwrap().unwrap();
        store.transition(applying.id, QueueStatus::Extracting, QueueStatus::Analyzing, None).unwrap();
        store.transition(applying.id, QueueStatus::Analyzing, QueueStatus::Ready, None).unwrap();
        store.begin_applying(applying.id, QueueStatus::Ready).unwrap();
    }

    let files = Arc::new(RecoveryFiles::default());
    let pipeline = Pipeline::open(
        &database,
        Arc::new(IdleWorker),
        Arc::new(IdleModel),
        Arc::clone(&files),
        Arc::new(NoEvents),
        SettingsStore::new(temp.path().join("settings.json")),
    ).unwrap();

    pipeline.recover().unwrap();

    let items = pipeline.list().unwrap();
    assert_eq!(items.iter().find(|item| item.id == processing_id).unwrap().status, QueueStatus::Canceled);
    assert_eq!(items.iter().find(|item| item.id == applying_id).unwrap().status, QueueStatus::Applying);
    assert_eq!(files.reconciled.lock().unwrap().as_slice(), &[applying_id]);
}

#[test]
fn periodic_lease_renewal_prevents_a_second_store_from_stealing_live_work() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("leases.sqlite3");
    let owner = Arc::new(QueueStore::open(&database).unwrap());
    let item = owner.enqueue(Path::new("long.pdf"), "hash").unwrap();
    owner.claim_next().unwrap().unwrap();
    let lease = LeaseKeeper::start(Arc::clone(&owner), item.id, Duration::from_millis(10)).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection.execute("UPDATE queue_items SET lease_expires_at = 0 WHERE id = ?1", [item.id]).unwrap();
    connection.execute("UPDATE queue_sessions SET heartbeat_at = 0 WHERE session_id = ?1", [owner.session_id()]).unwrap();
    std::thread::sleep(Duration::from_millis(40));
    let observer = QueueStore::open(&database).unwrap();

    lease.stop_and_check().unwrap();
    assert_eq!(observer.recover_interrupted().unwrap(), 0);
    assert!(observer.claim_next().unwrap().is_none());
}

#[test]
fn lease_renewal_loss_is_reported_to_the_active_operation() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("lost-lease.sqlite3");
    let owner = Arc::new(QueueStore::open(&database).unwrap());
    let item = owner.enqueue(Path::new("lost.pdf"), "hash").unwrap();
    owner.claim_next().unwrap().unwrap();
    let lease = LeaseKeeper::start(Arc::clone(&owner), item.id, Duration::from_millis(10)).unwrap();
    lease.check().unwrap();
    Connection::open(&database).unwrap().execute(
        "DELETE FROM queue_sessions WHERE session_id = ?1", [owner.session_id()],
    ).unwrap();
    std::thread::sleep(Duration::from_millis(40));

    let error = lease.stop_and_check().unwrap_err();
    assert_eq!(error.code, "STATE_CONFLICT");
}

#[test]
fn post_crash_recovery_waits_for_stale_deadline_then_requeues() {
    let temp = tempdir().unwrap();
    let database = temp.path().join("deferred.sqlite3");
    let crashed = QueueStore::open(&database).unwrap();
    let item = crashed.enqueue(Path::new("crashed.pdf"), "hash").unwrap();
    crashed.claim_next().unwrap().unwrap();
    let crashed_session = crashed.session_id().to_owned();
    std::mem::forget(crashed);
    let pipeline = Pipeline::open(
        &database,
        Arc::new(IdleWorker),
        Arc::new(IdleModel),
        Arc::new(RecoveryFiles::default()),
        Arc::new(NoEvents),
        SettingsStore::new(temp.path().join("settings.json")),
    ).unwrap();

    pipeline.recover().unwrap();
    assert_eq!(pipeline.list().unwrap()[0].status, QueueStatus::Extracting);
    let connection = Connection::open(&database).unwrap();
    connection.execute("UPDATE queue_items SET lease_expires_at = 0 WHERE id = ?1", [item.id]).unwrap();
    connection.execute("UPDATE queue_sessions SET heartbeat_at = 0 WHERE session_id = ?1", [crashed_session]).unwrap();

    pipeline.recover().unwrap();

    let recovered = pipeline.list().unwrap().pop().unwrap();
    assert_eq!(recovered.status, QueueStatus::Queued);
    assert_eq!(recovered.processing_failures, 1);
}
