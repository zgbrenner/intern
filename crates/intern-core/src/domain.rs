use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ErrorCode;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueStatus {
    Queued,
    Extracting,
    Analyzing,
    Ready,
    NeedsReview,
    Failed,
    Canceled,
    Applying,
    Completed,
}

impl QueueStatus {
    pub(crate) fn as_db(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Extracting => "extracting",
            Self::Analyzing => "analyzing",
            Self::Ready => "ready",
            Self::NeedsReview => "needs_review",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Applying => "applying",
            Self::Completed => "completed",
        }
    }

    pub(crate) fn from_db(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "extracting" => Self::Extracting,
            "analyzing" => Self::Analyzing,
            "ready" => Self::Ready,
            "needs_review" => Self::NeedsReview,
            "failed" => Self::Failed,
            "canceled" => Self::Canceled,
            "applying" => Self::Applying,
            "completed" => Self::Completed,
            _ => return None,
        })
    }

    pub(crate) fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            // Queued → NeedsReview lets ingestion flag an item before any
            // processing starts (e.g. its content is already filed as a
            // completed duplicate).
            (Self::Queued, Self::Canceled | Self::NeedsReview)
                | (
                    Self::Extracting,
                    Self::Analyzing | Self::Queued | Self::Failed | Self::Canceled
                )
                | (
                    Self::Analyzing,
                    Self::Ready | Self::NeedsReview | Self::Queued | Self::Failed | Self::Canceled
                )
                | (Self::Ready, Self::Canceled)
                | (Self::NeedsReview, Self::Ready | Self::Canceled)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::QueueStatus;

    #[test]
    fn queued_may_be_flagged_for_review_before_processing_but_not_finished() {
        assert!(QueueStatus::Queued.can_transition_to(QueueStatus::NeedsReview));
        assert!(!QueueStatus::Queued.can_transition_to(QueueStatus::Ready));
        assert!(!QueueStatus::Queued.can_transition_to(QueueStatus::Completed));
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QueueItem {
    pub id: i64,
    pub source_path: PathBuf,
    pub source_hash: String,
    pub status: QueueStatus,
    pub processing_failures: u32,
    pub error_code: Option<ErrorCode>,
    pub owner_session: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub previous_status: Option<QueueStatus>,
    pub active_receipt_id: Option<i64>,
    pub reconciliation_receipt_id: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}
