//! The shared filed index inside `<intake>/.intern/filed/`.
//!
//! A claim names a document by its path, size, and modification time, so a
//! claim's done tombstone stops a machine with a lagging sync view from
//! processing the same *upload* twice. It says nothing about the same
//! *content* arriving again: a teammate re-sends last month's agreement under
//! a new name, and each machine's local queue has only its own history to
//! check it against. The filed index closes that gap.
//!
//! When a machine files a document out of the intake folder it leaves a
//! marker named by the document's content hash - the same SHA-256 the queue
//! fingerprints every document with. A machine that later meets the same
//! bytes, under any name and from any uploader, learns what they were filed
//! as and by whom, and routes the newcomer to review as a duplicate instead
//! of filing a second copy. "Process anyway" remains one click away.
//!
//! Markers are small, carry no document text, and live for a year - long
//! enough to outlast the annual re-send of a recurring document.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    cloud::relative_to_root,
    coordination::{Stored, Versioned, load},
    fsatomic,
    identity::MachineIdentity,
};

/// How long a marker outlives the filing it records.
pub const FILED_RETENTION_SECONDS: i64 = 365 * 24 * 3600;

const FORMAT_VERSION: u32 = 1;

/// One record of a document filed out of the intake folder.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FiledMarker {
    pub version: u32,
    /// The SHA-256 of the document's bytes, lowercase hex. The marker is
    /// named `<contentHash>.json`.
    pub content_hash: String,
    /// The filename the document was filed under.
    pub filename: String,
    /// Where the document was in the intake folder, relative to the intake
    /// root and `/`-separated.
    pub relative_path: String,
    pub machine_id: String,
    pub machine_name: String,
    pub user_name: String,
    /// Unix seconds.
    pub filed_at: i64,
}

impl Versioned for FiledMarker {
    fn version(&self) -> u32 {
        self.version
    }
}

/// Reads and writes filed markers under one intake folder.
pub struct FiledIndex {
    intake_root: PathBuf,
    directory: PathBuf,
    identity: MachineIdentity,
}

impl FiledIndex {
    /// Builds the index for an intake folder without touching the disk; the
    /// directory is created by the first `record`.
    pub fn new(intake_root: impl Into<PathBuf>, identity: MachineIdentity) -> Self {
        let intake_root = intake_root.into();
        Self {
            directory: Self::directory_under(&intake_root),
            intake_root,
            identity,
        }
    }

    /// Where the markers live: `<intake>/.intern/filed`.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Where markers for an intake folder live, without building an index.
    pub fn directory_under(intake_root: &Path) -> PathBuf {
        intake_root.join(".intern").join("filed")
    }

    /// The machine the index writes markers as.
    pub fn identity(&self) -> &MachineIdentity {
        &self.identity
    }

    /// Whether a document at `path` came from this intake folder - the only
    /// documents the index records.
    pub fn covers(&self, path: &Path) -> bool {
        self.relative_path(path).is_some()
    }

    /// `path` relative to the intake root, as a marker would record it, or
    /// `None` when it is not inside the folder.
    pub fn relative_path(&self, path: &Path) -> Option<String> {
        relative_to_root(path, &self.intake_root)
    }

    /// Records that the document that was at `source`, with content
    /// `content_hash`, has been filed as `filename`. Replaces any earlier
    /// marker for the same content, whoever wrote it: the newest filing is
    /// the one a duplicate should be compared with.
    pub fn record(
        &self,
        content_hash: &str,
        source: &Path,
        filename: &str,
        filed_at: i64,
    ) -> io::Result<PathBuf> {
        if !is_hex_digest(content_hash) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the content hash is not a hex digest",
            ));
        }
        let relative_path = relative_to_root(source, &self.intake_root).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the document did not come from the intake folder",
            )
        })?;
        let marker = FiledMarker {
            version: FORMAT_VERSION,
            content_hash: content_hash.to_owned(),
            filename: filename.to_owned(),
            relative_path,
            machine_id: self.identity.id.clone(),
            machine_name: self.identity.name.clone(),
            user_name: self.identity.user.clone(),
            filed_at,
        };
        fs::create_dir_all(&self.directory)?;
        let path = self.marker_path(content_hash);
        let bytes = serde_json::to_vec_pretty(&marker).map_err(io::Error::from)?;
        fsatomic::replace_file(&path, &bytes)?;
        Ok(path)
    }

    /// Removes this machine's marker for `content_hash` - an undone filing.
    /// A marker another machine wrote records a filing this machine did not
    /// undo, and stays. True when a marker was removed.
    pub fn retract(&self, content_hash: &str) -> io::Result<bool> {
        if !is_hex_digest(content_hash) {
            return Ok(false);
        }
        let path = self.marker_path(content_hash);
        match load::<FiledMarker>(&path) {
            Stored::Parsed(marker) if marker.machine_id == self.identity.id => {
                match fs::remove_file(&path) {
                    Ok(()) => Ok(true),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                    Err(error) => Err(error),
                }
            }
            _ => Ok(false),
        }
    }

    /// The marker for `content_hash`, if any machine left one that parses.
    pub fn lookup(&self, content_hash: &str) -> Option<FiledMarker> {
        if !is_hex_digest(content_hash) {
            return None;
        }
        match load::<FiledMarker>(&self.marker_path(content_hash)) {
            Stored::Parsed(marker) if marker.content_hash == content_hash => Some(marker),
            _ => None,
        }
    }

    fn marker_path(&self, content_hash: &str) -> PathBuf {
        self.directory.join(format!("{content_hash}.json"))
    }
}

/// A marker is named by the hash, so only a lowercase hex digest may become
/// a filename: nothing with a separator, a dot, or a case the filesystem
/// might fold differently from another machine.
fn is_hex_digest(value: &str) -> bool {
    (32..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::is_hex_digest;

    #[test]
    fn only_a_lowercase_hex_digest_can_name_a_marker() {
        assert!(is_hex_digest(&"a1".repeat(32)));
        assert!(is_hex_digest(&"0".repeat(32)));
        assert!(
            !is_hex_digest(&"A1".repeat(32)),
            "uppercase folds on some filesystems"
        );
        assert!(!is_hex_digest("abc"), "too short to be a digest");
        assert!(!is_hex_digest(&"g".repeat(64)));
        assert!(!is_hex_digest(&format!("../{}", "a".repeat(62))));
        assert!(!is_hex_digest(""));
    }
}
