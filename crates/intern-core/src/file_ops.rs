use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, atomic::{AtomicU64, Ordering}},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ErrorCode, InternError, InternResult, QueueItem, QueueStore};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Rename,
    VerifiedCopy,
}

impl OperationKind {
    pub(crate) fn as_db(self) -> &'static str {
        match self { Self::Rename => "rename", Self::VerifiedCopy => "verified_copy" }
    }

    pub(crate) fn from_db(value: &str) -> Option<Self> {
        match value { "rename" => Some(Self::Rename), "verified_copy" => Some(Self::VerifiedCopy), _ => None }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationDirection {
    Apply,
    Undo,
}

impl OperationDirection {
    pub(crate) fn as_db(self) -> &'static str {
        match self { Self::Apply => "apply", Self::Undo => "undo" }
    }

    pub(crate) fn from_db(value: &str) -> Option<Self> {
        match value { "apply" => Some(Self::Apply), "undo" => Some(Self::Undo), _ => None }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStage {
    Planned,
    Copied,
    Verified,
    Published,
    RollbackRequired,
    RolledBack,
    Complete,
}

impl OperationStage {
    pub(crate) fn as_db(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Copied => "copied",
            Self::Verified => "verified",
            Self::Published => "published",
            Self::RollbackRequired => "rollback_required",
            Self::RolledBack => "rolled_back",
            Self::Complete => "complete",
        }
    }

    pub(crate) fn from_db(value: &str) -> Option<Self> {
        match value {
            "planned" => Some(Self::Planned),
            "copied" => Some(Self::Copied),
            "verified" => Some(Self::Verified),
            "published" => Some(Self::Published),
            "rollback_required" => Some(Self::RollbackRequired),
            "rolled_back" => Some(Self::RolledBack),
            "complete" => Some(Self::Complete),
            _ => None,
        }
    }

    pub(crate) fn can_advance_to(self, next: Self) -> bool {
        self == next || matches!(
            (self, next),
            (Self::Planned, Self::Copied | Self::Published | Self::RollbackRequired | Self::RolledBack)
                | (Self::Copied, Self::Verified | Self::RollbackRequired)
                | (Self::Verified, Self::Published | Self::RollbackRequired)
                | (Self::Published, Self::Complete | Self::RollbackRequired)
                | (Self::RollbackRequired, Self::RolledBack)
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationReceipt {
    pub id: i64,
    pub queue_item_id: i64,
    pub direction: OperationDirection,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub temporary_path: Option<PathBuf>,
    pub pre_operation_hash: String,
    pub post_operation_hash: Option<String>,
    pub kind: OperationKind,
    pub stage: OperationStage,
    pub source_exists: bool,
    pub destination_exists: bool,
    pub temporary_exists: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    pub volume: u64,
    pub file: u128,
}

pub trait LockedFile: Send {
    fn hash(&mut self) -> io::Result<String>;
    fn identity(&self) -> io::Result<FileIdentity>;
    fn delete(self: Box<Self>) -> io::Result<()>;
}

pub trait FileSystem: Send + Sync {
    fn exists(&self, path: &Path) -> bool;
    fn hash(&self, path: &Path) -> io::Result<String>;
    fn same_volume(&self, source: &Path, destination: &Path) -> io::Result<bool>;
    fn rename_no_replace(&self, source: &Path, destination: &Path) -> io::Result<()>;
    fn copy_new_locked(&self, source: &Path, destination: &Path) -> io::Result<Box<dyn LockedFile>>;
    fn lock_for_delete(&self, path: &Path) -> io::Result<Box<dyn LockedFile>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StdFileSystem;

impl FileSystem for StdFileSystem {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn hash(&self, path: &Path) -> io::Result<String> {
        hash_reader(fs::File::open(path)?)
    }

    fn same_volume(&self, source: &Path, destination: &Path) -> io::Result<bool> {
        same_volume(source, destination)
    }

    fn rename_no_replace(&self, source: &Path, destination: &Path) -> io::Result<()> {
        rename_no_replace(source, destination)
    }

    fn copy_new_locked(&self, source: &Path, destination: &Path) -> io::Result<Box<dyn LockedFile>> {
        copy_new_locked(source, destination)
    }

    fn lock_for_delete(&self, path: &Path) -> io::Result<Box<dyn LockedFile>> {
        lock_for_delete(path)
    }
}

pub struct FileApplier {
    filesystem: Arc<dyn FileSystem>,
    store: Arc<QueueStore>,
}

impl FileApplier {
    pub fn new(filesystem: Arc<dyn FileSystem>, store: Arc<QueueStore>) -> Self {
        Self { filesystem, store }
    }

    pub fn local(store: Arc<QueueStore>) -> Self {
        Self::new(Arc::new(StdFileSystem), store)
    }

    pub fn fingerprint(&self, path: &Path) -> InternResult<String> {
        self.filesystem.hash(path).map_err(|_| {
            InternError::new(ErrorCode::IoError, "could not fingerprint source file")
        })
    }

    pub fn apply(
        &self,
        queue_item_id: i64,
        source: &Path,
        requested_destination: &Path,
        expected_fingerprint: &str,
    ) -> InternResult<OperationReceipt> {
        let destination = self.available_destination(requested_destination)?;
        self.transfer(
            queue_item_id,
            OperationDirection::Apply,
            source,
            &destination,
            expected_fingerprint,
        )
    }

    pub fn undo(&self, queue_item_id: i64, applied: &OperationReceipt) -> InternResult<OperationReceipt> {
        let durable = self.store.load_receipt(queue_item_id)?.ok_or_else(|| {
            InternError::new(ErrorCode::StateConflict, "undo requires a durable apply receipt")
        })?;
        if durable != *applied
            || applied.queue_item_id != queue_item_id
            || applied.direction != OperationDirection::Apply
            || applied.stage != OperationStage::Complete
        {
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "undo receipt does not match the completed durable apply operation",
            ));
        }
        if self.filesystem.exists(&applied.source) {
            return Err(InternError::new(ErrorCode::DestinationUnavailable, "original source path is occupied"));
        }
        let expected = applied.post_operation_hash.as_deref().ok_or_else(|| {
            InternError::new(ErrorCode::InvalidData, "completed receipt has no destination hash")
        })?;
        self.transfer(
            queue_item_id,
            OperationDirection::Undo,
            &applied.destination,
            &applied.source,
            expected,
        )
    }

    pub fn reconcile(&self, queue_item_id: i64) -> InternResult<QueueItem> {
        let Some(receipt) = self.store.load_active_receipt(queue_item_id)? else {
            return self.store.resolve_empty_applying(queue_item_id);
        };
        match receipt.stage {
            OperationStage::RolledBack => self.reconcile_rolled_back(receipt),
            OperationStage::Published => self.reconcile_published(receipt),
            OperationStage::Complete => self.reconcile_complete(receipt),
            OperationStage::Planned
            | OperationStage::Copied
            | OperationStage::Verified
            | OperationStage::RollbackRequired => self.reconcile_incomplete(receipt),
        }
    }

    fn reconcile_incomplete(&self, receipt: OperationReceipt) -> InternResult<QueueItem> {
        let source_exists = self.filesystem.exists(&receipt.source);
        let destination_exists = self.filesystem.exists(&receipt.destination);
        if source_exists && !destination_exists {
            let _source = self.verify_reconciled_source(&receipt)?;
            self.cleanup_reconciled_temporary(&receipt)?;
            return self.store.resolve_reconciled_rollback(
                receipt.queue_item_id,
                receipt.id,
                receipt.stage,
            );
        }
        if !source_exists && destination_exists {
            let _destination = self.verify_reconciled_destination(&receipt)?;
            return self.store.resolve_verified_operation(
                receipt.queue_item_id,
                receipt.id,
                receipt.stage,
            );
        }
        Err(InternError::new(
            ErrorCode::StateConflict,
            "incomplete receipt paths require manual reconciliation",
        ).with_receipt(receipt))
    }

    fn cleanup_reconciled_temporary(&self, receipt: &OperationReceipt) -> InternResult<()> {
        let Some(temporary_path) = receipt.temporary_path.as_deref() else {
            return Ok(());
        };
        if !self.filesystem.exists(temporary_path) {
            if receipt.temporary_exists {
                return Err(InternError::new(
                    ErrorCode::StateConflict,
                    "recorded temporary file absence cannot be proven",
                ).with_receipt(receipt.clone()));
            }
            return Ok(());
        }
        let mut temporary = self.filesystem.lock_for_delete(temporary_path).map_err(|_| {
            InternError::new(
                ErrorCode::MoveVerificationFailed,
                "temporary file cannot be locked for reconciliation",
            ).with_receipt(receipt.clone())
        })?;
        let identity = temporary.identity().map_err(|_| {
            InternError::new(
                ErrorCode::MoveVerificationFailed,
                "temporary file identity is unavailable",
            ).with_receipt(receipt.clone())
        })?;
        let hash = temporary.hash().map_err(|_| {
            InternError::new(
                ErrorCode::MoveVerificationFailed,
                "temporary file hash is unavailable",
            ).with_receipt(receipt.clone())
        })?;
        let identity_after = temporary.identity().map_err(|_| {
            InternError::new(
                ErrorCode::MoveVerificationFailed,
                "temporary file identity became unavailable",
            ).with_receipt(receipt.clone())
        })?;
        if hash != receipt.pre_operation_hash || identity_after != identity {
            return Err(InternError::new(
                ErrorCode::MoveVerificationFailed,
                "temporary file does not match its receipt",
            ).with_receipt(receipt.clone()));
        }
        self.store.renew_operation_lease(
            receipt.queue_item_id,
            receipt.id,
            receipt.stage,
        ).map_err(|_| {
            InternError::new(
                ErrorCode::StateConflict,
                "temporary cleanup ownership was lost",
            ).with_receipt(receipt.clone())
        })?;
        temporary.delete().map_err(|_| {
            InternError::new(
                ErrorCode::SourceDeleteFailed,
                "temporary reconciliation deletion failed",
            ).with_receipt(receipt.clone())
        })
    }

    fn reconcile_rolled_back(&self, receipt: OperationReceipt) -> InternResult<QueueItem> {
        if self.filesystem.exists(&receipt.destination) {
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "rolled-back destination path is occupied",
            ).with_receipt(receipt));
        }
        let _source = self.verify_reconciled_source(&receipt)?;
        self.cleanup_reconciled_temporary(&receipt)?;
        self.store.resolve_reconciled_rollback(
            receipt.queue_item_id,
            receipt.id,
            OperationStage::RolledBack,
        )
    }

    fn verify_reconciled_source(
        &self,
        receipt: &OperationReceipt,
    ) -> InternResult<Box<dyn LockedFile>> {
        let mut source = self.filesystem.lock_for_delete(&receipt.source).map_err(|_| {
            InternError::new(ErrorCode::FileChanged, "rolled-back source cannot be verified")
                .with_receipt(receipt.clone())
        })?;
        let identity = source.identity().map_err(|_| {
            InternError::new(ErrorCode::FileChanged, "rolled-back source identity is unavailable")
                .with_receipt(receipt.clone())
        })?;
        let hash = source.hash().map_err(|_| {
            InternError::new(ErrorCode::FileChanged, "rolled-back source hash is unavailable")
                .with_receipt(receipt.clone())
        })?;
        let identity_after = source.identity().map_err(|_| {
            InternError::new(ErrorCode::FileChanged, "rolled-back source identity became unavailable")
                .with_receipt(receipt.clone())
        })?;
        if hash != receipt.pre_operation_hash || identity_after != identity {
            return Err(InternError::new(
                ErrorCode::FileChanged,
                "rolled-back source does not match its receipt",
            ).with_receipt(receipt.clone()));
        }
        Ok(source)
    }

    fn reconcile_complete(&self, receipt: OperationReceipt) -> InternResult<QueueItem> {
        if self.filesystem.exists(&receipt.source) {
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "completed receipt still has a source path",
            ).with_receipt(receipt));
        }
        let destination = self.verify_reconciled_destination(&receipt)?;
        let resolved = self.store.resolve_verified_operation(
            receipt.queue_item_id,
            receipt.id,
            OperationStage::Complete,
        );
        drop(destination);
        resolved
    }

    fn reconcile_published(&self, receipt: OperationReceipt) -> InternResult<QueueItem> {
        let mut destination = self.verify_reconciled_destination(&receipt)?;
        let destination_identity = destination.identity().map_err(|_| {
            InternError::new(ErrorCode::MoveVerificationFailed, "published destination identity is unavailable")
                .with_receipt(receipt.clone())
        })?;
        if self.filesystem.exists(&receipt.source) {
            let mut source = self.filesystem.lock_for_delete(&receipt.source).map_err(|_| {
                InternError::new(ErrorCode::FileChanged, "published source cannot be locked")
                    .with_receipt(receipt.clone())
            })?;
            let source_identity = source.identity().map_err(|_| {
                InternError::new(ErrorCode::FileChanged, "published source identity is unavailable")
                    .with_receipt(receipt.clone())
            })?;
            let source_hash = source.hash().map_err(|_| {
                InternError::new(ErrorCode::FileChanged, "published source hash is unavailable")
                    .with_receipt(receipt.clone())
            })?;
            let source_identity_after = source.identity().map_err(|_| {
                InternError::new(ErrorCode::FileChanged, "published source identity became unavailable")
                    .with_receipt(receipt.clone())
            })?;
            let destination_identity_after = destination.identity().map_err(|_| {
                InternError::new(ErrorCode::MoveVerificationFailed, "published destination identity became unavailable")
                    .with_receipt(receipt.clone())
            })?;
            if source_hash != receipt.pre_operation_hash
                || source_identity_after != source_identity
                || destination_identity_after != destination_identity
            {
                return Err(InternError::new(
                    ErrorCode::FileChanged,
                    "published paths changed during reconciliation",
                ).with_receipt(receipt));
            }
            self.store.renew_operation_lease(
                receipt.queue_item_id,
                receipt.id,
                OperationStage::Published,
            ).map_err(|_| {
                InternError::new(ErrorCode::StateConflict, "reconciliation deletion ownership was lost")
                    .with_receipt(receipt.clone())
            })?;
            source.delete().map_err(|_| {
                InternError::new(ErrorCode::SourceDeleteFailed, "reconciled source deletion failed")
                    .with_receipt(receipt.clone())
            })?;
        }
        let resolved = self.store.resolve_verified_operation(
            receipt.queue_item_id,
            receipt.id,
            OperationStage::Published,
        );
        drop(destination);
        resolved
    }

    fn verify_reconciled_destination(
        &self,
        receipt: &OperationReceipt,
    ) -> InternResult<Box<dyn LockedFile>> {
        if receipt.post_operation_hash.as_deref().is_some_and(|hash| hash != receipt.pre_operation_hash.as_str()) {
            return Err(InternError::new(
                ErrorCode::MoveVerificationFailed,
                "receipt does not contain an equal verified destination hash",
            ).with_receipt(receipt.clone()));
        }
        let mut destination = self.filesystem.lock_for_delete(&receipt.destination).map_err(|_| {
            InternError::new(
                ErrorCode::MoveVerificationFailed,
                "reconciled destination cannot be locked",
            ).with_receipt(receipt.clone())
        })?;
        let identity = destination.identity().map_err(|_| {
            InternError::new(
                ErrorCode::MoveVerificationFailed,
                "reconciled destination identity is unavailable",
            ).with_receipt(receipt.clone())
        })?;
        let hash = destination.hash().map_err(|_| {
            InternError::new(
                ErrorCode::MoveVerificationFailed,
                "reconciled destination hash is unavailable",
            ).with_receipt(receipt.clone())
        })?;
        let identity_after = destination.identity().map_err(|_| {
            InternError::new(
                ErrorCode::MoveVerificationFailed,
                "reconciled destination identity became unavailable",
            ).with_receipt(receipt.clone())
        })?;
        if hash != receipt.pre_operation_hash || identity_after != identity {
            return Err(InternError::new(
                ErrorCode::MoveVerificationFailed,
                "reconciled destination does not match its receipt",
            ).with_receipt(receipt.clone()));
        }
        Ok(destination)
    }

    fn transfer(
        &self,
        queue_item_id: i64,
        direction: OperationDirection,
        source: &Path,
        destination: &Path,
        expected_fingerprint: &str,
    ) -> InternResult<OperationReceipt> {
        if self.filesystem.exists(destination) {
            return Err(InternError::new(ErrorCode::DestinationUnavailable, "destination is occupied"));
        }
        let same_volume = self.filesystem.same_volume(source, destination).map_err(|_| {
            InternError::new(ErrorCode::DestinationUnavailable, "destination volume is unavailable")
        })?;
        if same_volume {
            let pre_hash = self.filesystem.hash(source).map_err(|_| {
                InternError::new(ErrorCode::FileChanged, "source is unavailable or changed")
            })?;
            if pre_hash != expected_fingerprint {
                return Err(InternError::new(ErrorCode::FileChanged, "source fingerprint changed"));
            }
            let receipt = self.planned_receipt(
                queue_item_id,
                direction,
                source,
                destination,
                None,
                pre_hash,
                OperationKind::Rename,
            )?;
            return self.same_volume_transfer(receipt);
        }

        let mut locked = self.filesystem.lock_for_delete(source).map_err(|_| {
            InternError::new(ErrorCode::FileChanged, "source cannot be exclusively verified")
        })?;
        let identity = locked.identity().map_err(|_| {
            InternError::new(ErrorCode::FileChanged, "source identity is unavailable")
        })?;
        let pre_hash = locked.hash().map_err(|_| {
            InternError::new(ErrorCode::FileChanged, "source cannot be verified")
        })?;
        if pre_hash != expected_fingerprint {
            return Err(InternError::new(ErrorCode::FileChanged, "source fingerprint changed"));
        }
        let temporary = temporary_path(destination);
        let receipt = self.planned_receipt(
            queue_item_id,
            direction,
            source,
            destination,
            Some(temporary),
            pre_hash,
            OperationKind::VerifiedCopy,
        )?;
        self.cross_volume_transfer(receipt, identity, locked)
    }

    fn planned_receipt(
        &self,
        queue_item_id: i64,
        direction: OperationDirection,
        source: &Path,
        destination: &Path,
        temporary_path: Option<PathBuf>,
        pre_operation_hash: String,
        kind: OperationKind,
    ) -> InternResult<OperationReceipt> {
        self.store.create_receipt(queue_item_id, OperationReceipt {
            id: 0,
            queue_item_id,
            direction,
            source: source.to_owned(),
            destination: destination.to_owned(),
            temporary_path,
            pre_operation_hash,
            post_operation_hash: None,
            kind,
            stage: OperationStage::Planned,
            source_exists: true,
            destination_exists: false,
            temporary_exists: false,
        })
    }

    fn same_volume_transfer(&self, mut receipt: OperationReceipt) -> InternResult<OperationReceipt> {
        self.filesystem.rename_no_replace(&receipt.source, &receipt.destination).map_err(|error| {
            let code = if error.kind() == io::ErrorKind::AlreadyExists {
                ErrorCode::DestinationUnavailable
            } else {
                ErrorCode::IoError
            };
            self.reconciliation_error(receipt.clone(), code, "atomic no-replace rename failed")
        })?;

        receipt.source_exists = false;
        receipt.destination_exists = true;
        receipt.stage = OperationStage::Published;
        receipt = self.store.update_receipt(OperationStage::Planned, &receipt).map_err(|_| {
            self.reconciliation_error(receipt.clone(), ErrorCode::StateConflict, "rename completed but receipt publication failed")
        })?;

        let mut published_locked = match self.filesystem.lock_for_delete(&receipt.destination) {
            Ok(locked) => locked,
            Err(_) => {
                return Err(self.reconciliation_error(
                    receipt,
                    ErrorCode::MoveVerificationFailed,
                    "renamed destination could not be locked for verification",
                ));
            }
        };
        let published_identity = match published_locked.identity() {
            Ok(identity) => identity,
            Err(_) => {
                return Err(self.reconciliation_error(
                    receipt,
                    ErrorCode::MoveVerificationFailed,
                    "renamed destination identity is unavailable",
                ));
            }
        };
        let post_hash = match published_locked.hash() {
            Ok(hash) => hash,
            Err(_) => {
                drop(published_locked);
                return self.rollback_after_rename(receipt, "renamed destination could not be verified");
            }
        };
        if post_hash != receipt.pre_operation_hash {
            receipt.post_operation_hash = Some(post_hash);
            drop(published_locked);
            return self.rollback_after_rename(receipt, "renamed destination hash differs");
        }
        if published_locked.identity().map_err(|_| {
            self.reconciliation_error(
                receipt.clone(),
                ErrorCode::MoveVerificationFailed,
                "renamed destination identity became unavailable",
            )
        })? != published_identity {
            return Err(self.reconciliation_error(
                receipt,
                ErrorCode::MoveVerificationFailed,
                "renamed destination identity changed during verification",
            ));
        }
        receipt.post_operation_hash = Some(post_hash);
        receipt.stage = OperationStage::Complete;
        let complete = self.store.update_receipt(OperationStage::Published, &receipt).map_err(|_| {
            self.reconciliation_error(receipt, ErrorCode::StateConflict, "rename completed but completion journal failed")
        })?;
        drop(published_locked);
        Ok(complete)
    }

    fn rollback_after_rename(&self, mut receipt: OperationReceipt, message: &str) -> InternResult<OperationReceipt> {
        receipt.stage = OperationStage::RollbackRequired;
        receipt = self.store.update_receipt(OperationStage::Published, &receipt).map_err(|_| {
            self.reconciliation_error(receipt.clone(), ErrorCode::StateConflict, "rollback intent could not be journaled")
        })?;
        match self.filesystem.rename_no_replace(&receipt.destination, &receipt.source) {
            Ok(()) => {
                receipt.source_exists = true;
                receipt.destination_exists = false;
                receipt.stage = OperationStage::RolledBack;
                match self.store.update_receipt(OperationStage::RollbackRequired, &receipt) {
                    Ok(journaled) => Err(self.reconciliation_error(journaled, ErrorCode::MoveVerificationFailed, message)),
                    Err(_) => Err(self.reconciliation_error(
                        receipt,
                        ErrorCode::StateConflict,
                        "rollback succeeded but its durable journal update failed",
                    )),
                }
            }
            Err(_) => {
                receipt.source_exists = self.filesystem.exists(&receipt.source);
                receipt.destination_exists = self.filesystem.exists(&receipt.destination);
                match self.store.update_receipt(OperationStage::RollbackRequired, &receipt) {
                    Ok(actual) => Err(self.reconciliation_error(
                        actual,
                        ErrorCode::MoveVerificationFailed,
                        "rollback failed; surviving paths require reconciliation",
                    )),
                    Err(_) => Err(self.reconciliation_error(
                        receipt,
                        ErrorCode::StateConflict,
                        "rollback failed and surviving paths could not be durably updated",
                    )),
                }
            }
        }
    }

    fn cross_volume_transfer(
        &self,
        mut receipt: OperationReceipt,
        identity: FileIdentity,
        mut locked: Box<dyn LockedFile>,
    ) -> InternResult<OperationReceipt> {
        let temporary = receipt.temporary_path.clone().ok_or_else(|| {
            self.reconciliation_error(
                receipt.clone(),
                ErrorCode::InvalidData,
                "cross-volume receipt is missing its temporary path",
            )
        })?;
        let mut temporary_locked = match self.filesystem.copy_new_locked(&receipt.source, &temporary) {
            Ok(locked) => locked,
            Err(_) => {
                receipt.temporary_exists = self.filesystem.exists(&temporary);
                return match self.store.update_receipt(OperationStage::Planned, &receipt) {
                    Ok(actual) => Err(self.reconciliation_error(
                        actual,
                        ErrorCode::DestinationUnavailable,
                        "temporary copy failed",
                    )),
                    Err(_) => Err(self.reconciliation_error(
                        receipt,
                        ErrorCode::StateConflict,
                        "temporary copy failed and its surviving path could not be durably updated",
                    )),
                };
            }
        };
        receipt.temporary_exists = true;
        receipt.stage = OperationStage::Copied;
        receipt = self.store.update_receipt(OperationStage::Planned, &receipt).map_err(|_| {
            self.reconciliation_error(receipt.clone(), ErrorCode::StateConflict, "copied stage could not be journaled")
        })?;

        let copied_identity = temporary_locked.identity().map_err(|_| {
            self.reconciliation_error(
                receipt.clone(),
                ErrorCode::MoveVerificationFailed,
                "temporary copy identity could not be verified",
            )
        })?;
        let copied_hash = match temporary_locked.hash() {
            Ok(hash) => hash,
            Err(_) => {
                return Err(self.temp_failure(
                    receipt,
                    temporary_locked,
                    ErrorCode::MoveVerificationFailed,
                    "temporary copy could not be verified",
                ));
            }
        };
        if copied_hash != receipt.pre_operation_hash {
            return Err(self.temp_failure(
                receipt,
                temporary_locked,
                ErrorCode::MoveVerificationFailed,
                "temporary copy hash differs",
            ));
        }
        receipt.post_operation_hash = Some(copied_hash);
        receipt.stage = OperationStage::Verified;
        receipt = self.store.update_receipt(OperationStage::Copied, &receipt).map_err(|_| {
            self.reconciliation_error(receipt.clone(), ErrorCode::StateConflict, "verified stage could not be journaled")
        })?;

        if temporary_locked.identity().map_err(|_| {
            self.reconciliation_error(
                receipt.clone(),
                ErrorCode::MoveVerificationFailed,
                "verified temporary identity became unavailable",
            )
        })? != copied_identity {
            return Err(self.temp_failure(
                receipt,
                temporary_locked,
                ErrorCode::MoveVerificationFailed,
                "verified temporary identity changed",
            ));
        }
        drop(temporary_locked);

        if let Err(_publish_error) = self.filesystem.rename_no_replace(&temporary, &receipt.destination) {
            receipt.temporary_exists = self.filesystem.exists(&temporary);
            receipt.destination_exists = self.filesystem.exists(&receipt.destination);
            return match self.store.update_receipt(OperationStage::Verified, &receipt) {
                Ok(actual) => Err(self.reconciliation_error(actual, ErrorCode::DestinationUnavailable, "verified copy could not be atomically published")),
                Err(_) => Err(self.reconciliation_error(
                    receipt,
                    ErrorCode::StateConflict,
                    "publish failed and its surviving paths could not be durably updated",
                )),
            };
        }
        receipt.temporary_exists = false;
        receipt.destination_exists = true;
        receipt.stage = OperationStage::Published;
        receipt = self.store.update_receipt(OperationStage::Verified, &receipt).map_err(|_| {
            self.reconciliation_error(receipt.clone(), ErrorCode::StateConflict, "published stage could not be journaled")
        })?;

        let mut published_locked = self.filesystem.lock_for_delete(&receipt.destination).map_err(|_| {
            self.reconciliation_error(
                receipt.clone(),
                ErrorCode::MoveVerificationFailed,
                "published destination could not be locked for final verification",
            )
        })?;
        let published_identity = published_locked.identity().map_err(|_| {
            self.reconciliation_error(
                receipt.clone(),
                ErrorCode::MoveVerificationFailed,
                "published destination identity is unavailable",
            )
        })?;
        let published_hash = published_locked.hash().map_err(|_| {
            self.reconciliation_error(receipt.clone(), ErrorCode::MoveVerificationFailed, "published destination could not be verified")
        })?;
        if published_hash != receipt.pre_operation_hash {
            receipt.post_operation_hash = Some(published_hash);
            return match self.store.update_receipt(OperationStage::Published, &receipt) {
                Ok(actual) => Err(self.reconciliation_error(actual, ErrorCode::MoveVerificationFailed, "published destination hash differs")),
                Err(_) => Err(self.reconciliation_error(
                    receipt,
                    ErrorCode::StateConflict,
                    "published mismatch could not be durably updated",
                )),
            };
        }
        receipt.post_operation_hash = Some(published_hash);
        receipt = self.store.update_receipt(OperationStage::Published, &receipt).map_err(|_| {
            self.reconciliation_error(receipt.clone(), ErrorCode::StateConflict, "published hash could not be journaled")
        })?;

        let identity_before_delete = locked.identity().map_err(|_| {
            self.reconciliation_error(receipt.clone(), ErrorCode::FileChanged, "locked source identity became unavailable")
        })?;
        if identity_before_delete != identity {
            return Err(self.reconciliation_error(receipt, ErrorCode::FileChanged, "locked source identity changed"));
        }
        let source_hash = locked.hash().map_err(|_| {
            self.reconciliation_error(receipt.clone(), ErrorCode::FileChanged, "locked source could not be reverified")
        })?;
        if source_hash != receipt.pre_operation_hash {
            return Err(self.reconciliation_error(receipt, ErrorCode::FileChanged, "locked source hash changed"));
        }
        if published_locked.identity().map_err(|_| {
            self.reconciliation_error(
                receipt.clone(),
                ErrorCode::MoveVerificationFailed,
                "published destination identity became unavailable",
            )
        })? != published_identity {
            return Err(self.reconciliation_error(
                receipt,
                ErrorCode::MoveVerificationFailed,
                "published destination identity changed before source deletion",
            ));
        }
        self.store.renew_operation_lease(
            receipt.queue_item_id,
            receipt.id,
            OperationStage::Published,
        ).map_err(|_| {
            self.reconciliation_error(
                receipt.clone(),
                ErrorCode::StateConflict,
                "source deletion ownership could not be renewed",
            )
        })?;
        locked.delete().map_err(|_| {
            self.reconciliation_error(receipt.clone(), ErrorCode::SourceDeleteFailed, "locked source deletion failed")
        })?;
        drop(published_locked);

        receipt.source_exists = false;
        receipt.stage = OperationStage::Complete;
        self.store.update_receipt(OperationStage::Published, &receipt).map_err(|_| {
            self.reconciliation_error(receipt, ErrorCode::StateConflict, "source deleted but completion journal failed")
        })
    }

    fn temp_failure(
        &self,
        mut receipt: OperationReceipt,
        temporary_locked: Box<dyn LockedFile>,
        code: ErrorCode,
        message: &str,
    ) -> InternError {
        if self.store.renew_operation_lease(
            receipt.queue_item_id,
            receipt.id,
            receipt.stage,
        ).is_err() {
            receipt.temporary_exists = true;
            return self.reconciliation_error(
                receipt,
                ErrorCode::StateConflict,
                "temporary cleanup ownership could not be renewed",
            );
        }
        receipt.temporary_exists = temporary_locked.delete().is_err();
        match self.store.update_receipt(receipt.stage, &receipt) {
            Ok(actual) => self.reconciliation_error(actual, code, message),
            Err(_) => self.reconciliation_error(
                receipt,
                ErrorCode::StateConflict,
                "temporary cleanup result could not be durably updated",
            ),
        }
    }

    fn reconciliation_error(&self, receipt: OperationReceipt, code: ErrorCode, message: &str) -> InternError {
        match self.store.record_applying_rollback(receipt.queue_item_id, receipt.id, code) {
            Ok(_) => InternError::new(code, message).with_receipt(receipt),
            Err(_) => InternError::new(
                ErrorCode::StateConflict,
                "operation state is uncertain and applying ownership could not be journaled",
            ).with_receipt(receipt),
        }
    }

    fn available_destination(&self, requested: &Path) -> InternResult<PathBuf> {
        let parent = requested.parent().ok_or_else(|| {
            InternError::new(ErrorCode::DestinationUnavailable, "destination has no parent")
        })?;
        let stem = requested.file_stem().and_then(|value| value.to_str()).ok_or_else(|| {
            InternError::new(ErrorCode::DestinationUnavailable, "destination name is invalid")
        })?;
        let extension = requested.extension().and_then(|value| value.to_str());
        for index in 1_u32.. {
            let name = if index == 1 {
                requested.file_name().ok_or_else(|| {
                    InternError::new(ErrorCode::DestinationUnavailable, "destination name is missing")
                })?.to_owned()
            } else {
                match extension {
                    Some(value) => format!("{stem} ({index}).{value}").into(),
                    None => format!("{stem} ({index})").into(),
                }
            };
            let candidate = parent.join(name);
            if !self.filesystem.exists(&candidate) {
                return Ok(candidate);
            }
        }
        unreachable!("u32 destination suffix space exhausted")
    }
}

