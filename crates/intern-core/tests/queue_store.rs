use std::{
    fs,
    path::Path,
    sync::{Arc, Barrier},
    thread,
};

use intern_core::{
    ErrorCode, FileApplier, FileSystem, HISTORY_LIMIT, OperationDirection, OperationKind,
    OperationStage, QueueStatus, QueueStore, StdFileSystem, source_path_key,
};
use tempfile::TempDir;

fn store(temp: &TempDir) -> QueueStore {
    QueueStore::open(temp.path().join("queue.sqlite3")).unwrap()
}

fn advance_to_ready(db: &QueueStore, id: i64) {
    assert_eq!(db.claim_next().unwrap().unwrap().id, id);
    db.transition(id, QueueStatus::Extracting, QueueStatus::Analyzing, None)
        .unwrap();
    db.transition(id, QueueStatus::Analyzing, QueueStatus::Ready, None)
        .unwrap();
}

/// Enqueues `source` and completes it through a real journalled apply so the
/// completed row carries an apply/complete receipt naming `destination`.
fn complete_via_apply(db: &Arc<QueueStore>, source: &Path, destination: &Path) -> i64 {
    let hash = StdFileSystem.hash(source).unwrap();
    let item = db.enqueue(source, &hash).unwrap();
    advance_to_ready(db, item.id);
    db.begin_applying(item.id, QueueStatus::Ready).unwrap();
    let receipt = FileApplier::local(Arc::clone(db))
        .apply(item.id, source, destination, &hash)
        .unwrap();
    db.complete_apply(item.id, receipt.id).unwrap();
    item.id
}

#[test]
fn duplicate_unchanged_path_focuses_existing_item_but_same_hash_at_new_path_enqueues() {
    let temp = TempDir::new().unwrap();
    let db = store(&temp);
    let first = db.enqueue(Path::new("C:/docs/a.pdf"), "same-hash").unwrap();
    let duplicate = db.enqueue(Path::new("C:/docs/a.pdf"), "same-hash").unwrap();
    let case_variant = db
        .enqueue(Path::new("c:\\DOCS\\A.PDF"), "same-hash")
        .unwrap();
    let second_path = db.enqueue(Path::new("C:/docs/b.pdf"), "same-hash").unwrap();
    assert_eq!(first.id, duplicate.id);
    assert_eq!(first.id, case_variant.id);
    assert_ne!(first.id, second_path.id);
    assert_eq!(db.list().unwrap().len(), 2);
}

#[test]
fn exactly_one_concurrent_claimant_wins() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("queue.sqlite3");
    QueueStore::open(&path)
        .unwrap()
        .enqueue(Path::new("one.pdf"), "h1")
        .unwrap();
    QueueStore::open(&path)
        .unwrap()
        .enqueue(Path::new("two.pdf"), "h2")
        .unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let db = QueueStore::open(path).unwrap();
                barrier.wait();
                db.claim_next().unwrap()
            })
        })
        .collect();
    barrier.wait();
    let claims = handles
        .into_iter()
        .filter_map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].status, QueueStatus::Extracting);
}

#[test]
fn invalid_transition_has_stable_code_and_does_not_change_state() {
    let temp = TempDir::new().unwrap();
    let db = store(&temp);
    let item = db.enqueue(Path::new("one.pdf"), "h1").unwrap();
    let err = db
        .transition(item.id, QueueStatus::Queued, QueueStatus::Completed, None)
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidTransition);
    assert_eq!(db.list().unwrap()[0].status, QueueStatus::Queued);
}

