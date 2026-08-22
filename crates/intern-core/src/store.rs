use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{
    ErrorCode, InternError, InternResult, OperationDirection, OperationKind, OperationReceipt,
    OperationStage, QueueItem, QueueStatus,
};

const LEASE_SECONDS: i64 = 60;
static SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub struct QueueStore {
    connection: Mutex<Connection>,
    session_id: String,
}

/// A completed item whose content matches a newly added file.
///
/// `filed_as` is the leaf name the content actually lives under: the
/// destination of the latest completed apply receipt. A keep-original
/// completion moved nothing and has no such receipt, so `filed_as` is `None`
/// and the caller falls back to the item's original filename.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuplicateInfo {
    pub queue_item_id: i64,
    pub source_path: PathBuf,
    pub filed_as: Option<String>,
}

/// One finished journalled file operation, for the history view.
///
/// `at` is the receipt's `updated_at`: the moment the operation reached its
/// terminal stage, not the moment it was planned. `original_path` and
/// `new_path` are the receipt's source and destination as recorded — for an
/// undo the "original" is therefore the previously applied name and the "new"
/// path is the restored one, which is exactly what a history reader expects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryEntry {
    pub receipt_id: i64,
    pub queue_item_id: i64,
    pub at: i64,
    pub direction: OperationDirection,
    pub kind: OperationKind,
    pub stage: OperationStage,
    pub original_path: PathBuf,
    pub new_path: PathBuf,
}

/// The most receipts a history listing will ever return.
pub const HISTORY_LIMIT: usize = 500;

impl QueueStore {
    pub fn open(path: impl AsRef<Path>) -> InternResult<Self> {
        let mut connection = Connection::open(path).map_err(InternError::from)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(InternError::from)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS schema_migrations (
               version INTEGER PRIMARY KEY,
               applied_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS queue_sessions (
               session_id TEXT PRIMARY KEY,
               heartbeat_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS queue_items (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               source_path TEXT NOT NULL,
               source_path_key TEXT NOT NULL,
               source_hash TEXT NOT NULL,
               status TEXT NOT NULL,
               processing_failures INTEGER NOT NULL DEFAULT 0,
               error_code TEXT,
               owner_session TEXT REFERENCES queue_sessions(session_id) ON DELETE SET NULL,
               lease_expires_at INTEGER,
               previous_status TEXT,
               active_receipt_id INTEGER,
               reconciliation_receipt_id INTEGER,
               applying_epoch INTEGER NOT NULL DEFAULT 0,
               created_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL,
               UNIQUE(source_path_key, source_hash)
             );
             CREATE TABLE IF NOT EXISTS proposals (
               queue_item_id INTEGER PRIMARY KEY REFERENCES queue_items(id) ON DELETE CASCADE,
               proposal_json TEXT NOT NULL,
               created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS operation_receipts (
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
             INSERT OR IGNORE INTO schema_migrations(version, applied_at)
               VALUES (1, unixepoch());",
            )
            .map_err(InternError::from)?;
        migrate_legacy_schema(&mut connection)?;
        let session_id = new_session_id();
        connection
            .execute(
                "INSERT INTO queue_sessions(session_id, heartbeat_at) VALUES (?1, ?2)",
                params![session_id, now()],
            )
            .map_err(InternError::from)?;
        Ok(Self {
            connection: Mutex::new(connection),
            session_id,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn enqueue(&self, source_path: &Path, source_hash: &str) -> InternResult<QueueItem> {
        let path = source_path.to_string_lossy().into_owned();
        let path_key = windows_path_key(&path);
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        transaction.execute(
            "INSERT INTO queue_items(source_path, source_path_key, source_hash, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?4)
             ON CONFLICT(source_path_key, source_hash) DO NOTHING",
            params![path, path_key, source_hash, timestamp],
        ).map_err(InternError::from)?;
        let item = query_one(
            &transaction,
            "WHERE source_path_key = ?1 AND source_hash = ?2",
            params![path_key, source_hash],
        )?;
        transaction.commit().map_err(InternError::from)?;
        Ok(item)
    }

    pub fn claim_next(&self) -> InternResult<Option<QueueItem>> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        touch_session(&transaction, &self.session_id)?;
        let id = transaction
            .query_row(
                "SELECT id FROM queue_items
             WHERE status = 'queued'
               AND NOT EXISTS (
                 SELECT 1 FROM queue_items active
                 WHERE active.status IN ('extracting', 'analyzing', 'applying')
               )
             ORDER BY id LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(InternError::from)?;
        let Some(id) = id else {
            transaction.commit().map_err(InternError::from)?;
            return Ok(None);
        };
        let changed = transaction
            .execute(
                "UPDATE queue_items
             SET status = 'extracting', error_code = NULL, owner_session = ?1,
                 lease_expires_at = ?2, updated_at = ?3
             WHERE id = ?4 AND status = 'queued'",
                params![self.session_id, lease_deadline(), now(), id],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            transaction.rollback().map_err(InternError::from)?;
            return Ok(None);
        }
        let item = query_one(&transaction, "WHERE id = ?1", params![id])?;
        transaction.commit().map_err(InternError::from)?;
        Ok(Some(item))
    }

    pub fn renew_lease(&self, id: i64) -> InternResult<QueueItem> {
        let connection = self.lock()?;
        touch_session(&connection, &self.session_id)?;
        let changed = connection
            .execute(
                "UPDATE queue_items SET lease_expires_at = ?1, updated_at = ?2
             WHERE id = ?3 AND owner_session = ?4
               AND status IN ('extracting', 'analyzing', 'applying')",
                params![lease_deadline(), now(), id, self.session_id],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "queue lease is not owned by this session",
            ));
        }
        query_one(&connection, "WHERE id = ?1", params![id])
    }

    pub(crate) fn renew_operation_lease(
        &self,
        id: i64,
        receipt_id: i64,
        expected_stage: OperationStage,
    ) -> InternResult<QueueItem> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        touch_session(&transaction, &self.session_id)?;
        let changed = transaction
            .execute(
                "UPDATE queue_items SET lease_expires_at = ?1, updated_at = ?2
             WHERE id = ?3 AND status = 'applying' AND owner_session = ?4
               AND active_receipt_id = ?5
               AND EXISTS (
                 SELECT 1 FROM operation_receipts receipts
                 WHERE receipts.id = ?5 AND receipts.queue_item_id = ?3
                   AND receipts.stage = ?6
               )",
                params![
                    lease_deadline(),
                    now(),
                    id,
                    self.session_id,
                    receipt_id,
                    expected_stage.as_db()
                ],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            transaction.rollback().map_err(InternError::from)?;
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "operation receipt lease renewal compare-and-swap failed",
            ));
        }
        let item = query_one(&transaction, "WHERE id = ?1", params![id])?;
        transaction.commit().map_err(InternError::from)?;
        Ok(item)
    }

    pub fn transition(
        &self,
        id: i64,
        expected: QueueStatus,
        next: QueueStatus,
        error: Option<ErrorCode>,
    ) -> InternResult<QueueItem> {
        if !expected.can_transition_to(next) {
            return Err(InternError::new(
                ErrorCode::InvalidTransition,
                "queue transition is not permitted",
            ));
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        let expected_active = is_active(expected);
        let next_active = is_active(next);
        let changed = transaction
            .execute(
                "UPDATE queue_items
             SET status = ?1, error_code = ?2,
                 owner_session = CASE WHEN ?3 THEN ?4 ELSE NULL END,
                 lease_expires_at = CASE WHEN ?3 THEN ?5 ELSE NULL END,
                 updated_at = ?6
             WHERE id = ?7 AND status = ?8
               AND (NOT ?9 OR owner_session = ?4)",
                params![
                    next.as_db(),
                    error.map(ErrorCode::as_str),
                    next_active,
                    self.session_id,
                    lease_deadline(),
                    now(),
                    id,
                    expected.as_db(),
                    expected_active,
                ],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            transaction.rollback().map_err(InternError::from)?;
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "queue item changed or is owned by another session",
            ));
        }
        let item = query_one(&transaction, "WHERE id = ?1", params![id])?;
        transaction.commit().map_err(InternError::from)?;
        Ok(item)
    }

