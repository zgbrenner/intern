use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use intern_core::{
    ErrorCode, FileApplier, FileIdentity, FileSystem, LockRetry, LockedFile, OperationKind,
    OperationStage, QueueStatus, QueueStore, StdFileSystem,
};
use tempfile::TempDir;

fn write(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
}

fn applying(
    temp: &TempDir,
    source: &Path,
    filesystem: Arc<dyn FileSystem>,
) -> (FileApplier, Arc<QueueStore>, i64) {
    let store = Arc::new(QueueStore::open(temp.path().join("queue.sqlite3")).unwrap());
    let item = store.enqueue(source, "queue-fingerprint").unwrap();
    let claimed = store.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, item.id);
    store
        .transition(
            item.id,
            QueueStatus::Extracting,
            QueueStatus::Analyzing,
            None,
        )
        .unwrap();
    store
        .transition(item.id, QueueStatus::Analyzing, QueueStatus::Ready, None)
        .unwrap();
    store.begin_applying(item.id, QueueStatus::Ready).unwrap();
    (FileApplier::new(filesystem, store.clone()), store, item.id)
}

#[test]
fn filesystem_error_codes_are_stable() {
    assert_eq!(ErrorCode::FileChanged.as_str(), "FILE_CHANGED");
    assert_eq!(
        ErrorCode::DestinationUnavailable.as_str(),
        "DESTINATION_UNAVAILABLE"
    );
    assert_eq!(
        ErrorCode::MoveVerificationFailed.as_str(),
        "MOVE_VERIFICATION_FAILED"
    );
    assert_eq!(
        ErrorCode::SourceDeleteFailed.as_str(),
        "SOURCE_DELETE_FAILED"
    );
}

struct CopyingFileSystem {
    inner: StdFileSystem,
    corrupt_copy: bool,
    fail_source_delete: bool,
}

struct DeleteFailLocked {
    inner: Box<dyn LockedFile>,
}

struct HashMismatchLocked {
    inner: Box<dyn LockedFile>,
}

impl LockedFile for DeleteFailLocked {
    fn hash(&mut self) -> io::Result<String> {
        self.inner.hash()
    }
    fn identity(&self) -> io::Result<FileIdentity> {
        self.inner.identity()
    }
    fn delete(self: Box<Self>) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected locked delete failure",
        ))
    }
}

impl LockedFile for HashMismatchLocked {
    fn hash(&mut self) -> io::Result<String> {
        Ok("injected-copy-mismatch".into())
    }
    fn identity(&self) -> io::Result<FileIdentity> {
        self.inner.identity()
    }
    fn delete(self: Box<Self>) -> io::Result<()> {
        self.inner.delete()
    }
}

impl FileSystem for CopyingFileSystem {
    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }
    fn hash(&self, path: &Path) -> io::Result<String> {
        self.inner.hash(path)
    }
    fn same_volume(&self, _source: &Path, _destination: &Path) -> io::Result<bool> {
        Ok(false)
    }
    fn rename_no_replace(&self, source: &Path, destination: &Path) -> io::Result<()> {
        self.inner.rename_no_replace(source, destination)
    }
    fn copy_new_locked(
        &self,
        source: &Path,
        destination: &Path,
    ) -> io::Result<Box<dyn LockedFile>> {
        let locked = self.inner.copy_new_locked(source, destination)?;
        if self.corrupt_copy {
            Ok(Box::new(HashMismatchLocked { inner: locked }))
        } else {
            Ok(locked)
        }
    }
    fn lock_for_delete(&self, path: &Path) -> io::Result<Box<dyn LockedFile>> {
        let locked = self.inner.lock_for_delete(path)?;
        if self.fail_source_delete {
            Ok(Box::new(DeleteFailLocked { inner: locked }))
        } else {
            Ok(locked)
        }
    }
}

fn copying_filesystem(corrupt_copy: bool, fail_source_delete: bool) -> Arc<dyn FileSystem> {
    Arc::new(CopyingFileSystem {
        inner: StdFileSystem,
        corrupt_copy,
        fail_source_delete,
    })
}

