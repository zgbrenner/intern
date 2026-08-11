use std::{fs, path::Path, sync::{Arc, Barrier}, thread};

use intern_core::{ErrorCode, FileApplier, FileSystem, QueueStatus, QueueStore, StdFileSystem};
use tempfile::TempDir;

fn store(temp: &TempDir) -> QueueStore {
    QueueStore::open(temp.path().join("queue.sqlite3")).unwrap()
}

#[test]
fn duplicate_unchanged_path_focuses_existing_item_but_same_hash_at_new_path_enqueues() {
    let temp = TempDir::new().unwrap();
    let db = store(&temp);
    let first = db.enqueue(Path::new("C:/docs/a.pdf"), "same-hash").unwrap();
    let duplicate = db.enqueue(Path::new("C:/docs/a.pdf"), "same-hash").unwrap();
    let case_variant = db.enqueue(Path::new("c:\\DOCS\\A.PDF"), "same-hash").unwrap();
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
    QueueStore::open(&path).unwrap().enqueue(Path::new("one.pdf"), "h1").unwrap();
    QueueStore::open(&path).unwrap().enqueue(Path::new("two.pdf"), "h2").unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (0..2).map(|_| {
        let path = path.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            let db = QueueStore::open(path).unwrap();
            barrier.wait();
            db.claim_next().unwrap()
        })
    }).collect();
    barrier.wait();
    let claims = handles.into_iter().filter_map(|h| h.join().unwrap()).collect::<Vec<_>>();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].status, QueueStatus::Extracting);
}

#[test]
fn invalid_transition_has_stable_code_and_does_not_change_state() {
    let temp = TempDir::new().unwrap();
    let db = store(&temp);
    let item = db.enqueue(Path::new("one.pdf"), "h1").unwrap();
    let err = db.transition(item.id, QueueStatus::Queued, QueueStatus::Completed, None).unwrap_err();
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
            db.transition(item.id, QueueStatus::Extracting, QueueStatus::Analyzing, None).unwrap();
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
    db.transition(applying.id, QueueStatus::Extracting, QueueStatus::Analyzing, None).unwrap();
    db.transition(applying.id, QueueStatus::Analyzing, QueueStatus::Ready, None).unwrap();
    db.begin_applying(applying.id, QueueStatus::Ready).unwrap();
    drop(db);
    let db = store(&temp);
    assert_eq!(db.recover_interrupted().unwrap(), 0);
    assert_eq!(db.list().unwrap()[0].status, QueueStatus::Applying);
    assert!(db.claim_next().unwrap().is_none());
    db.claim_applying_reconciliation(applying.id).unwrap();
    let db = Arc::new(db);
    assert_eq!(FileApplier::local(db.clone()).reconcile(applying.id).unwrap().status, QueueStatus::Ready);
    assert_eq!(db.claim_next().unwrap().unwrap().source_path, Path::new("waiting.pdf"));
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
    inspector.execute(
        "UPDATE queue_items SET lease_expires_at = 0 WHERE id = ?1",
        [active.id],
    ).unwrap();

    assert_eq!(observer.recover_interrupted().unwrap(), 0);
    assert_eq!(observer.list().unwrap()[0].status, QueueStatus::Extracting);
    assert_eq!(owner.renew_lease(active.id).unwrap().status, QueueStatus::Extracting);
}

#[test]
fn automatic_processing_retries_stop_after_two_failures() {
    let temp = TempDir::new().unwrap();
    let db = store(&temp);
    let item = db.enqueue(Path::new("a.pdf"), "ha").unwrap();
    for expected_attempt in 1..=2 {
        let claimed = db.claim_next().unwrap().unwrap();
        assert_eq!(claimed.id, item.id);
        let status = db.record_processing_failure(item.id, ErrorCode::ModelOutputInvalid).unwrap();
        assert_eq!(status, if expected_attempt == 1 { QueueStatus::Queued } else { QueueStatus::Failed });
    }
    assert!(db.claim_next().unwrap().is_none());
}

#[test]
fn clear_terminal_removes_only_terminal_rows() {
    let temp = TempDir::new().unwrap();
    let db = store(&temp);
    let active = db.enqueue(Path::new("active.pdf"), "a").unwrap();
    let failed = db.enqueue(Path::new("failed.pdf"), "f").unwrap();
    db.transition(failed.id, QueueStatus::Queued, QueueStatus::Canceled, None).unwrap();
    assert_eq!(db.clear_terminal().unwrap(), 1);
    assert_eq!(db.list().unwrap()[0].id, active.id);
}