#[test]
fn recovery_requeues_processing_but_leaves_applying_for_reconciliation() {
    for interrupted_status in [QueueStatus::Extracting, QueueStatus::Analyzing] {
        let temp = TempDir::new().unwrap();
        let db = store(&temp);
        db.enqueue(Path::new("processing.pdf"), "hp").unwrap();
        let item = db.claim_next().unwrap().unwrap();
        if interrupted_status == QueueStatus::Analyzing {
            db.transition(
                item.id,
                QueueStatus::Extracting,
                QueueStatus::Analyzing,
                None,
            )
            .unwrap();
        }
        drop(db);
        let db = store(&temp);
        assert_eq!(db.recover_interrupted().unwrap(), 1);
        assert_eq!(db.list().unwrap()[0].status, QueueStatus::Queued);
    }

    let temp = TempDir::new().unwrap();
    let db = store(&temp);
    db.enqueue(Path::new("applying.pdf"), "ha").unwrap();
    db.enqueue(Path::new("waiting.pdf"), "hw").unwrap();
    let applying = db.claim_next().unwrap().unwrap();
    db.transition(
        applying.id,
        QueueStatus::Extracting,
        QueueStatus::Analyzing,
        None,
    )
    .unwrap();
    db.transition(
        applying.id,
        QueueStatus::Analyzing,
        QueueStatus::Ready,
        None,
    )
    .unwrap();
    db.begin_applying(applying.id, QueueStatus::Ready).unwrap();
    drop(db);
    let db = store(&temp);
    assert_eq!(db.recover_interrupted().unwrap(), 0);
    assert_eq!(db.list().unwrap()[0].status, QueueStatus::Applying);
    assert!(db.claim_next().unwrap().is_none());
    db.claim_applying_reconciliation(applying.id).unwrap();
    let db = Arc::new(db);
    assert_eq!(
        FileApplier::local(db.clone())
            .reconcile(applying.id)
            .unwrap()
            .status,
        QueueStatus::Ready
    );
    assert_eq!(
        db.claim_next().unwrap().unwrap().source_path,
        Path::new("waiting.pdf")
    );
}

#[test]
fn live_owner_cannot_be_stolen_but_closed_owner_can_be_recovered() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("queue.sqlite3");
    let owner = QueueStore::open(&path).unwrap();
    owner.enqueue(Path::new("active.pdf"), "ha").unwrap();
    let active = owner.claim_next().unwrap().unwrap();
    let observer = QueueStore::open(&path).unwrap();
    assert_eq!(observer.recover_interrupted().unwrap(), 0);
    assert_eq!(observer.list().unwrap()[0].status, QueueStatus::Extracting);
    drop(owner);
    assert_eq!(observer.recover_interrupted().unwrap(), 1);
    assert_eq!(observer.list().unwrap()[0].id, active.id);
    assert_eq!(observer.list().unwrap()[0].status, QueueStatus::Queued);
}

#[test]
fn expired_item_lease_does_not_override_a_fresh_owner_heartbeat() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("queue.sqlite3");
    let owner = QueueStore::open(&path).unwrap();
    owner.enqueue(Path::new("active.pdf"), "ha").unwrap();
    let active = owner.claim_next().unwrap().unwrap();
    let observer = QueueStore::open(&path).unwrap();
    let inspector = rusqlite::Connection::open(&path).unwrap();
    inspector
        .execute(
            "UPDATE queue_items SET lease_expires_at = 0 WHERE id = ?1",
            [active.id],
        )
        .unwrap();

    assert_eq!(observer.recover_interrupted().unwrap(), 0);
    assert_eq!(observer.list().unwrap()[0].status, QueueStatus::Extracting);
    assert_eq!(
        owner.renew_lease(active.id).unwrap().status,
        QueueStatus::Extracting
    );
}

#[test]
fn automatic_processing_retries_stop_after_two_failures() {
    let temp = TempDir::new().unwrap();
    let db = store(&temp);
    let item = db.enqueue(Path::new("a.pdf"), "ha").unwrap();
    for expected_attempt in 1..=2 {
        let claimed = db.claim_next().unwrap().unwrap();
        assert_eq!(claimed.id, item.id);
        let status = db
            .record_processing_failure(item.id, ErrorCode::ModelOutputInvalid)
            .unwrap();
        assert_eq!(
            status,
            if expected_attempt == 1 {
                QueueStatus::Queued
            } else {
                QueueStatus::Failed
            }
        );
    }
    assert!(db.claim_next().unwrap().is_none());
}

#[test]
fn clear_terminal_removes_only_terminal_rows() {
    let temp = TempDir::new().unwrap();
    let db = store(&temp);
    let active = db.enqueue(Path::new("active.pdf"), "a").unwrap();
    let failed = db.enqueue(Path::new("failed.pdf"), "f").unwrap();
    db.transition(failed.id, QueueStatus::Queued, QueueStatus::Canceled, None)
        .unwrap();
    assert_eq!(db.clear_terminal().unwrap(), 1);
    assert_eq!(db.list().unwrap()[0].id, active.id);
}

