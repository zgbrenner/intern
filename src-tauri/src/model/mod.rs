use std::{error::Error, fmt};

pub mod client;
pub mod download;
pub mod manifest;
pub mod prompt;
pub mod server;
pub mod setup;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum ModelErrorCode {
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
}

impl ModelErrorCode {
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
        }
    }
}

#[derive(Debug)]
pub struct ModelError {
    code: ModelErrorCode,
    message: &'static str,
}

impl ModelError {
    pub const fn new(code: ModelErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }

    pub const fn code(&self) -> ModelErrorCode {
        self.code
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl Error for ModelError {}

pub type ModelResult<T> = Result<T, ModelError>;