struct PostRenameMismatchFileSystem {
    inner: StdFileSystem,
}

impl FileSystem for PostRenameMismatchFileSystem {
    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }
    fn hash(&self, path: &Path) -> io::Result<String> {
        if path.file_name().and_then(|value| value.to_str()) == Some("named.pdf") {
            Ok("injected-mismatch".into())
        } else {
            self.inner.hash(path)
        }
    }
    fn same_volume(&self, _source: &Path, _destination: &Path) -> io::Result<bool> {
        Ok(true)
    }
    fn rename_no_replace(&self, source: &Path, destination: &Path) -> io::Result<()> {
        self.inner.rename_no_replace(source, destination)
    }
    fn copy_new_locked(
        &self,
        source: &Path,
        destination: &Path,
    ) -> io::Result<Box<dyn LockedFile>> {
        self.inner.copy_new_locked(source, destination)
    }
    fn lock_for_delete(&self, path: &Path) -> io::Result<Box<dyn LockedFile>> {
        let locked = self.inner.lock_for_delete(path)?;
        if path.file_name().and_then(|value| value.to_str()) == Some("named.pdf") {
            Ok(Box::new(HashMismatchLocked { inner: locked }))
        } else {
            Ok(locked)
        }
    }
}

struct PartialCopyFileSystem {
    inner: StdFileSystem,
}

struct RenameFailFileSystem {
    inner: StdFileSystem,
}

struct CrossPublishFailFileSystem {
    inner: StdFileSystem,
}

impl FileSystem for RenameFailFileSystem {
    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }
    fn hash(&self, path: &Path) -> io::Result<String> {
        self.inner.hash(path)
    }
    fn same_volume(&self, _source: &Path, _destination: &Path) -> io::Result<bool> {
        Ok(true)
    }
    fn rename_no_replace(&self, _source: &Path, _destination: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected rename failure",
        ))
    }
    fn copy_new_locked(
        &self,
        source: &Path,
        destination: &Path,
    ) -> io::Result<Box<dyn LockedFile>> {
        self.inner.copy_new_locked(source, destination)
    }
    fn lock_for_delete(&self, path: &Path) -> io::Result<Box<dyn LockedFile>> {
        self.inner.lock_for_delete(path)
    }
}

impl FileSystem for CrossPublishFailFileSystem {
    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }
    fn hash(&self, path: &Path) -> io::Result<String> {
        self.inner.hash(path)
    }
    fn same_volume(&self, _source: &Path, _destination: &Path) -> io::Result<bool> {
        Ok(false)
    }
    fn rename_no_replace(&self, _source: &Path, _destination: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected publish failure",
        ))
    }
    fn copy_new_locked(
        &self,
        source: &Path,
        destination: &Path,
    ) -> io::Result<Box<dyn LockedFile>> {
        self.inner.copy_new_locked(source, destination)
    }
    fn lock_for_delete(&self, path: &Path) -> io::Result<Box<dyn LockedFile>> {
        self.inner.lock_for_delete(path)
    }
}

struct ReconciliationProbeFileSystem {
    inner: StdFileSystem,
    database: PathBuf,
    item_id: i64,
    attempted: Arc<AtomicBool>,
}

struct ReconciliationProbeLocked {
    inner: Box<dyn LockedFile>,
    database: PathBuf,
    item_id: i64,
    attempted: Arc<AtomicBool>,
}

impl LockedFile for ReconciliationProbeLocked {
    fn hash(&mut self) -> io::Result<String> {
        if !self.attempted.swap(true, Ordering::SeqCst) {
            let inspector = rusqlite::Connection::open(&self.database).unwrap();
            inspector
                .execute(
                    "UPDATE queue_items SET lease_expires_at = 0 WHERE id = ?1",
                    [self.item_id],
                )
                .unwrap();
            let reconcilier = QueueStore::open(&self.database).unwrap();
            assert_eq!(
                reconcilier
                    .claim_applying_reconciliation(self.item_id)
                    .unwrap_err()
                    .code(),
                ErrorCode::StateConflict,
            );
        }
        self.inner.hash()
    }