/// Pointing the queue at the wrong folder must be recoverable, and recovering
/// must not cost a rename the user can no longer undo. Only rows that never
/// started are dropped.
#[test]
fn discard_queued_drops_waiting_work_and_nothing_else() {
    let temp = TempDir::new().unwrap();
    let db = store(&temp);

    // Enqueued in claim order, because claim_next takes the oldest and the queue
    // holds exactly one active claim - it processes one document at a time. The
    // review item therefore has to reach a resting state before the next claim.
    let awaiting_decision = db.enqueue(Path::new("review.pdf"), "r").unwrap();
    let in_flight = db.enqueue(Path::new("in-flight.pdf"), "f").unwrap();
    let canceled = db.enqueue(Path::new("canceled.pdf"), "c").unwrap();
    let waiting_one = db.enqueue(Path::new("waiting-one.pdf"), "w1").unwrap();
    let waiting_two = db.enqueue(Path::new("waiting-two.pdf"), "w2").unwrap();

    assert_eq!(db.claim_next().unwrap().unwrap().id, awaiting_decision.id);
    db.transition(
        awaiting_decision.id,
        QueueStatus::Extracting,
        QueueStatus::Analyzing,
        None,
    )
    .unwrap();
    db.transition(
        awaiting_decision.id,
        QueueStatus::Analyzing,
        QueueStatus::NeedsReview,
        None,
    )
    .unwrap();
    // Only now is the single active slot free for the next claim.
    assert_eq!(db.claim_next().unwrap().unwrap().id, in_flight.id);
    db.transition(
        canceled.id,
        QueueStatus::Queued,
        QueueStatus::Canceled,
        None,
    )
    .unwrap();

    assert_eq!(db.discard_queued().unwrap(), 2);

    let remaining: Vec<i64> = db.list().unwrap().into_iter().map(|item| item.id).collect();
    assert!(!remaining.contains(&waiting_one.id));
    assert!(!remaining.contains(&waiting_two.id));
    // Mid-flight work belongs to a session and has to reach its own end; the
    // review is a human's to decide, and terminal rows carry rename receipts.
    assert!(remaining.contains(&in_flight.id));
    assert!(remaining.contains(&awaiting_decision.id));
    assert!(remaining.contains(&canceled.id));

    // Idempotent: nothing left to discard.
    assert_eq!(db.discard_queued().unwrap(), 0);
}

#[test]
fn explicit_manual_retry_and_keep_original_cas_paths_are_enforced() {
    let temp = TempDir::new().unwrap();
    let db = store(&temp);

    let retry = db.enqueue(Path::new("retry.pdf"), "hr").unwrap();
    for _ in 0..2 {
        db.claim_next().unwrap().unwrap();
        db.record_processing_failure(retry.id, ErrorCode::ModelOutputInvalid)
            .unwrap();
    }
    assert_eq!(
        db.manual_retry(retry.id).unwrap().status,
        QueueStatus::Queued
    );
    assert_eq!(
        db.manual_retry(retry.id).unwrap_err().code(),
        ErrorCode::StateConflict
    );

    let keep = db.enqueue(Path::new("keep.pdf"), "hk").unwrap();
    let claimed = db.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, retry.id);
    db.transition(
        retry.id,
        QueueStatus::Extracting,
        QueueStatus::Canceled,
        None,
    )
    .unwrap();
    let claimed = db.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, keep.id);
    db.transition(
        keep.id,
        QueueStatus::Extracting,
        QueueStatus::Analyzing,
        None,
    )
    .unwrap();
    db.transition(keep.id, QueueStatus::Analyzing, QueueStatus::Ready, None)
        .unwrap();
    assert_eq!(
        db.complete_keep_original(keep.id, QueueStatus::Ready)
            .unwrap()
            .status,
        QueueStatus::Completed
    );
}