fn hash_reader(mut file: fs::File) -> io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 { break; }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn temporary_path(destination: &Path) -> PathBuf {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let process = std::process::id();
    let name = destination.file_name().and_then(|value| value.to_str()).unwrap_or("destination");
    destination.with_file_name(format!(".{name}.{process}.{nanos}.{sequence}.intern-tmp"))
}

#[cfg(windows)]
mod windows_file {
    #![allow(unsafe_code)]

    use std::{
        ffi::OsStr,
        fs::{self, OpenOptions},
        io::{self, Read, Seek, SeekFrom, Write},
        mem::{size_of, MaybeUninit},
        os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
        path::Path,
    };

    use sha2::{Digest, Sha256};
    use windows_sys::Win32::{
        Foundation::{DELETE, GENERIC_READ, GENERIC_WRITE, HANDLE},
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileDispositionInfo,
            GetFileInformationByHandle, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            SetFileInformationByHandle,
        },
    };

    use super::{FileIdentity, LockedFile};

    pub(super) struct WindowsLockedFile {
        file: fs::File,
        identity: FileIdentity,
    }

    impl LockedFile for WindowsLockedFile {
        fn hash(&mut self) -> io::Result<String> {
            self.file.seek(SeekFrom::Start(0))?;
            let mut hasher = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = self.file.read(&mut buffer)?;
                if read == 0 { break; }
                hasher.update(&buffer[..read]);
            }
            Ok(format!("{:x}", hasher.finalize()))
        }

        fn identity(&self) -> io::Result<FileIdentity> {
            file_identity(&self.file)
        }

        fn delete(self: Box<Self>) -> io::Result<()> {
            if file_identity(&self.file)? != self.identity {
                return Err(io::Error::new(io::ErrorKind::Other, "locked file identity changed"));
            }
            let disposition = FILE_DISPOSITION_INFO { DeleteFile: 1 };
            // SAFETY: `self.file` owns a live handle opened with DELETE access, and
            // `disposition` remains valid for the exact byte size supplied here.
            let success = unsafe {
                SetFileInformationByHandle(
                    self.file.as_raw_handle() as HANDLE,
                    FileDispositionInfo,
                    &disposition as *const FILE_DISPOSITION_INFO as *const core::ffi::c_void,
                    size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            };
            if success == 0 { return Err(io::Error::last_os_error()); }
            drop(self);
            Ok(())
        }
    }

    pub(super) fn copy_new_locked(
        source: &Path,
        destination: &Path,
    ) -> io::Result<Box<dyn LockedFile>> {
        let mut input = fs::File::open(source)?;
        let output = OpenOptions::new()
            .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
            .share_mode(FILE_SHARE_READ)
            .create_new(true)
            .open(destination)?;
        let identity = file_identity(&output)?;
        let mut locked = WindowsLockedFile { file: output, identity };
        let copied = io::copy(&mut input, &mut locked.file)
            .and_then(|_| locked.file.flush())
            .and_then(|_| locked.file.sync_all());
        if let Err(copy_error) = copied {
            return match Box::new(locked).delete() {
                Ok(()) => Err(copy_error),
                Err(cleanup_error) => Err(io::Error::new(
                    cleanup_error.kind(),
                    format!(
                        "temporary copy failed ({copy_error}); same-handle cleanup failed ({cleanup_error})"
                    ),
                )),
            };
        }
        Ok(Box::new(locked))
    }

    pub(super) fn lock_for_delete(path: &Path) -> io::Result<Box<dyn LockedFile>> {
        let file = OpenOptions::new()
            .access_mode(GENERIC_READ | DELETE)
            .share_mode(FILE_SHARE_READ)
            .open(path)?;
        let identity = file_identity(&file)?;
        Ok(Box::new(WindowsLockedFile { file, identity }))
    }

    pub(super) fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
        let source = wide(source.as_os_str());
        let destination = wide(destination.as_os_str());
        // SAFETY: both vectors are NUL-terminated UTF-16 paths and remain alive
        // for the duration of the call. Omitting MOVEFILE_REPLACE_EXISTING makes
        // this an atomic fail-if-destination-exists rename.
        let success = unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), MOVEFILE_WRITE_THROUGH) };
        if success == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    pub(super) fn same_volume(source: &Path, destination: &Path) -> io::Result<bool> {
        let source_file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(source)?;
        let parent = destination.parent().unwrap_or_else(|| Path::new("."));
        let parent_file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(parent)?;
        Ok(file_identity(&source_file)?.volume == file_identity(&parent_file)?.volume)
    }

    fn file_identity(file: &fs::File) -> io::Result<FileIdentity> {
        let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: the handle is live and `information` points to writable storage
        // of the exact structure expected by GetFileInformationByHandle.
        let success = unsafe {
            GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
        };
        if success == 0 { return Err(io::Error::last_os_error()); }
        // SAFETY: a nonzero return guarantees the API initialized the structure.
        let information = unsafe { information.assume_init() };
        Ok(FileIdentity {
            volume: information.dwVolumeSerialNumber as u64,
            file: ((information.nFileIndexHigh as u128) << 32) | information.nFileIndexLow as u128,
        })
    }

    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(Some(0)).collect()
    }
}