    pub fn manual_retry(&self, id: i64) -> InternResult<QueueItem> {
        self.cas_status(id, QueueStatus::Failed, QueueStatus::Queued, true)
    }

    /// Finds the most recently completed item holding the same content at a
    /// different path.
    ///
    /// Only `completed` rows count: an item whose apply was undone is back in
    /// `ready`, its content back at its original path, and a still-pending item
    /// has not been filed anywhere yet. Ties on the completion timestamp fall
    /// to the newest row.
    pub fn find_completed_duplicate(
        &self,
        source_hash: &str,
        excluding_path_key: &str,
    ) -> InternResult<Option<DuplicateInfo>> {
        let connection = self.lock()?;
        let Some((queue_item_id, source_path)) = connection
            .query_row(
                "SELECT id, source_path FROM queue_items
                 WHERE status = 'completed' AND source_hash = ?1 AND source_path_key <> ?2
                 ORDER BY updated_at DESC, id DESC LIMIT 1",
                params![source_hash, excluding_path_key],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(InternError::from)?
        else {
            return Ok(None);
        };
        // The newest receipt is where the content actually is now. Anything
        // other than a completed apply (no receipt at all, or an undo followed
        // by keep-original) means the file kept its original name.
        let filed_as = connection
            .query_row(
                "SELECT destination_path FROM operation_receipts
                 WHERE queue_item_id = ?1
                   AND id = (
                     SELECT MAX(latest.id) FROM operation_receipts latest
                     WHERE latest.queue_item_id = ?1
                   )
                   AND direction = 'apply' AND stage = 'complete'",
                params![queue_item_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(InternError::from)?
            .and_then(|destination| {
                Path::new(&destination)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            });
        Ok(Some(DuplicateInfo {
            queue_item_id,
            source_path: PathBuf::from(source_path),
            filed_as,
        }))
    }

    /// Requeues a duplicate-flagged review item so it analyzes normally.
    ///
    /// The compare-and-swap covers the error code as well as the status: only
    /// the pre-processing DUPLICATE flag may take this shortcut back to the
    /// queue, and a concurrent decision on the item makes the retry fail
    /// closed.
    pub fn retry_duplicate(&self, id: i64) -> InternResult<QueueItem> {
        let connection = self.lock()?;
        let changed = connection
            .execute(
                "UPDATE queue_items
             SET status = 'queued', processing_failures = 0, error_code = NULL,
                 owner_session = NULL, lease_expires_at = NULL,
                 previous_status = NULL, active_receipt_id = NULL,
                 reconciliation_receipt_id = NULL, updated_at = ?1
             WHERE id = ?2 AND status = 'needs_review' AND error_code = 'DUPLICATE'",
                params![now(), id],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "item is not a duplicate awaiting review",
            ));
        }
        query_one(&connection, "WHERE id = ?1", params![id])
    }

    pub fn complete_keep_original(
        &self,
        id: i64,
        expected: QueueStatus,
    ) -> InternResult<QueueItem> {
        if !matches!(expected, QueueStatus::Ready | QueueStatus::NeedsReview) {
            return Err(InternError::new(
                ErrorCode::InvalidTransition,
                "keep-original requires a reviewable item",
            ));
        }
        self.cas_status(id, expected, QueueStatus::Completed, false)
    }

    pub fn begin_applying(&self, id: i64, expected: QueueStatus) -> InternResult<QueueItem> {
        if !matches!(expected, QueueStatus::Ready | QueueStatus::Completed) {
            return Err(InternError::new(
                ErrorCode::InvalidTransition,
                "apply requires ready or completed state",
            ));
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        touch_session(&transaction, &self.session_id)?;
        let changed = transaction
            .execute(
                "UPDATE queue_items
             SET status = 'applying', previous_status = ?1, owner_session = ?2,
                 lease_expires_at = ?3, active_receipt_id = NULL,
                 reconciliation_receipt_id = NULL, applying_epoch = applying_epoch + 1,
                 error_code = NULL, updated_at = ?4
             WHERE id = ?5 AND status = ?1 AND active_receipt_id IS NULL
               AND NOT EXISTS (
                 SELECT 1 FROM queue_items active
                 WHERE active.id <> ?5 AND active.status IN ('extracting', 'analyzing', 'applying')
               )",
                params![
                    expected.as_db(),
                    self.session_id,
                    lease_deadline(),
                    now(),
                    id
                ],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            transaction.rollback().map_err(InternError::from)?;
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "item cannot enter applying",
            ));
        }
        let item = query_one(&transaction, "WHERE id = ?1", params![id])?;
        transaction.commit().map_err(InternError::from)?;
        Ok(item)
    }