#[test]
fn legacy_schema_migrates_without_discarding_queue_or_receipt_rows() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("legacy.sqlite3");
    let legacy = rusqlite::Connection::open(&path).unwrap();
    legacy
        .execute_batch(
            "PRAGMA foreign_keys=ON;
         CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
         CREATE TABLE queue_items(
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           source_path TEXT NOT NULL,
           source_path_key TEXT NOT NULL,
           source_hash TEXT NOT NULL,
           status TEXT NOT NULL,
           processing_failures INTEGER NOT NULL DEFAULT 0,
           error_code TEXT,
           created_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL,
           UNIQUE(source_path_key, source_hash)
         );
         CREATE TABLE operation_receipts(
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           queue_item_id INTEGER NOT NULL REFERENCES queue_items(id) ON DELETE CASCADE,
           receipt_json TEXT NOT NULL,
           created_at INTEGER NOT NULL
         );
         INSERT INTO schema_migrations VALUES(1, 1);
         INSERT INTO queue_items(
           source_path, source_path_key, source_hash, status, created_at, updated_at
         ) VALUES('legacy.pdf', 'legacy.pdf', 'hash', 'queued', 1, 1);
         INSERT INTO operation_receipts(queue_item_id, receipt_json, created_at)
           VALUES(1, '{\"legacy\":true}', 1);",
        )
        .unwrap();
    drop(legacy);

    let migrated = QueueStore::open(&path).unwrap();
    assert_eq!(
        migrated.list().unwrap()[0].source_path,
        Path::new("legacy.pdf")
    );
    drop(migrated);
    let inspected = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        inspected
            .query_row(
                "SELECT receipt_json FROM operation_receipts_legacy_v1 WHERE id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "{\"legacy\":true}",
    );
}

#[test]
fn v2_duplicate_nonterminal_receipts_open_unbound_and_fail_closed() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("v2-duplicates.sqlite3");
    let legacy = rusqlite::Connection::open(&path).unwrap();
    legacy.execute_batch(
        "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
         CREATE TABLE queue_items(
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           source_path TEXT NOT NULL,
           source_path_key TEXT NOT NULL,
           source_hash TEXT NOT NULL,
           status TEXT NOT NULL,
           processing_failures INTEGER NOT NULL DEFAULT 0,
           error_code TEXT,
           owner_session TEXT,
           lease_expires_at INTEGER,
           previous_status TEXT,
           reconciliation_receipt_id INTEGER,
           created_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL,
           UNIQUE(source_path_key, source_hash)
         );
         CREATE TABLE operation_receipts(
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           queue_item_id INTEGER NOT NULL REFERENCES queue_items(id) ON DELETE CASCADE,
           direction TEXT NOT NULL,
           source_path TEXT NOT NULL,
           destination_path TEXT NOT NULL,
           temporary_path TEXT,
           pre_hash TEXT NOT NULL,
           post_hash TEXT,
           operation_kind TEXT NOT NULL,
           stage TEXT NOT NULL,
           source_exists INTEGER NOT NULL,
           destination_exists INTEGER NOT NULL,
           temporary_exists INTEGER NOT NULL,
           created_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL
         );
         INSERT INTO schema_migrations VALUES(2, 1);
         INSERT INTO queue_items(
           source_path, source_path_key, source_hash, status, previous_status, created_at, updated_at
         ) VALUES('source.pdf', 'source.pdf', 'hash', 'applying', 'ready', 1, 1);
         INSERT INTO operation_receipts(
           queue_item_id, direction, source_path, destination_path, pre_hash,
           operation_kind, stage, source_exists, destination_exists, temporary_exists,
           created_at, updated_at
         ) VALUES
           (1, 'apply', 'source.pdf', 'one.pdf', 'hash', 'rename', 'planned', 1, 0, 0, 1, 1),
           (1, 'apply', 'source.pdf', 'two.pdf', 'hash', 'rename', 'planned', 1, 0, 0, 1, 1);",
    ).unwrap();
    drop(legacy);

    let migrated = Arc::new(QueueStore::open(&path).unwrap());
    let items = migrated.list().unwrap();
    let item = &items[0];
    assert_eq!(item.status, QueueStatus::Applying);
    assert_eq!(item.active_receipt_id, None);
    migrated.claim_applying_reconciliation(item.id).unwrap();
    assert_eq!(
        FileApplier::local(migrated.clone())
            .reconcile(item.id)
            .unwrap_err()
            .code(),
        ErrorCode::StateConflict,
    );
    assert_eq!(migrated.list().unwrap()[0].status, QueueStatus::Applying);
    drop(migrated);
    let inspected = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        inspected
            .query_row("SELECT COUNT(*) FROM operation_receipts", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2,
    );
}

