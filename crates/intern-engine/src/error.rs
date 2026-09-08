//! Engine error codes.

use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum EngineErrorCode {
    #[serde(rename = "MODEL_MANIFEST_INVALID")]
    ManifestInvalid,
    #[serde(rename = "INSUFFICIENT_DISK")]
    InsufficientDisk,
    #[serde(rename = "MODEL_DOWNLOAD_FAILED")]
    DownloadFailed,
    #[serde(rename = "MODEL_DOWNLOAD_INTERRUPTED")]
    DownloadInterrupted,
    #[serde(rename = "MODEL_DOWNLOAD_CANCELED")]
    DownloadCanceled,
    #[serde(rename = "MODEL_FILE_INVALID")]
    ModelFileInvalid,
    #[serde(rename = "MODEL_SERVER_START_FAILED")]
    ModelServerStartFailed,
    #[serde(rename = "MODEL_SERVER_UNHEALTHY")]
    ModelServerUnhealthy,
    #[serde(rename = "MODEL_REQUEST_FAILED")]
    ModelRequestFailed,
    #[serde(rename = "MODEL_RESPONSE_INVALID")]
    ModelResponseInvalid,
    #[serde(rename = "MODEL_SELF_TEST_FAILED")]
    ModelSelfTestFailed,
    #[serde(rename = "SETUP_BUSY")]
    SetupBusy,
    /// The hosted model's endpoint, model name, or key is missing or malformed.
    #[serde(rename = "HOSTED_MODEL_MISCONFIGURED")]
    HostedModelMisconfigured,
    /// The hosted service rejected the API key.
    #[serde(rename = "HOSTED_MODEL_UNAUTHORIZED")]
    HostedModelUnauthorized,
    /// The hosted service could not be reached, or answered with a server error.
    #[serde(rename = "HOSTED_MODEL_UNREACHABLE")]
    HostedModelUnreachable,
    /// The hosted service asked for a slower pace.
    #[serde(rename = "HOSTED_MODEL_RATE_LIMITED")]
    HostedModelRateLimited,
    /// The hosted service refused the request itself - an unknown model
    /// name, a request it considers malformed, or one too large for it.
    #[serde(rename = "HOSTED_MODEL_REJECTED")]
    HostedModelRejected,
    /// The hosted model declined to answer about this document.
    #[serde(rename = "HOSTED_MODEL_REFUSED")]
    HostedModelRefused,
}

impl EngineErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ManifestInvalid => "MODEL_MANIFEST_INVALID",
            Self::InsufficientDisk => "INSUFFICIENT_DISK",
            Self::DownloadFailed => "MODEL_DOWNLOAD_FAILED",
            Self::DownloadInterrupted => "MODEL_DOWNLOAD_INTERRUPTED",
            Self::DownloadCanceled => "MODEL_DOWNLOAD_CANCELED",
            Self::ModelFileInvalid => "MODEL_FILE_INVALID",
            Self::ModelServerStartFailed => "MODEL_SERVER_START_FAILED",
            Self::ModelServerUnhealthy => "MODEL_SERVER_UNHEALTHY",
            Self::ModelRequestFailed => "MODEL_REQUEST_FAILED",
            Self::ModelResponseInvalid => "MODEL_RESPONSE_INVALID",
            Self::ModelSelfTestFailed => "MODEL_SELF_TEST_FAILED",
            Self::SetupBusy => "SETUP_BUSY",
            Self::HostedModelMisconfigured => "HOSTED_MODEL_MISCONFIGURED",
            Self::HostedModelUnauthorized => "HOSTED_MODEL_UNAUTHORIZED",
            Self::HostedModelUnreachable => "HOSTED_MODEL_UNREACHABLE",
            Self::HostedModelRateLimited => "HOSTED_MODEL_RATE_LIMITED",
            Self::HostedModelRejected => "HOSTED_MODEL_REJECTED",
            Self::HostedModelRefused => "HOSTED_MODEL_REFUSED",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EngineError {
    code: EngineErrorCode,
    message: &'static str,
}

impl EngineError {
    pub const fn new(code: EngineErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }

    pub const fn code(&self) -> EngineErrorCode {
        self.code
    }

    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl Error for EngineError {}

pub type EngineResult<T> = Result<T, EngineError>;