    fn identity(&self) -> io::Result<FileIdentity> {
        self.inner.identity()
    }
    fn delete(self: Box<Self>) -> io::Result<()> {
        self.inner.delete()
    }
}

impl FileSystem for ReconciliationProbeFileSystem {
    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }
    fn hash(&self, path: &Path) -> io::Result<String> {
        self.inner.hash(path)
    }
    fn same_volume(&self, _source: &Path, _destination: &Path) -> io::Result<bool> {
        Ok(false)
    }
    fn rename_no_replace(&self, source: &Path, destination: &Path) -> io::Result<()> {
        self.inner.rename_no_replace(source, destination)
    }
    fn copy_new_locked(
        &self,
        source: &Path,
        destination: &Path,
    ) -> io::Result<Box<dyn LockedFile>> {
        self.inner.copy_new_locked(source, destination)
    }
    fn lock_for_delete(&self, path: &Path) -> io::Result<Box<dyn LockedFile>> {
        let locked = self.inner.lock_for_delete(path)?;
        if path.file_name().and_then(|value| value.to_str()) == Some("named.pdf") {
            Ok(Box::new(ReconciliationProbeLocked {
                inner: locked,
                database: self.database.clone(),
                item_id: self.item_id,
                attempted: self.attempted.clone(),
            }))
        } else {
            Ok(locked)
        }
    }
}

impl FileSystem for PartialCopyFileSystem {
    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }
    fn hash(&self, path: &Path) -> io::Result<String> {
        self.inner.hash(path)
    }
    fn same_volume(&self, _source: &Path, _destination: &Path) -> io::Result<bool> {
        Ok(false)
    }
    fn rename_no_replace(&self, source: &Path, destination: &Path) -> io::Result<()> {
        self.inner.rename_no_replace(source, destination)
    }
    fn copy_new_locked(
        &self,
        source: &Path,
        destination: &Path,
    ) -> io::Result<Box<dyn LockedFile>> {
        let locked = self.inner.copy_new_locked(source, destination)?;
        locked.delete()?;
        Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "injected partial copy",
        ))
    }
    fn lock_for_delete(&self, path: &Path) -> io::Result<Box<dyn LockedFile>> {
        self.inner.lock_for_delete(path)
    }
}

#[test]
fn same_volume_apply_never_overwrites_and_uses_deterministic_suffix() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.pdf");
    let requested = temp.path().join("named.pdf");
    let second_collision = temp.path().join("named (2).pdf");
    write(&source, b"source");
    write(&requested, b"existing");
    write(&second_collision, b"existing two");
    let (applier, _store, item_id) = applying(&temp, &source, Arc::new(StdFileSystem));
    let receipt = applier
        .apply(
            item_id,
            &source,
            &requested,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap();
    assert_eq!(receipt.destination, temp.path().join("named (3).pdf"));
    assert_eq!(fs::read(&requested).unwrap(), b"existing");
    assert_eq!(fs::read(&second_collision).unwrap(), b"existing two");
    assert!(!source.exists());
    assert_eq!(receipt.kind, OperationKind::Rename);
    assert_eq!(receipt.stage, OperationStage::Complete);
}

#[test]
fn changed_source_is_rejected_before_a_receipt_or_mutation() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"changed");
    let (applier, store, item_id) = applying(&temp, &source, Arc::new(StdFileSystem));
    let err = applier
        .apply(item_id, &source, &destination, "old-fingerprint")
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::FileChanged);
    assert!(store.load_receipt(item_id).unwrap().is_none());
    assert!(source.exists());
    assert!(!destination.exists());
}

