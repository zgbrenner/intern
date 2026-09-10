use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use intern_engine::HostedProvider;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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
    /// Open to the tray rather than to the window. Like every other
    /// preference here it is simply off when the file does not mention it;
    /// the destination is the one field Intern will not invent, so a file
    /// that has lost it is reported rather than defaulted quietly.
    #[serde(default)]
    pub start_minimized: bool,
    #[serde(default)]
    pub automatic_rename: bool,
    #[serde(default)]
    pub intake_folder: String,
    #[serde(default)]
    pub intake_enabled: bool,
    /// Explicitly private/local intake; never permitted for a detected sync root.
    #[serde(default)]
    pub intake_local_only: bool,
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

/// A settings file that was read, and the fields in it that could not be
/// understood.
///
/// One misspelled value used to discard the whole document: a
/// `destinationLayout` of `none` took the destination folder and the watched
/// intake folder down with it, and Intern opened looking healthy with
/// neither. So whatever parses is kept, whatever does not takes its default,
/// and the fields that did not are named - defaulting a setting quietly
/// would be the same silence in a smaller place.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LoadedSettings {
    pub settings: AppSettings,
    pub unreadable: Vec<String>,
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
        self.load_with_report().map(|loaded| loaded.settings)
    }

    /// The settings, and the fields of the file that had to be defaulted.
    pub fn load_with_report(&self) -> PipelineResult<LoadedSettings> {
        if !self.path.exists() {
            return Ok(LoadedSettings::default());
        }
        let bytes = fs::read(&self.path).map_err(|_| {
            PipelineError::new("SETTINGS_UNAVAILABLE", "settings could not be read")
        })?;
        // Windows tooling marks a UTF-8 file: one `Set-Content -Encoding utf8`
        // in Windows PowerShell is enough, and serde_json refuses to read past
        // the mark. A marked file is not a corrupt one, and the document
        // parser reads text files by their mark for the same reason.
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
        let document = serde_json::from_slice::<Value>(bytes).map_err(|_| settings_invalid())?;
        let Value::Object(fields) = document else {
            return Err(settings_invalid());
        };
        match serde_json::from_value::<AppSettings>(Value::Object(fields.clone())) {
            Ok(settings) => Ok(LoadedSettings {
                settings,
                unreadable: Vec::new(),
            }),
            Err(_) => keep_what_parses(fields),
        }
    }

    pub fn save(&self, settings: &AppSettings) -> PipelineResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        let bytes = serde_json::to_vec_pretty(settings)
            .map_err(|_| PipelineError::new("SETTINGS_INVALID", "settings could not be encoded"))?;
        // Written beside the target and renamed into place so a crash mid-write
        // leaves the previous settings intact instead of a truncated file, and
        // flushed to the disk before the rename, because a rename can outlive
        // the bytes it points at when the machine loses power - the shared
        // folder's own state is written the same way.
        let mut temp = self.path.as_os_str().to_owned();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        write_sync(&temp, &bytes).map_err(io_error)?;
        fs::rename(&temp, &self.path).map_err(io_error)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn write_sync(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn io_error(_: std::io::Error) -> PipelineError {
    PipelineError::new("SETTINGS_UNAVAILABLE", "settings could not be saved")
}

fn settings_invalid() -> PipelineError {
    PipelineError::new("SETTINGS_INVALID", "settings are not valid")
}

/// Reads a settings document a field at a time, keeping every field that is
/// understood and defaulting the rest.
///
/// A field is tried on its own, over the defaults, so a value nothing can
/// make sense of costs its own line and nothing else. Fields Intern does not
/// know are left alone, as they always were: a settings file written by a
/// newer version is still readable by an older one.
fn keep_what_parses(fields: Map<String, Value>) -> PipelineResult<LoadedSettings> {
    let Ok(Value::Object(defaults)) = serde_json::to_value(AppSettings::default()) else {
        return Err(settings_invalid());
    };
    let mut kept = defaults.clone();
    let mut unreadable = Vec::new();
    // A field the file simply leaves out is ordinary - a settings file written
    // by an older version lacks every field added since - unless the reader
    // requires it, in which case its absence is part of why the document did
    // not parse and is the person's to hear about too.
    for name in defaults.keys() {
        if fields.contains_key(name) {
            continue;
        }
        let mut without = defaults.clone();
        without.remove(name);
        if serde_json::from_value::<AppSettings>(Value::Object(without)).is_err() {
            unreadable.push(name.clone());
        }
    }
    for (name, value) in fields {
        let mut probe = defaults.clone();
        probe.insert(name.clone(), value.clone());
        if serde_json::from_value::<AppSettings>(Value::Object(probe)).is_ok() {
            kept.insert(name, value);
        } else {
            unreadable.push(name);
        }
    }
    let settings = serde_json::from_value::<AppSettings>(Value::Object(kept))
        .map_err(|_| settings_invalid())?;
    Ok(LoadedSettings {
        settings,
        unreadable,
    })
}