    pub fn complete_apply(&self, id: i64, receipt_id: i64) -> InternResult<QueueItem> {
        self.finish_applying(
            id,
            receipt_id,
            QueueStatus::Completed,
            QueueStatus::Ready,
            OperationDirection::Apply,
        )
    }

    pub fn complete_undo(&self, id: i64, receipt_id: i64) -> InternResult<QueueItem> {
        self.finish_applying(
            id,
            receipt_id,
            QueueStatus::Ready,
            QueueStatus::Completed,
            OperationDirection::Undo,
        )
    }

    pub fn claim_applying_reconciliation(&self, id: i64) -> InternResult<QueueItem> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        touch_session(&transaction, &self.session_id)?;
        let timestamp = now();
        let changed = transaction
            .execute(
                "UPDATE queue_items
             SET owner_session = ?1, lease_expires_at = ?2, updated_at = ?3
             WHERE id = ?4 AND status = 'applying'
               AND (
                 owner_session IS NULL
                 OR NOT EXISTS (
                   SELECT 1 FROM queue_sessions sessions
                   WHERE sessions.session_id = queue_items.owner_session
                 )
                 OR (
                   lease_expires_at <= ?3
                   AND NOT EXISTS (
                     SELECT 1 FROM queue_sessions sessions
                     WHERE sessions.session_id = queue_items.owner_session
                       AND sessions.heartbeat_at > ?5
                   )
                 )
               )",
                params![
                    self.session_id,
                    timestamp + LEASE_SECONDS,
                    timestamp,
                    id,
                    timestamp - LEASE_SECONDS
                ],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            transaction.rollback().map_err(InternError::from)?;
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "applying owner is still live",
            ));
        }
        let item = query_one(&transaction, "WHERE id = ?1", params![id])?;
        transaction.commit().map_err(InternError::from)?;
        Ok(item)
    }

    pub(crate) fn resolve_reconciled_rollback(
        &self,
        id: i64,
        receipt_id: i64,
        expected_stage: OperationStage,
    ) -> InternResult<QueueItem> {
        if !matches!(
            expected_stage,
            OperationStage::Planned
                | OperationStage::Copied
                | OperationStage::Verified
                | OperationStage::RollbackRequired
                | OperationStage::RolledBack
        ) {
            return Err(InternError::new(
                ErrorCode::InvalidTransition,
                "receipt cannot resolve as rolled back",
            ));
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        touch_session(&transaction, &self.session_id)?;
        if expected_stage != OperationStage::RolledBack {
            let changed = transaction
                .execute(
                    "UPDATE operation_receipts
                 SET stage = 'rolled_back', source_exists = 1, destination_exists = 0,
                     temporary_exists = 0,
                     updated_at = ?1
                 WHERE id = ?2 AND queue_item_id = ?3 AND stage = ?4",
                    params![now(), receipt_id, id, expected_stage.as_db()],
                )
                .map_err(InternError::from)?;
            if changed != 1 {
                transaction.rollback().map_err(InternError::from)?;
                return Err(InternError::new(
                    ErrorCode::StateConflict,
                    "receipt rollback stage compare-and-swap failed",
                ));
            }
        } else {
            let changed = transaction
                .execute(
                    "UPDATE operation_receipts SET temporary_exists = 0, updated_at = ?1
                 WHERE id = ?2 AND queue_item_id = ?3 AND stage = 'rolled_back'",
                    params![now(), receipt_id, id],
                )
                .map_err(InternError::from)?;
            if changed != 1 {
                transaction.rollback().map_err(InternError::from)?;
                return Err(InternError::new(
                    ErrorCode::StateConflict,
                    "rolled-back receipt cleanup compare-and-swap failed",
                ));
            }
        }
        let changed = transaction
            .execute(
                "UPDATE queue_items
             SET status = previous_status, owner_session = NULL, lease_expires_at = NULL,
                 previous_status = NULL, active_receipt_id = NULL,
                 reconciliation_receipt_id = NULL, error_code = NULL, updated_at = ?1
             WHERE id = ?2 AND status = 'applying' AND owner_session = ?3
               AND active_receipt_id = ?4
               AND EXISTS (
                 SELECT 1 FROM operation_receipts receipts
                 WHERE receipts.id = ?4 AND receipts.queue_item_id = ?2
                   AND receipts.stage = 'rolled_back'
                   AND (
                     (receipts.direction = 'apply' AND queue_items.previous_status = 'ready')
                     OR (receipts.direction = 'undo' AND queue_items.previous_status = 'completed')
                   )
               )",
                params![now(), id, self.session_id, receipt_id],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            transaction.rollback().map_err(InternError::from)?;
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "rolled-back reconciliation compare-and-swap failed",
            ));
        }
        let item = query_one(&transaction, "WHERE id = ?1", params![id])?;
        transaction.commit().map_err(InternError::from)?;
        Ok(item)
    }

    pub(crate) fn resolve_empty_applying(&self, id: i64) -> InternResult<QueueItem> {
        // Only begin_applying() advances this epoch marker. Pre-v4 applying rows stay at
        // zero, so ambiguous legacy operations still fail closed instead of being
        // mistaken for a crash that happened before the receipt transaction.
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        touch_session(&transaction, &self.session_id)?;
        let changed = transaction
            .execute(
                "UPDATE queue_items
             SET status = previous_status, owner_session = NULL, lease_expires_at = NULL,
                 previous_status = NULL, reconciliation_receipt_id = NULL,
                 error_code = NULL, updated_at = ?1
             WHERE id = ?2 AND status = 'applying' AND owner_session = ?3
               AND active_receipt_id IS NULL
               AND applying_epoch > 0
               AND previous_status IN ('ready', 'completed')",
                params![now(), id, self.session_id],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            transaction.rollback().map_err(InternError::from)?;
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "empty applying reconciliation compare-and-swap failed",
            ));
        }
        let item = query_one(&transaction, "WHERE id = ?1", params![id])?;
        transaction.commit().map_err(InternError::from)?;
        Ok(item)
    }

    pub(crate) fn resolve_verified_operation(
        &self,
        id: i64,
        receipt_id: i64,
        expected_stage: OperationStage,
    ) -> InternResult<QueueItem> {
        if !matches!(
            expected_stage,
            OperationStage::Planned
                | OperationStage::Copied
                | OperationStage::Verified
                | OperationStage::Published
                | OperationStage::Complete
        ) {
            return Err(InternError::new(
                ErrorCode::InvalidTransition,
                "receipt cannot resolve as a verified operation",
            ));
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        touch_session(&transaction, &self.session_id)?;
        let receipt = transaction
            .query_row(
                &receipt_select(
                    "WHERE id = ?1 AND queue_item_id = ?2 AND stage = ?3
                   AND EXISTS (
                     SELECT 1 FROM queue_items
                     WHERE id = ?2 AND status = 'applying' AND owner_session = ?4
                       AND active_receipt_id = ?1
                   )",
                ),
                params![receipt_id, id, expected_stage.as_db(), self.session_id],
                row_to_receipt,
            )
            .optional()
            .map_err(InternError::from)?
            .ok_or_else(|| {
                InternError::new(
                    ErrorCode::StateConflict,
                    "verified receipt reconciliation compare-and-swap failed",
                )
            })?;
        let (required_previous, next) = match receipt.direction {
            OperationDirection::Apply => (QueueStatus::Ready, QueueStatus::Completed),
            OperationDirection::Undo => (QueueStatus::Completed, QueueStatus::Ready),
        };
        if expected_stage != OperationStage::Complete {
            let changed = transaction
                .execute(
                    "UPDATE operation_receipts
                 SET stage = 'complete', source_exists = 0, destination_exists = 1,
                     temporary_exists = 0, post_hash = pre_hash, updated_at = ?1
                 WHERE id = ?2 AND queue_item_id = ?3 AND stage = ?4",
                    params![now(), receipt_id, id, expected_stage.as_db()],
                )
                .map_err(InternError::from)?;
            if changed != 1 {
                transaction.rollback().map_err(InternError::from)?;
                return Err(InternError::new(
                    ErrorCode::StateConflict,
                    "published receipt completion compare-and-swap failed",
                ));
            }
        }
        let changed = transaction
            .execute(
                "UPDATE queue_items
             SET status = ?1, owner_session = NULL, lease_expires_at = NULL,
                 previous_status = NULL, active_receipt_id = NULL,
                 reconciliation_receipt_id = NULL, error_code = NULL, updated_at = ?2
             WHERE id = ?3 AND status = 'applying' AND owner_session = ?4
               AND previous_status = ?5 AND active_receipt_id = ?6",
                params![
                    next.as_db(),
                    now(),
                    id,
                    self.session_id,
                    required_previous.as_db(),
                    receipt_id
                ],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            transaction.rollback().map_err(InternError::from)?;
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "verified operation queue reconciliation failed",
            ));
        }
        let item = query_one(&transaction, "WHERE id = ?1", params![id])?;
        transaction.commit().map_err(InternError::from)?;
        Ok(item)
    }

    pub fn record_applying_rollback(
        &self,
        id: i64,
        receipt_id: i64,
        error: ErrorCode,
    ) -> InternResult<QueueItem> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        touch_session(&transaction, &self.session_id)?;
        let changed = transaction
            .execute(
                "UPDATE queue_items
             SET reconciliation_receipt_id = ?1, error_code = ?2,
                 lease_expires_at = ?3, updated_at = ?4
             WHERE id = ?5 AND status = 'applying' AND owner_session = ?6
               AND active_receipt_id = ?1",
                params![
                    receipt_id,
                    error.as_str(),
                    lease_deadline(),
                    now(),
                    id,
                    self.session_id
                ],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "applying item is not owned by this session",
            ));
        }
        let item = query_one(&transaction, "WHERE id = ?1", params![id])?;
        transaction.commit().map_err(InternError::from)?;
        Ok(item)
    }

    pub fn defer_published_reconciliation(
        &self,
        id: i64,
        receipt_id: i64,
        error: ErrorCode,
    ) -> InternResult<QueueItem> {
        if error != ErrorCode::SourceDeleteFailed {
            return Err(InternError::new(
                ErrorCode::InvalidData,
                "only source-delete uncertainty can be deferred for user review",
            ));
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        let changed = transaction
            .execute(
                "UPDATE queue_items
             SET status = 'needs_review', reconciliation_receipt_id = ?1,
                 active_receipt_id = NULL, previous_status = NULL,
                 owner_session = NULL, lease_expires_at = NULL,
                 error_code = ?2, updated_at = ?3
             WHERE id = ?4 AND status = 'applying' AND owner_session = ?5
               AND active_receipt_id = ?1
               AND EXISTS (
                 SELECT 1 FROM operation_receipts receipts
                 WHERE receipts.id = ?1 AND receipts.queue_item_id = ?4
                   AND receipts.direction = 'apply' AND receipts.stage = 'published'
               )",
                params![receipt_id, error.as_str(), now(), id, self.session_id],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            transaction.rollback().map_err(InternError::from)?;
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "published source-delete uncertainty could not be deferred",
            ));
        }
        let item = query_one(&transaction, "WHERE id = ?1", params![id])?;
        transaction.commit().map_err(InternError::from)?;
        Ok(item)
    }

    pub fn claim_deferred_reconciliation(&self, id: i64) -> InternResult<QueueItem> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        touch_session(&transaction, &self.session_id)?;
        let changed = transaction
            .execute(
                "UPDATE queue_items
             SET status = 'applying', previous_status = 'ready', owner_session = ?1,
                 lease_expires_at = ?2, active_receipt_id = reconciliation_receipt_id,
                 updated_at = ?3
             WHERE id = ?4 AND status = 'needs_review'
               AND error_code = 'SOURCE_DELETE_FAILED'
               AND active_receipt_id IS NULL AND reconciliation_receipt_id IS NOT NULL
               AND EXISTS (
                 SELECT 1 FROM operation_receipts receipts
                 WHERE receipts.id = queue_items.reconciliation_receipt_id
                   AND receipts.queue_item_id = queue_items.id
                   AND receipts.direction = 'apply' AND receipts.stage = 'published'
               )
               AND NOT EXISTS (
                 SELECT 1 FROM queue_items active
                 WHERE active.id <> ?4
                   AND active.status IN ('extracting', 'analyzing', 'applying')
               )",
                params![self.session_id, lease_deadline(), now(), id],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            transaction.rollback().map_err(InternError::from)?;
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "deferred source deletion is not available for explicit retry",
            ));
        }
        let item = query_one(&transaction, "WHERE id = ?1", params![id])?;
        transaction.commit().map_err(InternError::from)?;
        Ok(item)
    }

    pub fn recover_interrupted(&self) -> InternResult<usize> {
        let connection = self.lock()?;
        let timestamp = now();
        connection
            .execute(
                "UPDATE queue_items
             SET status = 'queued', owner_session = NULL, lease_expires_at = NULL, updated_at = ?1
             WHERE status IN ('extracting', 'analyzing')
               AND (
                 owner_session IS NULL
                 OR NOT EXISTS (
                   SELECT 1 FROM queue_sessions sessions
                   WHERE sessions.session_id = queue_items.owner_session
                 )
                 OR (
                   lease_expires_at <= ?1
                   AND NOT EXISTS (
                     SELECT 1 FROM queue_sessions sessions
                     WHERE sessions.session_id = queue_items.owner_session
                       AND sessions.heartbeat_at > ?2
                   )
                 )
               )",
                params![timestamp, timestamp - LEASE_SECONDS],
            )
            .map_err(InternError::from)
    }

    pub fn list(&self) -> InternResult<Vec<QueueItem>> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(&queue_select("ORDER BY id"))
            .map_err(InternError::from)?;
        let rows = statement
            .query_map([], row_to_item)
            .map_err(InternError::from)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(InternError::from)
    }

    /// Lists finished operations, newest first, for the history view.
    ///
    /// Only receipts in a terminal stage (`complete` or `rolled_back`) are
    /// reported: an in-flight receipt is bookkeeping for the applier and may
    /// still end either way. Receipts are joined to their queue items, so a
    /// cleared history (which cascades receipt deletion) lists nothing stale.
    /// `limit` is capped at [`HISTORY_LIMIT`].
    pub fn list_operation_history(&self, limit: usize) -> InternResult<Vec<HistoryEntry>> {
        let limit = i64::try_from(limit.min(HISTORY_LIMIT)).unwrap_or(0);
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT receipts.id, receipts.queue_item_id, receipts.updated_at,
                        receipts.direction, receipts.operation_kind, receipts.stage,
                        receipts.source_path, receipts.destination_path
                 FROM operation_receipts receipts
                 JOIN queue_items items ON items.id = receipts.queue_item_id
                 WHERE receipts.stage IN ('complete', 'rolled_back')
                 ORDER BY receipts.updated_at DESC, receipts.id DESC
                 LIMIT ?1",
            )
            .map_err(InternError::from)?;
        let rows = statement
            .query_map(params![limit], row_to_history_entry)
            .map_err(InternError::from)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(InternError::from)
    }

    pub fn clear_terminal(&self) -> InternResult<usize> {
        let connection = self.lock()?;
        connection
            .execute(
                "DELETE FROM queue_items WHERE status IN ('failed', 'canceled', 'completed')",
                [],
            )
            .map_err(InternError::from)
    }

    /// Drops items that are still only waiting, leaving everything else alone.
    ///
    /// A user who points the queue at the wrong folder had no way out: the only
    /// bulk action was `clear_terminal`, which deletes finished work, and the
    /// only way to drop a waiting item was one at a time. Four hundred items
    /// meant four hundred clicks.
    ///
    /// Strictly `queued`. An item being extracted or analysed is owned by a
    /// session and must reach its own end; `ready` and `needs_review` are
    /// waiting on a human decision, not on the queue; and terminal rows hold the
    /// receipts that make a rename undoable.
    pub fn discard_queued(&self) -> InternResult<usize> {
        let connection = self.lock()?;
        connection
            .execute("DELETE FROM queue_items WHERE status = 'queued'", [])
            .map_err(InternError::from)
    }

    pub fn record_processing_failure(
        &self,
        id: i64,
        error: ErrorCode,
    ) -> InternResult<QueueStatus> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        let failures = transaction
            .query_row(
                "SELECT processing_failures FROM queue_items
             WHERE id = ?1 AND status IN ('extracting', 'analyzing') AND owner_session = ?2",
                params![id, self.session_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(InternError::from)?
            .ok_or_else(|| {
                InternError::new(
                    ErrorCode::StateConflict,
                    "item is not owned processing work",
                )
            })?
            + 1;
        let status = if failures >= 2 {
            QueueStatus::Failed
        } else {
            QueueStatus::Queued
        };
        transaction
            .execute(
                "UPDATE queue_items
             SET status = ?1, processing_failures = ?2, error_code = ?3,
                 owner_session = NULL, lease_expires_at = NULL, updated_at = ?4
             WHERE id = ?5 AND owner_session = ?6",
                params![
                    status.as_db(),
                    failures,
                    error.as_str(),
                    now(),
                    id,
                    self.session_id
                ],
            )
            .map_err(InternError::from)?;
        transaction.commit().map_err(InternError::from)?;
        Ok(status)
    }

    pub(crate) fn create_receipt(
        &self,
        queue_item_id: i64,
        mut receipt: OperationReceipt,
    ) -> InternResult<OperationReceipt> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        touch_session(&transaction, &self.session_id)?;
        let previous_status = transaction
            .query_row(
                "SELECT previous_status FROM queue_items
             WHERE id = ?1 AND status = 'applying' AND owner_session = ?2
               AND active_receipt_id IS NULL
               AND NOT EXISTS (
                 SELECT 1 FROM operation_receipts receipts
                 WHERE receipts.queue_item_id = ?1
                   AND receipts.stage NOT IN ('complete', 'rolled_back')
               )",
                params![queue_item_id, self.session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(InternError::from)?;
        let direction_matches = matches!(
            (receipt.direction, previous_status.as_deref()),
            (OperationDirection::Apply, Some("ready"))
                | (OperationDirection::Undo, Some("completed"))
        );
        if !direction_matches || receipt.stage != OperationStage::Planned {
            transaction.rollback().map_err(InternError::from)?;
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "planned receipt direction does not match the owned applying epoch",
            ));
        }
        receipt.queue_item_id = queue_item_id;
        let timestamp = now();
        let source_path = path_text(&receipt.source);
        let destination_path = path_text(&receipt.destination);
        let temporary_path = receipt.temporary_path.as_deref().map(path_text);
        transaction
            .execute(
                "INSERT INTO operation_receipts(
               queue_item_id, direction, source_path, destination_path, temporary_path,
               pre_hash, post_hash, operation_kind, stage, source_exists,
               destination_exists, temporary_exists, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
                params![
                    receipt.queue_item_id,
                    receipt.direction.as_db(),
                    source_path,
                    destination_path,
                    temporary_path,
                    receipt.pre_operation_hash,
                    receipt.post_operation_hash,
                    receipt.kind.as_db(),
                    receipt.stage.as_db(),
                    receipt.source_exists,
                    receipt.destination_exists,
                    receipt.temporary_exists,
                    timestamp,
                ],
            )
            .map_err(InternError::from)?;
        receipt.id = transaction.last_insert_rowid();
        let changed = transaction
            .execute(
                "UPDATE queue_items
             SET active_receipt_id = ?1, lease_expires_at = ?2, updated_at = ?3
             WHERE id = ?4 AND status = 'applying' AND owner_session = ?5
               AND active_receipt_id IS NULL",
                params![
                    receipt.id,
                    lease_deadline(),
                    timestamp,
                    queue_item_id,
                    self.session_id
                ],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            transaction.rollback().map_err(InternError::from)?;
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "applying epoch could not bind its receipt",
            ));
        }
        transaction.commit().map_err(InternError::from)?;
        Ok(receipt)
    }

    pub fn load_receipt(&self, queue_item_id: i64) -> InternResult<Option<OperationReceipt>> {
        let connection = self.lock()?;
        connection
            .query_row(
                &receipt_select("WHERE queue_item_id = ?1 ORDER BY id DESC LIMIT 1"),
                params![queue_item_id],
                row_to_receipt,
            )
            .optional()
            .map_err(InternError::from)
    }

    pub(crate) fn load_active_receipt(
        &self,
        queue_item_id: i64,
    ) -> InternResult<Option<OperationReceipt>> {
        let connection = self.lock()?;
        connection
            .query_row(
                &receipt_select(
                    "WHERE id = (
                   SELECT active_receipt_id FROM queue_items WHERE id = ?1
                 ) AND queue_item_id = ?1",
                ),
                params![queue_item_id],
                row_to_receipt,
            )
            .optional()
            .map_err(InternError::from)
    }

    pub(crate) fn update_receipt(
        &self,
        expected_stage: OperationStage,
        receipt: &OperationReceipt,
    ) -> InternResult<OperationReceipt> {
        if !expected_stage.can_advance_to(receipt.stage) {
            return Err(InternError::new(
                ErrorCode::InvalidTransition,
                "receipt stage transition is not permitted",
            ));
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        touch_session(&transaction, &self.session_id)?;
        let timestamp = now();
        let renewed = transaction
            .execute(
                "UPDATE queue_items SET lease_expires_at = ?1, updated_at = ?2
             WHERE id = ?3 AND status = 'applying' AND owner_session = ?4
               AND active_receipt_id = ?5",
                params![
                    timestamp + LEASE_SECONDS,
                    timestamp,
                    receipt.queue_item_id,
                    self.session_id,
                    receipt.id
                ],
            )
            .map_err(InternError::from)?;
        if renewed != 1 {
            transaction.rollback().map_err(InternError::from)?;
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "receipt owner lease could not be renewed",
            ));
        }
        let changed = transaction
            .execute(
                "UPDATE operation_receipts
             SET temporary_path = ?1, post_hash = ?2, stage = ?3,
                 source_exists = ?4, destination_exists = ?5, temporary_exists = ?6,
                 updated_at = ?7
             WHERE id = ?8 AND queue_item_id = ?9 AND stage = ?10
               AND EXISTS(
                 SELECT 1 FROM queue_items
                 WHERE id = ?9 AND status = 'applying' AND owner_session = ?11
                   AND active_receipt_id = ?8
               )",
                params![
                    receipt.temporary_path.as_deref().map(path_text),
                    receipt.post_operation_hash,
                    receipt.stage.as_db(),
                    receipt.source_exists,
                    receipt.destination_exists,
                    receipt.temporary_exists,
                    timestamp,
                    receipt.id,
                    receipt.queue_item_id,
                    expected_stage.as_db(),
                    self.session_id,
                ],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "receipt changed or applying ownership was lost",
            ));
        }
        let updated = transaction
            .query_row(
                &receipt_select("WHERE id = ?1"),
                params![receipt.id],
                row_to_receipt,
            )
            .map_err(InternError::from)?;
        transaction.commit().map_err(InternError::from)?;
        Ok(updated)
    }

    fn cas_status(
        &self,
        id: i64,
        expected: QueueStatus,
        next: QueueStatus,
        reset_failures: bool,
    ) -> InternResult<QueueItem> {
        let connection = self.lock()?;
        let changed = connection
            .execute(
                "UPDATE queue_items
             SET status = ?1,
                 processing_failures = CASE WHEN ?2 THEN 0 ELSE processing_failures END,
                 error_code = NULL, owner_session = NULL, lease_expires_at = NULL,
                 previous_status = NULL, active_receipt_id = NULL,
                 reconciliation_receipt_id = NULL, updated_at = ?3
             WHERE id = ?4 AND status = ?5",
                params![next.as_db(), reset_failures, now(), id, expected.as_db()],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "queue compare-and-swap failed",
            ));
        }
        query_one(&connection, "WHERE id = ?1", params![id])
    }

    fn finish_applying(
        &self,
        id: i64,
        receipt_id: i64,
        next: QueueStatus,
        required_previous: QueueStatus,
        direction: OperationDirection,
    ) -> InternResult<QueueItem> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(InternError::from)?;
        touch_session(&transaction, &self.session_id)?;
        let changed = transaction
            .execute(
                "UPDATE queue_items
             SET status = ?1, owner_session = NULL, lease_expires_at = NULL,
                 previous_status = NULL, active_receipt_id = NULL,
                 reconciliation_receipt_id = NULL,
                 error_code = NULL, updated_at = ?2
             WHERE id = ?3 AND status = 'applying' AND owner_session = ?4
               AND previous_status = ?5 AND active_receipt_id = ?6
               AND EXISTS (
                 SELECT 1 FROM operation_receipts receipts
                 WHERE receipts.id = ?6 AND receipts.queue_item_id = ?3
                   AND receipts.direction = ?7 AND receipts.stage = 'complete'
               )",
                params![
                    next.as_db(),
                    now(),
                    id,
                    self.session_id,
                    required_previous.as_db(),
                    receipt_id,
                    direction.as_db(),
                ],
            )
            .map_err(InternError::from)?;
        if changed != 1 {
            return Err(InternError::new(
                ErrorCode::StateConflict,
                "applying completion compare-and-swap failed",
            ));
        }
        let item = query_one(&transaction, "WHERE id = ?1", params![id])?;
        transaction.commit().map_err(InternError::from)?;
        Ok(item)
    }

    fn lock(&self) -> InternResult<std::sync::MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| InternError::new(ErrorCode::DatabaseUnavailable, "database lock poisoned"))
    }
}

