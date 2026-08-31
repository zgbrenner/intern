//! Scan-side types: watcher configuration, the host boundary, status, and the
//! filesystem walk with its skip and stability rules.

use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use crate::coordination::{DoneOutcome, MachinePresence};

pub const DEFAULT_SCAN_INTERVAL: Duration = Duration::from_secs(20);

#[derive(Clone, Debug)]
pub struct IntakeConfig {
    pub intake_root: PathBuf,
    /// Lowercase extensions without the dot — the host passes intern-queue's
    /// supported list.
    pub extensions: Vec<String>,
    pub process_others_uploads: bool,
    /// Injectable so tests can drive the loop without real waits.
    pub scan_interval: Duration,
}

impl IntakeConfig {
    pub fn new(intake_root: impl Into<PathBuf>, extensions: Vec<String>) -> Self {
        Self {
            intake_root: intake_root.into(),
            extensions,
            process_others_uploads: false,
            scan_interval: DEFAULT_SCAN_INTERVAL,
        }
    }
}

/// What the host queue knows about a document it was handed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ItemState {
    Unknown,
    Active,
    NeedsReview,
    Done {
        outcome: DoneOutcome,
        result_filename: Option<String>,
    },
    Failed,
}

/// The boundary to whatever processes documents (the pipeline in the real
/// app, a fake in tests). The watcher only hands over paths and asks about
/// their fate; it never reads document content itself.
pub trait IntakeHost: Send + Sync {
    fn enqueue(&self, paths: &[PathBuf]) -> Result<(), String>;
    fn item_state(&self, path: &Path) -> ItemState;
    /// The claim was lost to a takeover or sync conflict: cancel/remove the
    /// local item if it is still pending.
    fn abandon(&self, path: &Path);
    fn status_changed(&self, status: &IntakeStatus);
}

#[derive(Clone, Debug, PartialEq)]
pub struct IntakeStatus {
    pub watching: bool,
    pub folder: PathBuf,
    pub last_scan_at: Option<i64>,
    /// Eligible-extension files we are NOT claiming (scope/ownership rules).
    pub held_for_others: u32,
    /// Files skipped because their name is a sync client's conflict copy.
    pub sync_conflicts: u32,
    /// Claims held open because the document's content is not on this disk yet.
    pub awaiting_hydration: u32,
    pub claimed_by_others: u32,
    /// Done-by-us claims seen.
    pub processed_here: u32,
    pub machines: Vec<MachinePresence>,
    /// Last scan error code/message.
    pub error: Option<String>,
}

impl IntakeStatus {
    pub(crate) fn idle(folder: PathBuf) -> Self {
        Self {
            watching: true,
            folder,
            last_scan_at: None,
            held_for_others: 0,
            sync_conflicts: 0,
            awaiting_hydration: 0,
            claimed_by_others: 0,
            processed_here: 0,
            machines: Vec::new(),
            error: None,
        }
    }

    /// `last_scan_at` advances on every tick; a change there alone is not
    /// worth waking the host for.
    pub(crate) fn materially_differs(&self, other: &Self) -> bool {
        self.watching != other.watching
            || self.folder != other.folder
            || self.held_for_others != other.held_for_others
            || self.claimed_by_others != other.claimed_by_others
            || self.processed_here != other.processed_here
            || self.machines != other.machines
            || self.error != other.error
    }
}

/// One eligible file as observed by a scan tick (stat only — a OneDrive
/// placeholder must be claimable without ever hydrating it).
#[derive(Clone, Debug)]
pub(crate) struct FileFacts {
    pub path: PathBuf,
    /// Relative to the intake root, `/`-separated.
    pub relative_path: String,
    pub size: u64,
    pub modified_secs: i64,
}

/// Recursive walk with the same skip rules as intern-queue's path handling:
/// dot-names (which covers `.intern` itself), `~$` office lock files,
/// unsupported extensions, zero-byte files, and symlinks.
pub(crate) fn walk_intake(root: &Path, extensions: &[String]) -> io::Result<Vec<FileFacts>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            if skipped_name(&path) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push(path);
                continue;
            }
            if !file_type.is_file() || !supported(&path, extensions) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.len() == 0 {
                continue;
            }
            let Some(relative_path) = relative_slash_path(root, &path) else {
                continue;
            };
            files.push(FileFacts {
                size: metadata.len(),
                modified_secs: modified_secs(&metadata),
                path,
                relative_path,
            });
        }
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