#[test]
fn explicit_manual_retry_and_keep_original_cas_paths_are_enforced() {
    let temp = TempDir::new().unwrap();
    let db = store(&temp);

    let retry = db.enqueue(Path::new("retry.pdf"), "hr").unwrap();
    for _ in 0..2 {
        db.claim_next().unwrap().unwrap();
        db.record_processing_failure(retry.id, ErrorCode::ModelOutputInvalid).unwrap();
    }
    assert_eq!(db.manual_retry(retry.id).unwrap().status, QueueStatus::Queued);
    assert_eq!(db.manual_retry(retry.id).unwrap_err().code(), ErrorCode::StateConflict);

    let keep = db.enqueue(Path::new("keep.pdf"), "hk").unwrap();
    let claimed = db.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, retry.id);
    db.transition(retry.id, QueueStatus::Extracting, QueueStatus::Canceled, None).unwrap();
    let claimed = db.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, keep.id);
    db.transition(keep.id, QueueStatus::Extracting, QueueStatus::Analyzing, None).unwrap();
    db.transition(keep.id, QueueStatus::Analyzing, QueueStatus::Ready, None).unwrap();
    assert_eq!(db.complete_keep_original(keep.id, QueueStatus::Ready).unwrap().status, QueueStatus::Completed);
}

#[test]
fn legacy_schema_migrates_without_discarding_queue_or_receipt_rows() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("legacy.sqlite3");
    let legacy = rusqlite::Connection::open(&path).unwrap();
    legacy.execute_batch(
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
    ).unwrap();
    drop(legacy);

    let migrated = QueueStore::open(&path).unwrap();
    assert_eq!(migrated.list().unwrap()[0].source_path, Path::new("legacy.pdf"));
    drop(migrated);
    let inspected = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        inspected.query_row(
            "SELECT receipt_json FROM operation_receipts_legacy_v1 WHERE id = 1",
            [],
            |row| row.get::<_, String>(0),
        ).unwrap(),
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
        FileApplier::local(migrated.clone()).reconcile(item.id).unwrap_err().code(),
        ErrorCode::StateConflict,
    );
    assert_eq!(migrated.list().unwrap()[0].status, QueueStatus::Applying);
    drop(migrated);
    let inspected = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        inspected.query_row("SELECT COUNT(*) FROM operation_receipts", [], |row| row.get::<_, i64>(0)).unwrap(),
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
    connection.execute(
        "INSERT INTO operation_receipts(
           queue_item_id, direction, source_path, destination_path, pre_hash, post_hash,
           operation_kind, stage, source_exists, destination_exists, temporary_exists,
           created_at, updated_at
         ) VALUES(1, ?1, ?2, ?3, ?4, ?4, 'rename', 'complete', 0, 1, 0, 1, 1)",
        rusqlite::params![direction, receipt_source, receipt_destination, hash],
    ).unwrap();
}

fn insert_v2_complete_receipt(
    database: &Path,
    source: &Path,
    destination: &Path,
    direction: &str,
    hash: &str,
) {
    let connection = rusqlite::Connection::open(database).unwrap();
    connection.execute(
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
    ).unwrap();
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
        &database,
        &original,
        &original,
        &published,
        "apply",
        "ready",
        &hash,
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
    insert_v2_complete_receipt(
        &database,
        &first_published,
        &original,
        "undo",
        &hash,
    );

    let store = Arc::new(QueueStore::open(database).unwrap());
    let item = store.list().unwrap().into_iter().next().unwrap();
    assert_eq!(item.active_receipt_id, None);
    store.claim_applying_reconciliation(item.id).unwrap();
    assert_eq!(
        FileApplier::local(store.clone()).reconcile(item.id).unwrap_err().code(),
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
    insert_v2_complete_receipt(
        &database,
        &first_published,
        &original,
        "undo",
        &hash,
    );
    insert_v2_complete_receipt(
        &database,
        &original,
        &second_published,
        "apply",
        &hash,
    );

    let store = Arc::new(QueueStore::open(database).unwrap());
    let item = store.list().unwrap().into_iter().next().unwrap();
    assert_eq!(item.active_receipt_id, None);
    store.claim_applying_reconciliation(item.id).unwrap();
    assert_eq!(
        FileApplier::local(store.clone()).reconcile(item.id).unwrap_err().code(),
        ErrorCode::StateConflict,
    );
    assert_eq!(store.list().unwrap()[0].status, QueueStatus::Applying);
    assert!(!original.exists());
    assert!(second_published.exists());
}