fn create_v2_complete_epoch(
    database: &Path,
    queue_source: &Path,
    receipt_source: &Path,
    receipt_destination: &Path,
    direction: &str,
    previous_status: &str,
    hash: &str,
) {
    let connection = rusqlite::Connection::open(database).unwrap();
    connection.execute_batch(
        "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
         CREATE TABLE queue_items(
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           source_path TEXT NOT NULL,
           source_path_key TEXT NOT NULL,
           source_hash TEXT NOT NULL,
           status TEXT NOT NULL,
           processing_failures INTEGER NOT NULL DEFAULT 0,
           error_code TEXT,
           owner_session TEXT,
           lease_expires_at INTEGER,
           previous_status TEXT,
           reconciliation_receipt_id INTEGER,
           created_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL,
           UNIQUE(source_path_key, source_hash)
         );
         CREATE TABLE operation_receipts(
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           queue_item_id INTEGER NOT NULL REFERENCES queue_items(id) ON DELETE CASCADE,
           direction TEXT NOT NULL,
           source_path TEXT NOT NULL,
           destination_path TEXT NOT NULL,
           temporary_path TEXT,
           pre_hash TEXT NOT NULL,
           post_hash TEXT,
           operation_kind TEXT NOT NULL,
           stage TEXT NOT NULL,
           source_exists INTEGER NOT NULL,
           destination_exists INTEGER NOT NULL,
           temporary_exists INTEGER NOT NULL,
           created_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL
         );
         INSERT INTO schema_migrations VALUES(2, 1);",
    ).unwrap();
    let queue_source = queue_source.to_string_lossy().into_owned();
    let receipt_source = receipt_source.to_string_lossy().into_owned();
    let receipt_destination = receipt_destination.to_string_lossy().into_owned();
    connection.execute(
        "INSERT INTO queue_items(
           source_path, source_path_key, source_hash, status, previous_status, created_at, updated_at
         ) VALUES(?1, ?1, ?2, 'applying', ?3, 1, 1)",
        rusqlite::params![queue_source, hash, previous_status],
    ).unwrap();
    connection
        .execute(
            "INSERT INTO operation_receipts(
           queue_item_id, direction, source_path, destination_path, pre_hash, post_hash,
           operation_kind, stage, source_exists, destination_exists, temporary_exists,
           created_at, updated_at
         ) VALUES(1, ?1, ?2, ?3, ?4, ?4, 'rename', 'complete', 0, 1, 0, 1, 1)",
            rusqlite::params![direction, receipt_source, receipt_destination, hash],
        )
        .unwrap();
}

fn insert_v2_complete_receipt(
    database: &Path,
    source: &Path,
    destination: &Path,
    direction: &str,
    hash: &str,
) {
    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .execute(
            "INSERT INTO operation_receipts(
           queue_item_id, direction, source_path, destination_path, pre_hash, post_hash,
           operation_kind, stage, source_exists, destination_exists, temporary_exists,
           created_at, updated_at
         ) VALUES(1, ?1, ?2, ?3, ?4, ?4, 'rename', 'complete', 0, 1, 0, 1, 1)",
            rusqlite::params![
                direction,
                source.to_string_lossy().into_owned(),
                destination.to_string_lossy().into_owned(),
                hash,
            ],
        )
        .unwrap();
}

