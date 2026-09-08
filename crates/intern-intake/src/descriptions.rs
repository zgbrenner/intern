//! Description records for filed documents.
//!
//! Intern produces two things about a document: a filename and a one-sentence
//! description. The filename travels with the file; the sentence, until now,
//! stayed in the local queue where nobody else could see it. A SharePoint
//! library has a natural home for it - a column - and the sync client that
//! carries the file can carry a small record beside it.
//!
//! So, when asked to, Intern writes one JSON file per filed document under
//! `<destination>/.intern/descriptions/`: the filename, its path within the
//! destination (and within the SharePoint library or OneDrive it is synced
//! from, when that is known), the description, and the date, type, and
//! parties behind the name. Nothing in a record is document text. A Power
//! Automate flow - the recipe is in the guide - reads the record and fills
//! the library's column; a person can do the same from grid view.
//!
//! The records are written the same way the claim protocol writes its files:
//! atomically, one file per document, named by a hash of the document's
//! relative path so that a re-filed document overwrites its own record and
//! two machines never write the same file at once.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    cloud::{CloudRoot, matching_root, relative_to_root},
    fsatomic,
    identity::MachineIdentity,
};

const FORMAT_VERSION: u32 = 1;

const README_TEXT: &str = "\
This folder is maintained by Intern.

Each JSON file describes one document Intern filed into this folder tree: the
filename it was given, where it is, the one-sentence description Intern wrote
for it, and the date, document type, and parties behind the name. No file here
contains document text.

The records exist so that a SharePoint column can be filled from them. See the
\"Descriptions in a SharePoint column\" section of the Intern guide for the
Power Automate recipe. Deleting a record only removes that record; the
document it describes is untouched.
";

/// What Intern knows about a document it has just filed.
#[derive(Clone, Debug, PartialEq)]
pub struct FiledDocument {
    /// The document's path after filing - inside the ledger root.
    pub path: PathBuf,
    /// The name it arrived with.
    pub original_filename: String,
    pub description: String,
    pub document_date: Option<String>,
    pub document_type: Option<String>,
    pub parties: Vec<String>,
    pub confidence: Option<f32>,
    /// Unix seconds.
    pub filed_at: i64,
}

/// One record as written to disk.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DescriptionRecord {
    pub version: u32,
    /// The record's own key - the file is named `<key>.json`.
    pub key: String,
    /// The filed document's filename.
    pub filename: String,
    /// The document's path relative to the destination folder the record
    /// lives under, `/`-separated.
    pub path: String,
    /// The document's path relative to the SharePoint library or OneDrive
    /// root the destination is synced from, `/`-separated. Absent when the
    /// destination is not inside a sync root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_path: Option<String>,
    /// The sync root's display name - the tenant for a SharePoint library,
    /// the account for a OneDrive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
    /// `sharepoint`, `onedrive_business`, `onedrive_personal`, or
    /// `network_share`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub description: String,
    pub document_date: Option<String>,
    pub document_type: Option<String>,
    pub parties: Vec<String>,
    pub confidence: Option<f32>,
    pub original_filename: String,
    /// Unix seconds.
    pub filed_at: i64,
    pub machine_id: String,
    pub machine_name: String,
    pub user_name: String,
}

/// Writes and removes description records under one destination folder.
pub struct DescriptionLedger {
    root: PathBuf,
    identity: MachineIdentity,
    cloud_roots: Vec<CloudRoot>,
}

impl DescriptionLedger {
    /// `root` is the destination folder; `cloud_roots` are the sync roots
    /// detected on this machine, used only to name the library and the
    /// document's path within it.
    pub fn new(
        root: impl Into<PathBuf>,
        identity: MachineIdentity,
        cloud_roots: Vec<CloudRoot>,
    ) -> Self {
        Self {
            root: root.into(),
            identity,
            cloud_roots,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the records live: `<root>/.intern/descriptions`.
    pub fn directory(&self) -> PathBuf {
        Self::directory_under(&self.root)
    }

    /// Where records for a destination folder live, without building a ledger.
    pub fn directory_under(root: &Path) -> PathBuf {
        root.join(".intern").join("descriptions")
    }

    /// The record file for a filed document, or `None` when the document is
    /// not inside the ledger root.
    pub fn record_path(&self, filed: &Path) -> Option<PathBuf> {
        let relative = relative_to_root(filed, &self.root)?;
        Some(
            self.directory()
                .join(format!("{}.json", record_key(&relative))),
        )
    }

    /// Writes the record for one filed document, replacing any earlier record
    /// for the same path, and returns the record's path.
    pub fn record(&self, document: &FiledDocument) -> io::Result<PathBuf> {
        let relative = relative_to_root(&document.path, &self.root).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the filed document is not inside the destination folder",
            )
        })?;
        let filename = document
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| relative.clone());
        let matched = matching_root(&document.path, &self.cloud_roots);
        let record = DescriptionRecord {
            version: FORMAT_VERSION,
            key: record_key(&relative),
            filename,
            path: relative,
            library_path: matched.and_then(|root| relative_to_root(&document.path, &root.root)),
            library: matched.map(|root| root.display_name.clone()),
            provider: matched.map(|root| root.kind.as_str().to_owned()),
            description: document.description.trim().to_owned(),
            document_date: document.document_date.clone(),
            document_type: document.document_type.clone(),
            parties: document.parties.clone(),
            confidence: document.confidence,
            original_filename: document.original_filename.clone(),
            filed_at: document.filed_at,
            machine_id: self.identity.id.clone(),
            machine_name: self.identity.name.clone(),
            user_name: self.identity.user.clone(),
        };
        let directory = self.directory();
        fs::create_dir_all(&directory)?;
        match fsatomic::create_exclusive(&directory.join("README.txt"), README_TEXT.as_bytes()) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let path = directory.join(format!("{}.json", record.key));
        let bytes = serde_json::to_vec_pretty(&record).map_err(io::Error::from)?;
        fsatomic::replace_file(&path, &bytes)?;
        Ok(path)
    }

    /// Removes the record for a document that is no longer filed at `filed`
    /// (an undone rename). True when a record was removed.
    pub fn retract(&self, filed: &Path) -> io::Result<bool> {
        let Some(path) = self.record_path(filed) else {
            return Ok(false);
        };
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Reads the record for a filed document, if one exists and parses.
    pub fn read(&self, filed: &Path) -> Option<DescriptionRecord> {
        let path = self.record_path(filed)?;
        let bytes = fs::read(path).ok()?;
        serde_json::from_slice::<DescriptionRecord>(&bytes)
            .ok()
            .filter(|record| record.version == FORMAT_VERSION)
    }
}

/// The key a record is filed under: the first 32 hex characters of the
/// SHA-256 of the document's relative path, lowercased and `/`-separated, so
/// machines that spell the path with different casing or separators agree.
pub fn record_key(relative_path: &str) -> String {
    let normalized = relative_path.to_lowercase().replace('\\', "/");
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    format!("{:x}", hasher.finalize())[..32].to_string()
}