#[test]
fn one_active_receipt_is_bound_to_each_applying_epoch() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("queue.sqlite3");
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let filesystem = Arc::new(RenameFailFileSystem {
        inner: StdFileSystem,
    });
    let (applier, store, item_id) = applying(&temp, &source, filesystem);
    let fingerprint = applier.fingerprint(&source).unwrap();
    let first = applier
        .apply(item_id, &source, &destination, &fingerprint)
        .unwrap_err();
    assert_eq!(first.receipt().unwrap().stage, OperationStage::Planned);
    let first_id = first.receipt().unwrap().id;

    let second = applier
        .apply(item_id, &source, &destination, &fingerprint)
        .unwrap_err();

    assert_eq!(second.code(), ErrorCode::StateConflict);
    assert_eq!(store.load_receipt(item_id).unwrap().unwrap().id, first_id);
    assert!(source.exists());
    assert!(!destination.exists());

    drop(applier);
    drop(store);
    let reopened = Arc::new(QueueStore::open(database).unwrap());
    reopened.claim_applying_reconciliation(item_id).unwrap();
    let resolved = FileApplier::local(reopened).reconcile(item_id).unwrap();
    assert_eq!(resolved.status, QueueStatus::Ready);
}

#[test]
fn same_volume_verification_uncertainty_journals_successful_rollback() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("queue.sqlite3");
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let filesystem = Arc::new(PostRenameMismatchFileSystem {
        inner: StdFileSystem,
    });
    let (applier, store, item_id) = applying(&temp, &source, filesystem);
    let fingerprint = applier.fingerprint(&source).unwrap();
    let err = applier
        .apply(item_id, &source, &destination, &fingerprint)
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::MoveVerificationFailed);
    assert_eq!(err.receipt().unwrap().stage, OperationStage::RolledBack);
    assert_eq!(
        store.load_receipt(item_id).unwrap().unwrap().stage,
        OperationStage::RolledBack
    );
    assert_eq!(fs::read(&source).unwrap(), b"original");
    assert!(!destination.exists());
    assert_eq!(store.list().unwrap()[0].status, QueueStatus::Applying);

    drop(applier);
    drop(store);
    let reopened = Arc::new(QueueStore::open(database).unwrap());
    reopened.claim_applying_reconciliation(item_id).unwrap();
    let resolved = FileApplier::local(reopened).reconcile(item_id).unwrap();
    assert_eq!(resolved.status, QueueStatus::Ready);
    assert!(source.exists());
    assert!(!destination.exists());
}

#[test]
fn undo_refuses_destination_modified_after_apply() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let (applier, store, item_id) = applying(&temp, &source, Arc::new(StdFileSystem));
    let receipt = applier
        .apply(
            item_id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap();
    store.complete_apply(item_id, receipt.id).unwrap();
    store
        .begin_applying(item_id, QueueStatus::Completed)
        .unwrap();
    write(&receipt.destination, b"modified");
    let err = applier.undo(item_id, &receipt).unwrap_err();
    assert_eq!(err.code(), ErrorCode::FileChanged);
    assert!(receipt.destination.exists());
    assert!(!source.exists());
}

#[test]
fn undo_restores_an_unchanged_destination_with_its_own_receipt() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let (applier, store, item_id) = applying(&temp, &source, Arc::new(StdFileSystem));
    let applied = applier
        .apply(
            item_id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap();
    assert_eq!(
        store
            .complete_apply(item_id, applied.id + 1)
            .unwrap_err()
            .code(),
        ErrorCode::StateConflict
    );
    store.complete_apply(item_id, applied.id).unwrap();
    store
        .begin_applying(item_id, QueueStatus::Completed)
        .unwrap();
    let undone = applier.undo(item_id, &applied).unwrap();
    assert_eq!(undone.stage, OperationStage::Complete);
    assert_eq!(fs::read(&source).unwrap(), b"original");
    assert!(!destination.exists());
    assert_eq!(
        store.complete_undo(item_id, undone.id).unwrap().status,
        QueueStatus::Ready
    );
}

