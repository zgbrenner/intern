use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::pipeline::{PipelineError, PipelineResult};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub destination: String,
    pub start_minimized: bool,
    #[serde(default)]
    pub automatic_rename: bool,
}

#[derive(Clone, Debug)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> PipelineResult<AppSettings> {
        if !self.path.exists() {
            return Ok(AppSettings::default());
        }
        let bytes = fs::read(&self.path).map_err(|_| {
            PipelineError::new("SETTINGS_UNAVAILABLE", "settings could not be read")
        })?;
        serde_json::from_slice(&bytes)
            .map_err(|_| PipelineError::new("SETTINGS_INVALID", "settings are not valid"))
    }

    pub fn save(&self, settings: &AppSettings) -> PipelineResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        let bytes = serde_json::to_vec_pretty(settings)
            .map_err(|_| PipelineError::new("SETTINGS_INVALID", "settings could not be encoded"))?;
        fs::write(&self.path, bytes).map_err(io_error)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn io_error(_: std::io::Error) -> PipelineError {
    PipelineError::new("SETTINGS_UNAVAILABLE", "settings could not be saved")
}
