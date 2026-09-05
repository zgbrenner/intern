use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::pipeline::{PipelineError, PipelineResult};

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