#[test]
fn cross_volume_hash_mismatch_cleans_temp_and_retains_source() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let (applier, store, item_id) = applying(&temp, &source, copying_filesystem(true, false));
    let err = applier
        .apply(
            item_id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::MoveVerificationFailed);
    assert!(source.exists());
    assert!(!destination.exists());
    assert!(
        !store
            .load_receipt(item_id)
            .unwrap()
            .unwrap()
            .temporary_exists
    );
}

#[test]
fn partial_copy_failure_cleans_temp_and_retains_source() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let (applier, store, item_id) = applying(
        &temp,
        &source,
        Arc::new(PartialCopyFileSystem {
            inner: StdFileSystem,
        }),
    );
    let err = applier
        .apply(
            item_id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::DestinationUnavailable);
    assert!(source.exists());
    assert!(!destination.exists());
    assert!(
        !store
            .load_receipt(item_id)
            .unwrap()
            .unwrap()
            .temporary_exists
    );
}

#[test]
fn publish_failure_reopen_verifies_and_deletes_temp_before_rollback_resolution() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("queue.sqlite3");
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let filesystem = Arc::new(CrossPublishFailFileSystem {
        inner: StdFileSystem,
    });
    let (applier, store, item_id) = applying(&temp, &source, filesystem);
    let error = applier
        .apply(
            item_id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::DestinationUnavailable);
    let receipt = store.load_receipt(item_id).unwrap().unwrap();
    assert_eq!(receipt.stage, OperationStage::Verified);
    assert!(receipt.temporary_exists);
    let temporary = receipt.temporary_path.clone().unwrap();
    assert!(temporary.exists());
    drop(applier);
    drop(store);

    let reopened = Arc::new(QueueStore::open(database).unwrap());
    reopened.claim_applying_reconciliation(item_id).unwrap();
    let resolved = FileApplier::local(reopened).reconcile(item_id).unwrap();
    assert_eq!(resolved.status, QueueStatus::Ready);
    assert!(source.exists());
    assert!(!destination.exists());
    assert!(!temporary.exists());
}

#[test]
fn cross_volume_verifies_and_journals_before_locked_source_deletion() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let (applier, store, item_id) = applying(&temp, &source, copying_filesystem(false, false));
    let receipt = applier
        .apply(
            item_id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap();
    assert!(!source.exists());
    assert_eq!(fs::read(&destination).unwrap(), b"original");
    assert_eq!(receipt.kind, OperationKind::VerifiedCopy);
    assert_eq!(
        receipt.post_operation_hash.as_deref(),
        Some(receipt.pre_operation_hash.as_str())
    );
    assert_eq!(
        store.load_receipt(item_id).unwrap().unwrap().stage,
        OperationStage::Complete
    );
}

#[test]
fn published_verification_renews_ownership_and_blocks_a_reconcilier() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("queue.sqlite3");
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let store = Arc::new(QueueStore::open(&database).unwrap());
    let item = store.enqueue(&source, "queue-fingerprint").unwrap();
    store.claim_next().unwrap().unwrap();
    store
        .transition(
            item.id,
            QueueStatus::Extracting,
            QueueStatus::Analyzing,
            None,
        )
        .unwrap();
    store
        .transition(item.id, QueueStatus::Analyzing, QueueStatus::Ready, None)
        .unwrap();
    store.begin_applying(item.id, QueueStatus::Ready).unwrap();
    let attempted = Arc::new(AtomicBool::new(false));
    let filesystem = Arc::new(ReconciliationProbeFileSystem {
        inner: StdFileSystem,
        database,
        item_id: item.id,
        attempted: attempted.clone(),
    });
    let applier = FileApplier::new(filesystem, store.clone());

    let receipt = applier
        .apply(
            item.id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap();

    assert!(attempted.load(Ordering::SeqCst));
    assert_eq!(receipt.stage, OperationStage::Complete);
    assert!(!source.exists());
    assert_eq!(fs::read(destination).unwrap(), b"original");
}