#[test]
fn v2_complete_apply_receipt_binds_and_reconciles_after_migration() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("v2-apply-complete.sqlite3");
    let original = temp.path().join("source.pdf");
    let published = temp.path().join("named.pdf");
    fs::write(&published, b"original").unwrap();
    let hash = StdFileSystem.hash(&published).unwrap();
    create_v2_complete_epoch(
        &database, &original, &original, &published, "apply", "ready", &hash,
    );

    let store = Arc::new(QueueStore::open(database).unwrap());
    let item = store.list().unwrap().into_iter().next().unwrap();
    assert_eq!(item.active_receipt_id, Some(1));
    store.claim_applying_reconciliation(item.id).unwrap();
    let resolved = FileApplier::local(store).reconcile(item.id).unwrap();
    assert_eq!(resolved.status, QueueStatus::Completed);
    assert!(!original.exists());
    assert!(published.exists());
}

#[test]
fn v2_complete_undo_receipt_binds_and_reconciles_after_migration() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("v2-undo-complete.sqlite3");
    let original = temp.path().join("source.pdf");
    let published = temp.path().join("named.pdf");
    fs::write(&original, b"original").unwrap();
    let hash = StdFileSystem.hash(&original).unwrap();
    create_v2_complete_epoch(
        &database,
        &original,
        &published,
        &original,
        "undo",
        "completed",
        &hash,
    );

    let store = Arc::new(QueueStore::open(database).unwrap());
    let item = store.list().unwrap().into_iter().next().unwrap();
    assert_eq!(item.active_receipt_id, Some(1));
    store.claim_applying_reconciliation(item.id).unwrap();
    let resolved = FileApplier::local(store).reconcile(item.id).unwrap();
    assert_eq!(resolved.status, QueueStatus::Ready);
    assert!(original.exists());
    assert!(!published.exists());
}

#[test]
fn v2_empty_second_apply_does_not_bind_historical_first_apply_receipt() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("v2-empty-apply-two.sqlite3");
    let original = temp.path().join("source.pdf");
    let first_published = temp.path().join("first.pdf");
    fs::write(&original, b"original").unwrap();
    let hash = StdFileSystem.hash(&original).unwrap();

    create_v2_complete_epoch(
        &database,
        &original,
        &original,
        &first_published,
        "apply",
        "ready",
        &hash,
    );
    insert_v2_complete_receipt(&database, &first_published, &original, "undo", &hash);

    let store = Arc::new(QueueStore::open(database).unwrap());
    let item = store.list().unwrap().into_iter().next().unwrap();
    assert_eq!(item.active_receipt_id, None);
    store.claim_applying_reconciliation(item.id).unwrap();
    assert_eq!(
        FileApplier::local(store.clone())
            .reconcile(item.id)
            .unwrap_err()
            .code(),
        ErrorCode::StateConflict,
    );
    assert_eq!(store.list().unwrap()[0].status, QueueStatus::Applying);
    assert!(original.exists());
    assert!(!first_published.exists());
}

#[test]
fn v2_empty_second_undo_does_not_bind_historical_first_undo_receipt() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("v2-empty-undo-two.sqlite3");
    let original = temp.path().join("source.pdf");
    let first_published = temp.path().join("first.pdf");
    let second_published = temp.path().join("second.pdf");
    fs::write(&second_published, b"original").unwrap();
    let hash = StdFileSystem.hash(&second_published).unwrap();

    create_v2_complete_epoch(
        &database,
        &original,
        &original,
        &first_published,
        "apply",
        "completed",
        &hash,
    );
    insert_v2_complete_receipt(&database, &first_published, &original, "undo", &hash);
    insert_v2_complete_receipt(&database, &original, &second_published, "apply", &hash);

    let store = Arc::new(QueueStore::open(database).unwrap());
    let item = store.list().unwrap().into_iter().next().unwrap();
    assert_eq!(item.active_receipt_id, None);
    store.claim_applying_reconciliation(item.id).unwrap();
    assert_eq!(
        FileApplier::local(store.clone())
            .reconcile(item.id)
            .unwrap_err()
            .code(),
        ErrorCode::StateConflict,
    );
    assert_eq!(store.list().unwrap()[0].status, QueueStatus::Applying);
    assert!(!original.exists());
    assert!(second_published.exists());
}