impl Drop for QueueStore {
    fn drop(&mut self) {
        if let Ok(connection) = self.connection.lock() {
            // Failure is deliberately fail-closed: the item lease and stale
            // heartbeat still prevent another live session from stealing work.
            let _ = connection.execute(
                "DELETE FROM queue_sessions WHERE session_id = ?1",
                params![self.session_id],
            );
        }
    }
}

fn migrate_legacy_schema(connection: &mut Connection) -> InternResult<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(InternError::from)?;
    let has_v2_marker = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 2)",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(InternError::from)?;
    let migrating_to_v3 =
        has_v2_marker && !column_exists(&transaction, "queue_items", "active_receipt_id")?;
    for (column, definition) in [
        (
            "owner_session",
            "owner_session TEXT REFERENCES queue_sessions(session_id) ON DELETE SET NULL",
        ),
        ("lease_expires_at", "lease_expires_at INTEGER"),
        ("previous_status", "previous_status TEXT"),
        ("active_receipt_id", "active_receipt_id INTEGER"),
        (
            "reconciliation_receipt_id",
            "reconciliation_receipt_id INTEGER",
        ),
        (
            "applying_epoch",
            "applying_epoch INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        if !column_exists(&transaction, "queue_items", column)? {
            transaction
                .execute(
                    &format!("ALTER TABLE queue_items ADD COLUMN {definition}"),
                    [],
                )
                .map_err(InternError::from)?;
        }
    }
    if column_exists(&transaction, "operation_receipts", "receipt_json")? {
        transaction
            .execute(
                "ALTER TABLE operation_receipts RENAME TO operation_receipts_legacy_v1",
                [],
            )
            .map_err(InternError::from)?;
        transaction
            .execute_batch(
                "CREATE TABLE operation_receipts (
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
                 );",
            )
            .map_err(InternError::from)?;
    }
    if migrating_to_v3 {
        transaction
            .execute(
                "UPDATE queue_items
             SET active_receipt_id = (
               SELECT receipts.id FROM operation_receipts receipts
               WHERE receipts.queue_item_id = queue_items.id
                 AND receipts.id = (
                   SELECT MAX(latest.id) FROM operation_receipts latest
                   WHERE latest.queue_item_id = queue_items.id
                 )
                 AND receipts.stage <> 'rolled_back'
                 AND (
                   (queue_items.previous_status = 'ready' AND receipts.direction = 'apply'
                     AND receipts.source_path = queue_items.source_path)
                   OR (queue_items.previous_status = 'completed' AND receipts.direction = 'undo'
                     AND receipts.destination_path = queue_items.source_path)
                 )
             )
             WHERE status = 'applying' AND active_receipt_id IS NULL
               AND 1 = (
                 SELECT COUNT(*) FROM operation_receipts receipts
                 WHERE receipts.queue_item_id = queue_items.id
                   AND receipts.id = (
                     SELECT MAX(latest.id) FROM operation_receipts latest
                     WHERE latest.queue_item_id = queue_items.id
                   )
                   AND receipts.stage <> 'rolled_back'
                   AND (
                     (queue_items.previous_status = 'ready' AND receipts.direction = 'apply'
                       AND receipts.source_path = queue_items.source_path)
                     OR (queue_items.previous_status = 'completed' AND receipts.direction = 'undo'
                       AND receipts.destination_path = queue_items.source_path)
                   )
               )
               AND NOT EXISTS (
                 SELECT 1 FROM operation_receipts current_receipts
                 WHERE current_receipts.queue_item_id = queue_items.id
                   AND current_receipts.stage NOT IN ('complete', 'rolled_back')
                 GROUP BY current_receipts.queue_item_id
                 HAVING COUNT(*) > 1
               )",
                [],
            )
            .map_err(InternError::from)?;
    }
    let duplicate_nonterminal_receipts = transaction
        .query_row(
            "SELECT EXISTS(
           SELECT 1 FROM operation_receipts
           WHERE stage NOT IN ('complete', 'rolled_back')
           GROUP BY queue_item_id HAVING COUNT(*) > 1
         )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(InternError::from)?;
    if !duplicate_nonterminal_receipts {
        transaction
            .execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS one_active_receipt_per_item
               ON operation_receipts(queue_item_id)
               WHERE stage NOT IN ('complete', 'rolled_back');",
            )
            .map_err(InternError::from)?;
    }
    transaction
        .execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (3, ?1)",
            params![now()],
        )
        .map_err(InternError::from)?;
    transaction
        .execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (4, ?1)",
            params![now()],
        )
        .map_err(InternError::from)?;
    transaction.commit().map_err(InternError::from)
}

