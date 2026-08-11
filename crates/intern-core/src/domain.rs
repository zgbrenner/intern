use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ErrorCode;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Evidence {
    #[serde(rename = "date_evidence")]
    pub date: Option<String>,
    #[serde(rename = "type_evidence")]
    pub document_type: Option<String>,
    #[serde(rename = "subject_evidence")]
    pub subject: Option<String>,
    #[serde(rename = "party_evidence")]
    pub parties: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DateKind {
    Signed,
    Effective,
    Issued,
    Due,
    Other,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ModelProposal {
    pub document_date: Option<String>,
    pub date_kind: Option<DateKind>,
    pub document_type: Option<String>,
    pub filename_subject: Option<String>,
    pub parties: Vec<String>,
    pub description: String,
    pub confidence: f32,
    pub needs_review: bool,
    pub review_reasons: Vec<String>,
    #[serde(flatten)]
    pub evidence: Evidence,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ValidatedProposal {
    pub document_date: Option<String>,
    pub date_kind: Option<DateKind>,
    pub document_type: Option<String>,
    pub filename_subject: Option<String>,
    pub parties: Vec<String>,
    pub description: String,
    pub confidence: f32,
    #[serde(flatten)]
    pub evidence: Evidence,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Ready,
    NeedsReview,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewReason {
    EvidenceMissing,
    InvalidDate,
    LowConfidence,
    ModelRequestedReview,
    ParserWarning,
    DescriptionTooLong,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ValidationOutcome {
    pub proposal: ValidatedProposal,
    pub status: ProposalStatus,
    pub reasons: Vec<ReviewReason>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExtractedDocument {
    pub text: String,
    pub parser_warnings: Vec<ParserWarning>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParserWarning {
    pub code: String,
    pub field_affecting: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentPacket {
    pub text: String,
    pub text_segments: Vec<String>,
    pub image_included: bool,
    pub parser_warnings: Vec<ParserWarning>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ComposedName {
    pub value: String,
    pub collision_index: u32,
}

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
            (Self::Queued, Self::Canceled)
                | (Self::Extracting, Self::Analyzing | Self::Queued | Self::Failed | Self::Canceled)
                | (Self::Analyzing, Self::Ready | Self::NeedsReview | Self::Queued | Self::Failed | Self::Canceled)
                | (Self::Ready, Self::Canceled)
                | (Self::NeedsReview, Self::Ready | Self::Canceled)
        )
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