#[test]
fn find_completed_duplicate_reports_the_filed_name_and_skips_pending_or_undone_items() {
    let temp = TempDir::new().unwrap();
    let db = Arc::new(store(&temp));
    let original = temp.path().join("original.pdf");
    fs::write(&original, b"same-content").unwrap();
    let hash = StdFileSystem.hash(&original).unwrap();
    let filed = temp.path().join("2024 - Filed Agreement.pdf");
    let completed_id = complete_via_apply(&db, &original, &filed);

    // A pending twin holding the same content has been filed nowhere yet.
    let pending = temp.path().join("pending.pdf");
    fs::write(&pending, b"same-content").unwrap();
    db.enqueue(&pending, &hash).unwrap();

    let incoming_key = source_path_key(&temp.path().join("incoming.pdf"));
    let found = db
        .find_completed_duplicate(&hash, &incoming_key)
        .unwrap()
        .unwrap();
    assert_eq!(found.queue_item_id, completed_id);
    assert_eq!(
        found.filed_as.as_deref(),
        Some("2024 - Filed Agreement.pdf")
    );
    assert_eq!(found.source_path, original);

    // The completed item's own path is excluded: re-adding the same file at
    // the same place is the existing same-item dedupe, not a duplicate flag.
    assert!(
        db.find_completed_duplicate(&hash, &source_path_key(&original))
            .unwrap()
            .is_none()
    );

    // Undo returns the content home and the item to Ready; it no longer
    // counts as a filed duplicate.
    db.begin_applying(completed_id, QueueStatus::Completed)
        .unwrap();
    let receipt = db.load_receipt(completed_id).unwrap().unwrap();
    let undo = FileApplier::local(Arc::clone(&db))
        .undo(completed_id, &receipt)
        .unwrap();
    db.complete_undo(completed_id, undo.id).unwrap();
    assert!(
        db.find_completed_duplicate(&hash, &incoming_key)
            .unwrap()
            .is_none()
    );
}

#[test]
fn most_recent_completed_duplicate_wins_and_keep_original_reports_no_filed_name() {
    let temp = TempDir::new().unwrap();
    let db = Arc::new(store(&temp));
    let first = temp.path().join("first.pdf");
    fs::write(&first, b"shared-content").unwrap();
    let hash = StdFileSystem.hash(&first).unwrap();
    complete_via_apply(&db, &first, &temp.path().join("First Filed.pdf"));

    let second = temp.path().join("second.pdf");
    fs::write(&second, b"shared-content").unwrap();
    let kept = db.enqueue(&second, &hash).unwrap();
    advance_to_ready(&db, kept.id);
    db.complete_keep_original(kept.id, QueueStatus::Ready)
        .unwrap();

    let found = db
        .find_completed_duplicate(&hash, &source_path_key(&temp.path().join("third.pdf")))
        .unwrap()
        .unwrap();
    assert_eq!(found.queue_item_id, kept.id);
    assert_eq!(found.filed_as, None);
    assert_eq!(found.source_path, second);
}

#[test]
fn operation_history_lists_finished_work_newest_first_and_hides_in_flight_receipts() {
    let temp = TempDir::new().unwrap();
    let db = Arc::new(store(&temp));
    let original = temp.path().join("scan.pdf");
    fs::write(&original, b"content").unwrap();
    let filed = temp.path().join("2024-04-12 Employment Agreement.pdf");
    let item_id = complete_via_apply(&db, &original, &filed);

    // Undoing the rename records a second, newer terminal receipt.
    db.begin_applying(item_id, QueueStatus::Completed).unwrap();
    let applied = db.load_receipt(item_id).unwrap().unwrap();
    let undo = FileApplier::local(Arc::clone(&db))
        .undo(item_id, &applied)
        .unwrap();
    db.complete_undo(item_id, undo.id).unwrap();

    // A receipt still mid-operation is applier bookkeeping, not history.
    let pending = db
        .enqueue(Path::new("in-flight.pdf"), "in-flight-hash")
        .unwrap();
    let inspector = rusqlite::Connection::open(temp.path().join("queue.sqlite3")).unwrap();
    inspector
        .execute(
            "INSERT INTO operation_receipts(
               queue_item_id, direction, source_path, destination_path, pre_hash,
               operation_kind, stage, source_exists, destination_exists, temporary_exists,
               created_at, updated_at
             ) VALUES(?1, 'apply', 'in-flight.pdf', 'renamed.pdf', 'in-flight-hash',
                      'rename', 'published', 0, 1, 0, 9999999999, 9999999999)",
            [pending.id],
        )
        .unwrap();

    let history = db.list_operation_history(10).unwrap();
    assert_eq!(history.len(), 2);
    let newest = &history[0];
    assert_eq!(newest.receipt_id, undo.id);
    assert_eq!(newest.queue_item_id, item_id);
    assert_eq!(newest.direction, OperationDirection::Undo);
    assert_eq!(newest.stage, OperationStage::Complete);
    assert_eq!(newest.original_path, filed);
    assert_eq!(newest.new_path, original);
    let earlier = &history[1];
    assert_eq!(earlier.queue_item_id, item_id);
    assert_eq!(earlier.direction, OperationDirection::Apply);
    assert_eq!(earlier.kind, OperationKind::Rename);
    assert_eq!(earlier.stage, OperationStage::Complete);
    assert_eq!(earlier.original_path, original);
    assert_eq!(earlier.new_path, filed);
    assert!(newest.at >= earlier.at);
}