#[test]
fn complete_receipt_resolves_after_crash_reopen_without_blocking_queue() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("queue.sqlite3");
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let (applier, store, item_id) = applying(&temp, &source, Arc::new(StdFileSystem));
    let receipt = applier
        .apply(
            item_id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap();
    assert_eq!(receipt.stage, OperationStage::Complete);
    drop(applier);
    drop(store);

    let reopened = Arc::new(QueueStore::open(database).unwrap());
    reopened.claim_applying_reconciliation(item_id).unwrap();
    let resolved = FileApplier::local(reopened).reconcile(item_id).unwrap();
    assert_eq!(resolved.status, QueueStatus::Completed);
    assert!(!source.exists());
    assert_eq!(fs::read(destination).unwrap(), b"original");
}

#[test]
fn source_delete_failure_requires_explicit_reconciliation_before_source_deletion() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("queue.sqlite3");
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let (applier, store, item_id) = applying(&temp, &source, copying_filesystem(false, true));
    let err = applier
        .apply(
            item_id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::SourceDeleteFailed);
    assert_eq!(err.receipt().unwrap().stage, OperationStage::Published);
    assert_eq!(
        store.load_receipt(item_id).unwrap().unwrap().stage,
        OperationStage::Published
    );
    assert!(source.exists());
    assert_eq!(fs::read(&destination).unwrap(), b"original");
    assert_eq!(store.list().unwrap()[0].status, QueueStatus::NeedsReview);

    drop(applier);
    drop(store);
    let reopened = Arc::new(QueueStore::open(database).unwrap());
    assert_eq!(
        reopened.load_receipt(item_id).unwrap().unwrap().stage,
        OperationStage::Published
    );
    assert_eq!(reopened.list().unwrap()[0].status, QueueStatus::NeedsReview);
    reopened.recover_interrupted().unwrap();
    assert!(
        source.exists(),
        "periodic recovery must not silently delete source"
    );
    reopened.claim_deferred_reconciliation(item_id).unwrap();
    let resolved = FileApplier::local(reopened.clone())
        .reconcile(item_id)
        .unwrap();
    assert_eq!(resolved.status, QueueStatus::Completed);
    assert!(!source.exists());
    assert_eq!(fs::read(&destination).unwrap(), b"original");
}

#[test]
fn undo_does_not_overwrite_recreated_source() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let (applier, store, item_id) = applying(&temp, &source, Arc::new(StdFileSystem));
    let receipt = applier
        .apply(
            item_id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap();
    store.complete_apply(item_id, receipt.id).unwrap();
    store
        .begin_applying(item_id, QueueStatus::Completed)
        .unwrap();
    write(&source, b"new file");
    let err = applier.undo(item_id, &receipt).unwrap_err();
    assert_eq!(err.code(), ErrorCode::DestinationUnavailable);
    assert_eq!(fs::read(&source).unwrap(), b"new file");
    assert!(destination.exists());
}

#[test]
fn undo_rejects_a_receipt_that_does_not_match_the_durable_apply() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let (applier, store, item_id) = applying(&temp, &source, Arc::new(StdFileSystem));
    let applied = applier
        .apply(
            item_id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap();
    store.complete_apply(item_id, applied.id).unwrap();
    store
        .begin_applying(item_id, QueueStatus::Completed)
        .unwrap();
    let mut forged = applied.clone();
    forged.source = temp.path().join("different.pdf");
    let error = applier.undo(item_id, &forged).unwrap_err();
    assert_eq!(error.code(), ErrorCode::StateConflict);
    assert!(destination.exists());
    assert!(!source.exists());
    assert!(!forged.source.exists());
}