fn column_exists(connection: &Connection, table: &str, column: &str) -> InternResult<bool> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(InternError::from)?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(InternError::from)?;
    for name in names {
        if name.map_err(InternError::from)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn touch_session(connection: &Connection, session_id: &str) -> InternResult<()> {
    let changed = connection
        .execute(
            "UPDATE queue_sessions SET heartbeat_at = ?1 WHERE session_id = ?2",
            params![now(), session_id],
        )
        .map_err(InternError::from)?;
    if changed != 1 {
        return Err(InternError::new(
            ErrorCode::StateConflict,
            "queue session is no longer live",
        ));
    }
    Ok(())
}

fn query_one<P>(connection: &Connection, suffix: &str, parameters: P) -> InternResult<QueueItem>
where
    P: rusqlite::Params,
{
    connection
        .query_row(&queue_select(suffix), parameters, row_to_item)
        .map_err(InternError::from)
}

fn queue_select(suffix: &str) -> String {
    format!(
        "SELECT id, source_path, source_hash, status, processing_failures, error_code,
                owner_session, lease_expires_at, previous_status, active_receipt_id,
                reconciliation_receipt_id, created_at, updated_at
         FROM queue_items {suffix}"
    )
}

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueueItem> {
    let status = parse_status(row.get::<_, String>(3)?, 3)?;
    let error = row
        .get::<_, Option<String>>(5)?
        .map(|value| {
            ErrorCode::from_str(&value).ok_or_else(|| invalid_column(5, "unknown error code"))
        })
        .transpose()?;
    let previous_status = row
        .get::<_, Option<String>>(8)?
        .map(|value| parse_status(value, 8))
        .transpose()?;
    Ok(QueueItem {
        id: row.get(0)?,
        source_path: PathBuf::from(row.get::<_, String>(1)?),
        source_hash: row.get(2)?,
        status,
        processing_failures: u32::try_from(row.get::<_, i64>(4)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
        error_code: error,
        owner_session: row.get(6)?,
        lease_expires_at: row.get(7)?,
        previous_status,
        active_receipt_id: row.get(9)?,
        reconciliation_receipt_id: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn receipt_select(suffix: &str) -> String {
    format!(
        "SELECT id, queue_item_id, direction, source_path, destination_path, temporary_path,
                pre_hash, post_hash, operation_kind, stage, source_exists,
                destination_exists, temporary_exists
         FROM operation_receipts {suffix}"
    )
}

fn row_to_receipt(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationReceipt> {
    let direction_text: String = row.get(2)?;
    let kind_text: String = row.get(8)?;
    let stage_text: String = row.get(9)?;
    Ok(OperationReceipt {
        id: row.get(0)?,
        queue_item_id: row.get(1)?,
        direction: OperationDirection::from_db(&direction_text)
            .ok_or_else(|| invalid_column(2, "unknown receipt direction"))?,
        source: PathBuf::from(row.get::<_, String>(3)?),
        destination: PathBuf::from(row.get::<_, String>(4)?),
        temporary_path: row.get::<_, Option<String>>(5)?.map(PathBuf::from),
        pre_operation_hash: row.get(6)?,
        post_operation_hash: row.get(7)?,
        kind: OperationKind::from_db(&kind_text)
            .ok_or_else(|| invalid_column(8, "unknown operation kind"))?,
        stage: OperationStage::from_db(&stage_text)
            .ok_or_else(|| invalid_column(9, "unknown operation stage"))?,
        source_exists: row.get(10)?,
        destination_exists: row.get(11)?,
        temporary_exists: row.get(12)?,
    })
}

fn row_to_history_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    let direction_text: String = row.get(3)?;
    let kind_text: String = row.get(4)?;
    let stage_text: String = row.get(5)?;
    Ok(HistoryEntry {
        receipt_id: row.get(0)?,
        queue_item_id: row.get(1)?,
        at: row.get(2)?,
        direction: OperationDirection::from_db(&direction_text)
            .ok_or_else(|| invalid_column(3, "unknown receipt direction"))?,
        kind: OperationKind::from_db(&kind_text)
            .ok_or_else(|| invalid_column(4, "unknown operation kind"))?,
        stage: OperationStage::from_db(&stage_text)
            .ok_or_else(|| invalid_column(5, "unknown operation stage"))?,
        original_path: PathBuf::from(row.get::<_, String>(6)?),
        new_path: PathBuf::from(row.get::<_, String>(7)?),
    })
}

fn parse_status(value: String, index: usize) -> rusqlite::Result<QueueStatus> {
    QueueStatus::from_db(&value).ok_or_else(|| invalid_column(index, "unknown queue status"))
}

fn invalid_column(index: usize, message: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn is_active(status: QueueStatus) -> bool {
    matches!(
        status,
        QueueStatus::Extracting | QueueStatus::Analyzing | QueueStatus::Applying
    )
}

fn lease_deadline() -> i64 {
    now() + LEASE_SECONDS
}

fn new_session_id() -> String {
    let sequence = SESSION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}-{sequence}", std::process::id())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn windows_path_key(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// The normalized key `enqueue` stores for a source path, for callers that
/// need to compare against `source_path_key` (e.g. duplicate lookups).
pub fn source_path_key(path: &Path) -> String {
    windows_path_key(&path.to_string_lossy())
}