#[test]
fn operation_history_honors_the_requested_limit_and_caps_at_five_hundred() {
    let temp = TempDir::new().unwrap();
    let db = store(&temp);
    let item = db.enqueue(Path::new("bulk.pdf"), "bulk-hash").unwrap();
    let inspector = rusqlite::Connection::open(temp.path().join("queue.sqlite3")).unwrap();
    let mut insert = inspector
        .prepare(
            "INSERT INTO operation_receipts(
               queue_item_id, direction, source_path, destination_path, pre_hash,
               operation_kind, stage, source_exists, destination_exists, temporary_exists,
               created_at, updated_at
             ) VALUES(?1, 'apply', 'bulk.pdf', 'renamed.pdf', 'bulk-hash',
                      'rename', 'complete', 0, 1, 0, ?2, ?2)",
        )
        .unwrap();
    let total = HISTORY_LIMIT + 5;
    for moment in 0..total {
        insert
            .execute(rusqlite::params![item.id, moment as i64])
            .unwrap();
    }

    assert_eq!(db.list_operation_history(3).unwrap().len(), 3);
    let capped = db.list_operation_history(total + 100).unwrap();
    assert_eq!(capped.len(), HISTORY_LIMIT);
    assert_eq!(capped[0].at, (total - 1) as i64);
    assert!(capped.windows(2).all(|pair| pair[0].at >= pair[1].at));
}

#[test]
fn duplicate_flag_and_retry_are_compare_and_swap_guarded() {
    let temp = TempDir::new().unwrap();
    let db = store(&temp);
    let item = db.enqueue(Path::new("dup.pdf"), "h1").unwrap();

    let flagged = db
        .transition(
            item.id,
            QueueStatus::Queued,
            QueueStatus::NeedsReview,
            Some(ErrorCode::Duplicate),
        )
        .unwrap();
    assert_eq!(flagged.status, QueueStatus::NeedsReview);
    assert_eq!(flagged.error_code, Some(ErrorCode::Duplicate));

    let retried = db.retry_duplicate(item.id).unwrap();
    assert_eq!(retried.status, QueueStatus::Queued);
    assert_eq!(retried.error_code, None);

    // Once a session claims the item, the flag loses the race and the claim
    // is left untouched.
    assert_eq!(db.claim_next().unwrap().unwrap().id, item.id);
    let conflict = db
        .transition(
            item.id,
            QueueStatus::Queued,
            QueueStatus::NeedsReview,
            Some(ErrorCode::Duplicate),
        )
        .unwrap_err();
    assert_eq!(conflict.code(), ErrorCode::StateConflict);
    assert_eq!(db.list().unwrap()[0].status, QueueStatus::Extracting);

    // A review item that is not a duplicate cannot take the requeue shortcut.
    db.transition(
        item.id,
        QueueStatus::Extracting,
        QueueStatus::Analyzing,
        None,
    )
    .unwrap();
    db.transition(
        item.id,
        QueueStatus::Analyzing,
        QueueStatus::NeedsReview,
        None,
    )
    .unwrap();
    assert_eq!(
        db.retry_duplicate(item.id).unwrap_err().code(),
        ErrorCode::StateConflict
    );
}