#[test]
fn applying_direction_is_bound_to_the_previous_queue_status() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    let second_destination = temp.path().join("second.pdf");
    write(&source, b"original");
    let (applier, store, item_id) = applying(&temp, &source, Arc::new(StdFileSystem));
    let applied = applier
        .apply(
            item_id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap();
    store.complete_apply(item_id, applied.id).unwrap();
    store
        .begin_applying(item_id, QueueStatus::Completed)
        .unwrap();

    let error = applier
        .apply(
            item_id,
            &applied.destination,
            &second_destination,
            applied.post_operation_hash.as_deref().unwrap(),
        )
        .unwrap_err();

    assert_eq!(error.code(), ErrorCode::StateConflict);
    assert_eq!(store.load_receipt(item_id).unwrap().unwrap(), applied);
    assert!(destination.exists());
    assert!(!second_destination.exists());
}

#[test]
fn reopen_does_not_bind_a_historical_receipt_to_an_empty_applying_epoch() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("queue.sqlite3");
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let (applier, store, item_id) = applying(&temp, &source, Arc::new(StdFileSystem));
    let historical = applier
        .apply(
            item_id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap();
    store.complete_apply(item_id, historical.id).unwrap();
    store
        .begin_applying(item_id, QueueStatus::Completed)
        .unwrap();
    assert_eq!(store.list().unwrap()[0].active_receipt_id, None);
    drop(applier);
    drop(store);

    let reopened = Arc::new(QueueStore::open(database).unwrap());
    assert_eq!(reopened.list().unwrap()[0].active_receipt_id, None);
    assert_eq!(reopened.load_receipt(item_id).unwrap().unwrap(), historical);
    reopened.claim_applying_reconciliation(item_id).unwrap();
    let resolved = FileApplier::local(reopened).reconcile(item_id).unwrap();
    assert_eq!(resolved.status, QueueStatus::Completed);
    assert!(destination.exists());
    assert!(!source.exists());
}

#[test]
fn second_apply_crash_before_receipt_ignores_completed_first_apply() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("queue.sqlite3");
    let source = temp.path().join("source.pdf");
    let destination = temp.path().join("named.pdf");
    write(&source, b"original");
    let (applier, store, item_id) = applying(&temp, &source, Arc::new(StdFileSystem));
    let applied = applier
        .apply(
            item_id,
            &source,
            &destination,
            &applier.fingerprint(&source).unwrap(),
        )
        .unwrap();
    store.complete_apply(item_id, applied.id).unwrap();
    store
        .begin_applying(item_id, QueueStatus::Completed)
        .unwrap();
    let undone = applier.undo(item_id, &applied).unwrap();
    store.complete_undo(item_id, undone.id).unwrap();

    store.begin_applying(item_id, QueueStatus::Ready).unwrap();
    assert_eq!(store.list().unwrap()[0].active_receipt_id, None);
    drop(applier);
    drop(store);

    let reopened = Arc::new(QueueStore::open(database).unwrap());
    reopened.claim_applying_reconciliation(item_id).unwrap();
    let resolved = FileApplier::local(reopened).reconcile(item_id).unwrap();
    assert_eq!(resolved.status, QueueStatus::Ready);
    assert!(source.exists());
    assert!(!destination.exists());
}

#[cfg(windows)]
mod windows_safety {
    use std::{fs, io::Write};

    use super::*;

    #[test]
    fn windows_atomic_rename_fails_if_destination_already_exists() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source.pdf");
        let destination = temp.path().join("destination.pdf");
        write(&source, b"source");
        write(&destination, b"destination");
        let error = StdFileSystem
            .rename_no_replace(&source, &destination)
            .unwrap_err();
        assert!(matches!(error.raw_os_error(), Some(80 | 183)));
        assert_eq!(fs::read(&source).unwrap(), b"source");
        assert_eq!(fs::read(&destination).unwrap(), b"destination");
    }

    #[test]
    fn windows_delete_lock_denies_write_and_path_replacement_until_same_handle_delete() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source.pdf");
        write(&source, b"source");
        let mut locked = StdFileSystem.lock_for_delete(&source).unwrap();
        let identity = locked.identity().unwrap();
        assert_eq!(locked.hash().unwrap(), StdFileSystem.hash(&source).unwrap());
        assert!(fs::OpenOptions::new().write(true).open(&source).is_err());
        assert!(fs::remove_file(&source).is_err());
        assert_eq!(locked.identity().unwrap(), identity);
        locked.delete().unwrap();
        assert!(!source.exists());
    }

    #[test]
    fn windows_destination_race_cannot_overwrite_competing_file() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source.pdf");
        let destination = temp.path().join("destination.pdf");
        write(&source, b"source");
        let mut competing = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .unwrap();
        competing.write_all(b"winner").unwrap();
        competing.sync_all().unwrap();
        drop(competing);
        assert!(
            StdFileSystem
                .rename_no_replace(&source, &destination)
                .is_err()
        );
        assert_eq!(fs::read(&source).unwrap(), b"source");
        assert_eq!(fs::read(&destination).unwrap(), b"winner");
    }
}