/// A file only becomes claimable after it is observed with an identical
/// `(size, mtime)` on two consecutive scans.
///
/// The sync client (or a user copy) writes intake files incrementally, and a
/// key computed from a half-written file would never match the settled file —
/// the claim would orphan and the document would be processed from a torn
/// snapshot. One full scan interval of quiet is the cheapest proof of
/// stability available from stat alone.
#[derive(Debug, Default)]
pub struct StabilityTracker {
    observed: HashMap<PathBuf, (u64, i64)>,
}

impl StabilityTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the observation; true when it is unchanged since the previous
    /// scan. A first sighting is always unstable.
    pub fn observe(&mut self, path: &Path, size: u64, modified_secs: i64) -> bool {
        match self.observed.get(path) {
            Some(&previous) if previous == (size, modified_secs) => true,
            _ => {
                self.observed
                    .insert(path.to_path_buf(), (size, modified_secs));
                false
            }
        }
    }

    /// Drops observations for files that vanished so the map cannot grow
    /// without bound.
    pub fn retain_live(&mut self, live: &HashSet<PathBuf>) {
        self.observed.retain(|path, _| live.contains(path));
    }
}

fn supported(path: &Path, extensions: &[String]) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extensions.contains(&extension.to_ascii_lowercase()))
}

/// Whether a document's bytes are still in the cloud rather than on this disk.
///
/// A seam, because no test can conjure a real Files On-Demand placeholder.
pub trait Hydration: Send + Sync {
    fn is_dehydrated(&self, path: &Path) -> bool;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemHydration;

impl Hydration for SystemHydration {
    /// OneDrive and SharePoint mark an online-only file with the recall
    /// attributes; Windows sets them without the content ever being fetched, so
    /// reading them costs nothing and hydrates nothing. `symlink_metadata` is
    /// deliberate - following the placeholder is the one thing that would pull
    /// the file down.
    #[cfg(windows)]
    fn is_dehydrated(&self, path: &Path) -> bool {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
        const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
        const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

        fs::symlink_metadata(path).is_ok_and(|metadata| {
            metadata.file_attributes()
                & (FILE_ATTRIBUTE_OFFLINE
                    | FILE_ATTRIBUTE_RECALL_ON_OPEN
                    | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
                != 0
        })
    }

    /// No sync client outside Windows presents placeholders to Intern, so
    /// everything readable here is already local.
    #[cfg(not(windows))]
    fn is_dehydrated(&self, _path: &Path) -> bool {
        false
    }
}

/// A name a sync client produced by resolving an edit conflict, rather than a
/// document a person put in the folder.
///
/// OneDrive and SharePoint resolve a conflict by keeping both versions and
/// suffixing the losing one with the machine that wrote it -
/// `report-DESKTOP-A1B2C3.pdf`. That shape is indistinguishable from an
/// ordinary hyphenated filename, and `Invoice-ACME.pdf` is a real document, so
/// the suffix is believed only when it names a machine this shared folder has
/// actually seen. Silently skipping a document someone meant to file would be
/// the worse failure of the two, so the guess is never made on shape alone.
///
/// The other wording sync clients use - `(Jane's conflicted copy 2026-08-31)` -
/// carries no such ambiguity and stands on its own.
pub fn is_conflict_copy(path: &Path, machines: &[String]) -> bool {
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return false;
    };
    let stem = stem.to_lowercase();
    if stem.contains("conflicted copy") {
        return true;
    }
    machines.iter().any(|machine| {
        let machine = machine.trim().to_lowercase();
        !machine.is_empty() && stem.ends_with(&format!("-{machine}"))
    })
}

fn skipped_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_none_or(|name| name.starts_with('.') || name.starts_with("~$"))
}

fn relative_slash_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn modified_secs(metadata: &fs::Metadata) -> i64 {
    let Ok(modified) = metadata.modified() else {
        return 0;
    };
    match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs() as i64,
        Err(error) => -(error.duration().as_secs() as i64),
    }
}
