use std::{
    fs,
    path::{Path, PathBuf},
};

use intern_engine::HostedProvider;
use serde::{Deserialize, Serialize};

use crate::pipeline::{PipelineError, PipelineResult};

/// Which model reads documents.
///
/// Local is the product: a model on this machine, and document text that
/// never leaves it. Hosted sends the distilled text of every document to a
/// service behind an API key the user supplied, and exists for people who
/// have decided that trade is worth making. It is never chosen by default.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    #[default]
    Local,
    Hosted,
}

impl ModelSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Hosted => "hosted",
        }
    }
}

/// How filed documents are arranged under the destination folder.
///
/// A flat destination is the default and what every earlier version did. The
/// other layouts put each document in a subfolder derived from the facts its
/// filename already carries, so a year of contracts does not become one
/// folder of a thousand files. The subfolder names are sanitised the same
/// way filenames are; a document missing the fact a layout needs goes in a
/// clearly named catch-all rather than the root.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationLayout {
    /// Everything directly in the destination folder.
    #[default]
    Flat,
    /// `2026/`
    Year,
    /// `2026/Statement of Work/`
    YearType,
    /// `Statement of Work/`
    Type,
    /// `Ridgeline Cartography LLC/` - the first party.
    Party,
}

impl DestinationLayout {
    pub const ALL: [Self; 5] = [
        Self::Flat,
        Self::Year,
        Self::YearType,
        Self::Type,
        Self::Party,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Year => "year",
            Self::YearType => "year_type",
            Self::Type => "type",
            Self::Party => "party",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub destination: String,
    /// Subfolders under the destination, derived from each document's facts.
    #[serde(default)]
    pub destination_layout: DestinationLayout,
    pub start_minimized: bool,
    #[serde(default)]
    pub automatic_rename: bool,
    #[serde(default)]
    pub intake_folder: String,
    #[serde(default)]
    pub intake_enabled: bool,
    #[serde(default)]
    pub process_others_uploads: bool,
    #[serde(default)]
    pub machine_label: String,
    /// Keep Intern alive in the system tray when the window is closed.
    #[serde(default)]
    pub run_in_background: bool,
    /// Register Intern to start when the user signs in.
    #[serde(default)]
    pub start_at_login: bool,
    /// Write a description record beside every document filed into the
    /// destination folder (`<destination>/.intern/descriptions/`), so a
    /// SharePoint column can be filled from it.
    #[serde(default)]
    pub record_descriptions: bool,
    /// The local model, or a hosted one behind an API key.
    #[serde(default)]
    pub model_source: ModelSource,
    /// The wire format the hosted model speaks.
    #[serde(default)]
    pub hosted_provider: HostedProvider,
    /// The hosted model's API root; empty means the provider's default.
    #[serde(default)]
    pub hosted_base_url: String,
    /// The hosted model's name; empty means the provider's default, where
    /// there is one. The API key is never stored here - it lives in the
    /// operating system's credential store.
    #[serde(default)]
    pub hosted_model: String,
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
        // Written beside the target and renamed into place so a crash mid-write
        // leaves the previous settings intact instead of a truncated file.
        let mut temp = self.path.as_os_str().to_owned();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        fs::write(&temp, bytes).map_err(io_error)?;
        fs::rename(&temp, &self.path).map_err(io_error)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn io_error(_: std::io::Error) -> PipelineError {
    PipelineError::new("SETTINGS_UNAVAILABLE", "settings could not be saved")
}