/// A source the sync client is holding: the first `holds` opens fail the way
/// Windows reports ERROR_SHARING_VIOLATION, and everything after that succeeds.
struct SyncLockedFileSystem {
    inner: StdFileSystem,
    remaining_holds: AtomicUsize,
}

impl SyncLockedFileSystem {
    fn take_hold(&self) -> Option<io::Error> {
        let held = self
            .remaining_holds
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            });
        held.ok().map(|_| io::Error::from_raw_os_error(32))
    }
}

impl FileSystem for SyncLockedFileSystem {
    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }
    fn hash(&self, path: &Path) -> io::Result<String> {
        match self.take_hold() {
            Some(error) => Err(error),
            None => self.inner.hash(path),
        }
    }
    fn same_volume(&self, _source: &Path, _destination: &Path) -> io::Result<bool> {
        Ok(true)
    }
    fn rename_no_replace(&self, source: &Path, destination: &Path) -> io::Result<()> {
        self.inner.rename_no_replace(source, destination)
    }
    fn copy_new_locked(
        &self,
        source: &Path,
        destination: &Path,
    ) -> io::Result<Box<dyn LockedFile>> {
        self.inner.copy_new_locked(source, destination)
    }
    fn lock_for_delete(&self, path: &Path) -> io::Result<Box<dyn LockedFile>> {
        self.inner.lock_for_delete(path)
    }
}

fn sync_locked(holds: usize) -> Arc<SyncLockedFileSystem> {
    Arc::new(SyncLockedFileSystem {
        inner: StdFileSystem,
        remaining_holds: AtomicUsize::new(holds),
    })
}

#[cfg(windows)]
#[test]
fn a_source_the_sync_client_releases_is_applied_rather_than_sent_to_review() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("statement.pdf");
    let destination = temp.path().join("2026-04-01 Statement of Work.pdf");
    write(&source, b"statement bytes");

    // Two holds: one spent on the fingerprint, one on the pre-rename hash.
    let filesystem = sync_locked(2);
    let (applier, _store, item_id) = applying(&temp, &source, filesystem);
    let applier = applier.with_lock_retry(LockRetry::new(5, Duration::from_millis(1)));

    let fingerprint = applier.fingerprint(&source).unwrap();
    applier
        .apply(item_id, &source, &destination, &fingerprint)
        .unwrap();

    assert!(destination.exists());
    assert!(!source.exists());
}

#[cfg(windows)]
#[test]
fn a_source_still_held_after_the_retries_reads_as_locked_not_as_changed() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("statement.pdf");
    write(&source, b"statement bytes");

    let filesystem = sync_locked(usize::MAX);
    let (applier, _store, _item_id) = applying(&temp, &source, filesystem);
    let applier = applier.with_lock_retry(LockRetry::immediate());

    let error = applier.fingerprint(&source).unwrap_err();

    assert_eq!(error.code(), ErrorCode::SourceLocked);
    assert_eq!(ErrorCode::SourceLocked.as_str(), "SOURCE_LOCKED");
    // The operating system's own error survives into the message; "os error 32"
    // is the difference between a sync client mid-upload and a real problem.
    assert!(
        error.to_string().contains("os error 32"),
        "expected the OS error to be carried, got: {error}"
    );
}