#[cfg(windows)]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    windows_file::rename_no_replace(source, destination)
}

#[cfg(windows)]
fn copy_new_locked(source: &Path, destination: &Path) -> io::Result<Box<dyn LockedFile>> {
    windows_file::copy_new_locked(source, destination)
}

#[cfg(not(windows))]
fn copy_new_locked(source: &Path, destination: &Path) -> io::Result<Box<dyn LockedFile>> {
    let mut input = fs::File::open(source)?;
    let output = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(destination)?;
    let identity = portable_identity(&output.metadata()?);
    let mut locked = PortableLockedFile {
        file: output,
        path: destination.to_owned(),
        identity,
    };
    let copied = io::copy(&mut input, &mut locked.file)
        .and_then(|_| locked.file.flush())
        .and_then(|_| locked.file.sync_all());
    if let Err(copy_error) = copied {
        return match Box::new(locked).delete() {
            Ok(()) => Err(copy_error),
            Err(cleanup_error) => Err(io::Error::new(
                cleanup_error.kind(),
                format!(
                    "temporary copy failed ({copy_error}); identity-checked cleanup failed ({cleanup_error})"
                ),
            )),
        };
    }
    Ok(Box::new(locked))
}

#[cfg(not(windows))]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::hard_link(source, destination)?;
    if let Err(error) = fs::remove_file(source) {
        match fs::remove_file(destination) {
            Ok(()) => return Err(error),
            Err(rollback_error) => {
                return Err(io::Error::new(
                    rollback_error.kind(),
                    format!("source unlink failed ({error}); destination rollback failed ({rollback_error})"),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn same_volume(source: &Path, destination: &Path) -> io::Result<bool> {
    windows_file::same_volume(source, destination)
}

#[cfg(unix)]
fn same_volume(source: &Path, destination: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let destination_parent = destination.parent().unwrap_or_else(|| Path::new("."));
    Ok(fs::metadata(source)?.dev() == fs::metadata(destination_parent)?.dev())
}

#[cfg(not(any(unix, windows)))]
fn same_volume(_source: &Path, _destination: &Path) -> io::Result<bool> {
    Ok(false)
}

#[cfg(windows)]
fn lock_for_delete(path: &Path) -> io::Result<Box<dyn LockedFile>> {
    windows_file::lock_for_delete(path)
}

#[cfg(not(windows))]
fn lock_for_delete(path: &Path) -> io::Result<Box<dyn LockedFile>> {
    Ok(Box::new(PortableLockedFile::open(path)?))
}

#[cfg(not(windows))]
struct PortableLockedFile {
    file: fs::File,
    path: PathBuf,
    identity: FileIdentity,
}

#[cfg(not(windows))]
impl PortableLockedFile {
    fn open(path: &Path) -> io::Result<Self> {
        let file = fs::File::open(path)?;
        let identity = portable_identity(&file.metadata()?);
        Ok(Self { file, path: path.to_owned(), identity })
    }
}

#[cfg(not(windows))]
impl LockedFile for PortableLockedFile {
    fn hash(&mut self) -> io::Result<String> {
        self.file.seek(SeekFrom::Start(0))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = self.file.read(&mut buffer)?;
            if read == 0 { break; }
            hasher.update(&buffer[..read]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    fn identity(&self) -> io::Result<FileIdentity> {
        Ok(self.identity.clone())
    }

    fn delete(self: Box<Self>) -> io::Result<()> {
        if portable_identity(&fs::metadata(&self.path)?) != self.identity {
            return Err(io::Error::new(io::ErrorKind::Other, "locked file identity changed"));
        }
        fs::remove_file(&self.path)
    }
}

#[cfg(unix)]
fn portable_identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity { volume: metadata.dev(), file: metadata.ino() as u128 }
}

#[cfg(not(any(unix, windows)))]
fn portable_identity(metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity { volume: 0, file: metadata.len() as u128 }
}
