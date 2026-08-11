use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::OperationReceipt;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    FileChanged,
    DestinationUnavailable,
    MoveVerificationFailed,
    SourceDeleteFailed,
    InvalidTransition,
    StateConflict,
    DatabaseUnavailable,
    IoError,
    InvalidData,
    ModelOutputInvalid,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FileChanged => "FILE_CHANGED",
            Self::DestinationUnavailable => "DESTINATION_UNAVAILABLE",
            Self::MoveVerificationFailed => "MOVE_VERIFICATION_FAILED",
            Self::SourceDeleteFailed => "SOURCE_DELETE_FAILED",
            Self::InvalidTransition => "INVALID_TRANSITION",
            Self::StateConflict => "STATE_CONFLICT",
            Self::DatabaseUnavailable => "DATABASE_UNAVAILABLE",
            Self::IoError => "IO_ERROR",
            Self::InvalidData => "INVALID_DATA",
            Self::ModelOutputInvalid => "MODEL_OUTPUT_INVALID",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        Some(match value {
            "FILE_CHANGED" => Self::FileChanged,
            "DESTINATION_UNAVAILABLE" => Self::DestinationUnavailable,
            "MOVE_VERIFICATION_FAILED" => Self::MoveVerificationFailed,
            "SOURCE_DELETE_FAILED" => Self::SourceDeleteFailed,
            "INVALID_TRANSITION" => Self::InvalidTransition,
            "STATE_CONFLICT" => Self::StateConflict,
            "DATABASE_UNAVAILABLE" => Self::DatabaseUnavailable,
            "IO_ERROR" => Self::IoError,
            "INVALID_DATA" => Self::InvalidData,
            "MODEL_OUTPUT_INVALID" => Self::ModelOutputInvalid,
            _ => return None,
        })
    }
}

#[derive(Debug)]
pub struct InternError {
    code: ErrorCode,
    message: String,
    receipt: Option<OperationReceipt>,
}

impl InternError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            receipt: None,
        }
    }

    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn receipt(&self) -> Option<&OperationReceipt> {
        self.receipt.as_ref()
    }

    pub(crate) fn with_receipt(mut self, receipt: OperationReceipt) -> Self {
        self.receipt = Some(receipt);
        self
    }
}

impl fmt::Display for InternError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl Error for InternError {}

impl From<rusqlite::Error> for InternError {
    fn from(error: rusqlite::Error) -> Self {
        Self::new(ErrorCode::DatabaseUnavailable, error.to_string())
    }
}

pub type InternResult<T> = Result<T, InternError>;
