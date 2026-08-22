//! The multi-machine claim protocol stored inside `<intake>/.intern/`.
//!
//! Everything here is small JSON files replicated by the OneDrive/SharePoint
//! sync client. That transport is eventually consistent: writes arrive late,
//! races produce "conflict copy" duplicates, and a file can be replaced under
//! us at any moment. Every rule below is therefore best-effort mutual
//! exclusion only — good enough that two machines almost never start the same
//! document — while the fingerprint/CAS apply machinery in `intern-core`
//! remains the hard backstop that keeps a lost race from ever double-renaming
//! a document.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::{fsatomic, identity::MachineIdentity};

pub const CLAIM_LEASE_SECONDS: i64 = 900;
pub const CLAIM_RENEW_THRESHOLD_SECONDS: i64 = 450;
pub const COURTESY_DELAY_SECONDS: i64 = 120;
pub const DONE_RETENTION_SECONDS: i64 = 30 * 24 * 3600;
pub const PRESENCE_REFRESH_SECONDS: i64 = 300;
pub const PRESENCE_ACTIVE_WINDOW_SECONDS: i64 = 600;

const FORMAT_VERSION: u32 = 1;
const MALFORMED_RETENTION_SECONDS: i64 = 24 * 3600;

const README_TEXT: &str = "\
This .intern folder is maintained by Intern.

It coordinates multiple machines that watch the same intake folder through a
sync client (OneDrive/SharePoint). It contains only small JSON bookkeeping
files — claims/ (which machine is processing which document), origins/ (which
machine uploaded a document), machines/ (recent machine presence) — and never
any document content. Deleting it is safe, but two machines may then briefly
pick up the same document before their claims re-converge.
";

/// Injectable time source so tests never wait out real lease durations.
pub trait Clock: Send + Sync {
    /// Seconds since the Unix epoch.
    fn now(&self) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }
}

/// Stable identity of one document revision inside the intake folder.
///
/// The key hashes the relative path together with size and mtime, so a file
/// that is edited (or replaced by the sync client) becomes a *different*
/// document with its own claim, and machines with differing path casing or
/// separators still agree on the key.
pub fn document_key(relative_path: &str, size: u64, modified_secs: i64) -> String {
    let normalized = relative_path.to_lowercase().replace('\\', "/");
    let mut hasher = Sha256::new();
    hasher.update(format!("{normalized}|{size}|{modified_secs}").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// What a scanner observed about one intake file, as used for claims.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentFacts {
    /// Path relative to the intake root, `/`-separated.
    pub relative_path: String,
    pub size: u64,
    pub modified_secs: i64,
}

impl DocumentFacts {
    pub fn key(&self) -> String {
        document_key(&self.relative_path, self.size, self.modified_secs)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClaimState {
    Claimed,
    Done,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoneOutcome {
    Renamed,
    KeptOriginal,
    Failed,
    Removed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimInfo {
    pub version: u32,
    pub key: String,
    pub relative_path: String,
    pub size: u64,
    pub modified_at: i64,
    pub machine_id: String,
    pub machine_name: String,
    pub user_name: String,
    pub state: ClaimState,
    pub claimed_at: i64,
    pub lease_expires_at: i64,
    pub heartbeat_at: i64,
    #[serde(default)]
    pub done_at: Option<i64>,
    #[serde(default)]
    pub outcome: Option<DoneOutcome>,
    #[serde(default)]
    pub result_filename: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginInfo {
    pub version: u32,
    pub key: String,
    pub relative_path: String,
    pub machine_id: String,
    pub machine_name: String,
    pub user_name: String,
    pub observed_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachinePresence {
    pub version: u32,
    pub machine_id: String,
    pub machine_name: String,
    pub user_name: String,
    pub last_seen_at: i64,
}

/// `ClaimInfo` dominates the size, but boxing it would push the cost onto
/// every caller of the contract-fixed `acquire` signature.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum AcquireOutcome {
    Acquired,
    HeldByOther(ClaimInfo),
    Done(ClaimInfo),
    Failed(io::Error),
}

enum Stored<T> {
    Missing,
    Unreadable,
    Parsed(T),
}

trait Versioned {
    fn version(&self) -> u32;
}

impl Versioned for ClaimInfo {
    fn version(&self) -> u32 {
        self.version
    }
}

impl Versioned for OriginInfo {
    fn version(&self) -> u32 {
        self.version
    }
}

impl Versioned for MachinePresence {
    fn version(&self) -> u32 {
        self.version
    }
}

/// A malformed or future-version file is indistinguishable from sync-conflict
/// garbage, so it reads as `Unreadable` rather than an error or a panic;
/// `prune` clears such files once they are a day old.
fn load<T: DeserializeOwned + Versioned>(path: &Path) -> Stored<T> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Stored::Missing,
        Err(_) => return Stored::Unreadable,
    };
    match serde_json::from_slice::<T>(&bytes) {
        Ok(value) if value.version() == FORMAT_VERSION => Stored::Parsed(value),
        _ => Stored::Unreadable,
    }
}

enum TakeOver {
    Acquired,
    Lost,
    Failed(io::Error),
}

pub struct ClaimStore {
    root: PathBuf,
    identity: MachineIdentity,
    clock: Arc<dyn Clock>,
    presence_touched_at: Mutex<Option<i64>>,
}

impl ClaimStore {
    pub fn new(intake_root: &Path, identity: MachineIdentity) -> io::Result<Self> {
        Self::with_clock(intake_root, identity, Arc::new(SystemClock))
    }

    pub fn with_clock(
        intake_root: &Path,
        identity: MachineIdentity,
        clock: Arc<dyn Clock>,
    ) -> io::Result<Self> {
        let root = intake_root.join(".intern");
        for subdir in ["claims", "origins", "machines"] {
            fs::create_dir_all(root.join(subdir))?;
        }
        match fsatomic::create_exclusive(&root.join("README.txt"), README_TEXT.as_bytes()) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        Ok(Self {
            root,
            identity,
            clock,
            presence_touched_at: Mutex::new(None),
        })
    }

    /// Tries to claim one document for this machine.
    ///
    /// Creation is exclusive, so of any number of racing machines exactly one
    /// gets `Acquired` (on one filesystem; across sync replicas the loser is
    /// caught by `verify` once files converge). A stale claim may be taken
    /// over only when BOTH the lease deadline has passed AND the heartbeat is
    /// a full lease old: the lease alone is not trustworthy, because the
    /// owner's renewal may still be sitting in the sync client's upload queue
    /// and the two machines' clocks may disagree by minutes. Requiring the
    /// heartbeat — rewritten on every renewal — to also be `CLAIM_LEASE_SECONDS`
    /// behind means the owner has been silent for a whole lease period as
    /// observed from here.
    pub fn acquire(&self, doc: &DocumentFacts) -> AcquireOutcome {
        let key = doc.key();
        let path = self.claim_path(&key);
        for _ in 0..3 {
            match load::<ClaimInfo>(&path) {
                Stored::Missing => {
                    let bytes = match encode(&self.fresh_claim(doc, &key)) {
                        Ok(bytes) => bytes,
                        Err(error) => return AcquireOutcome::Failed(error),
                    };
                    match fsatomic::create_exclusive(&path, &bytes) {
                        Ok(()) => return AcquireOutcome::Acquired,
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                        Err(error) => return AcquireOutcome::Failed(error),
                    }
                }
                Stored::Unreadable => {
                    return AcquireOutcome::Failed(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "claim file is unreadable or from a newer version",
                    ));
                }
                Stored::Parsed(claim) => {
                    if claim.state == ClaimState::Done {
                        return AcquireOutcome::Done(claim);
                    }
                    if claim.machine_id == self.identity.id {
                        return AcquireOutcome::Acquired;
                    }
                    let now = self.clock.now();
                    let lease_stale = now >= claim.lease_expires_at;
                    let heartbeat_stale = now >= claim.heartbeat_at + CLAIM_LEASE_SECONDS;
                    if !(lease_stale && heartbeat_stale) {
                        return AcquireOutcome::HeldByOther(claim);
                    }
                    match self.take_over(&path, &key, &claim, doc) {
                        TakeOver::Acquired => return AcquireOutcome::Acquired,
                        TakeOver::Lost => continue,
                        TakeOver::Failed(error) => return AcquireOutcome::Failed(error),
                    }
                }
            }
        }
        match load::<ClaimInfo>(&path) {
            Stored::Parsed(claim) if claim.state == ClaimState::Done => AcquireOutcome::Done(claim),
            Stored::Parsed(claim) => AcquireOutcome::HeldByOther(claim),
            _ => AcquireOutcome::Failed(io::Error::other(
                "claim acquisition kept losing races and gave up",
            )),
        }
    }

    /// Re-reads the claim from disk; true iff it still names this machine in
    /// the `claimed` state. Called before enqueueing and on every scan, this
    /// is what actually decides races the filesystem could not — whichever
    /// machine's claim survives sync convergence keeps the document, and the
    /// other sees `verify` fail and abandons.
    pub fn verify(&self, key: &str) -> bool {
        matches!(
            load::<ClaimInfo>(&self.claim_path(key)),
            Stored::Parsed(claim)
                if claim.machine_id == self.identity.id && claim.state == ClaimState::Claimed
        )
    }

    /// Extends the lease, self-gated: a rewrite only happens once less than
    /// `CLAIM_RENEW_THRESHOLD_SECONDS` remain, so a busy machine does not
    /// churn the sync client with a new upload every scan tick.
    pub fn renew(&self, key: &str) -> io::Result<()> {
        let path = self.claim_path(key);
        let Stored::Parsed(mut claim) = load::<ClaimInfo>(&path) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "claim is missing or unreadable",
            ));
        };
        if claim.machine_id != self.identity.id || claim.state != ClaimState::Claimed {
            return Err(io::Error::other("claim is no longer owned by this machine"));
        }
        let now = self.clock.now();
        if claim.lease_expires_at - now >= CLAIM_RENEW_THRESHOLD_SECONDS {
            return Ok(());
        }
        claim.lease_expires_at = now + CLAIM_LEASE_SECONDS;
        claim.heartbeat_at = now;
        fsatomic::replace_file(&path, &encode(&claim)?)
    }

    /// Converts our claim into a done tombstone that outlives the lease, so
    /// other machines never re-process a document that was already handled.
    pub fn mark_done(
        &self,
        key: &str,
        outcome: DoneOutcome,
        result_filename: Option<&str>,
    ) -> io::Result<()> {
        let path = self.claim_path(key);
        let Stored::Parsed(mut claim) = load::<ClaimInfo>(&path) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "claim is missing or unreadable",
            ));
        };
        if claim.machine_id != self.identity.id {
            return Err(io::Error::other("claim is not owned by this machine"));
        }
        let now = self.clock.now();
        claim.state = ClaimState::Done;
        claim.done_at = Some(now);
        claim.heartbeat_at = now;
        claim.outcome = Some(outcome);
        claim.result_filename = result_filename.map(str::to_string);
        fsatomic::replace_file(&path, &encode(&claim)?)
    }

    /// Deletes our own claim (e.g. the local item disappeared before work
    /// started). Refuses to touch a claim this machine does not own.
    pub fn release(&self, key: &str) -> io::Result<()> {
        let path = self.claim_path(key);
        match load::<ClaimInfo>(&path) {
            Stored::Missing => Ok(()),
            Stored::Unreadable => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "claim ownership cannot be proven from an unreadable file",
            )),
            Stored::Parsed(claim) if claim.machine_id == self.identity.id => {
                match fs::remove_file(&path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error),
                }
            }
            Stored::Parsed(_) => Err(io::Error::other("claim is not owned by this machine")),
        }
    }

    pub fn read(&self, key: &str) -> Option<ClaimInfo> {
        match load::<ClaimInfo>(&self.claim_path(key)) {
            Stored::Parsed(claim) => Some(claim),
            _ => None,
        }
    }

    /// Records which machine first observed (i.e. uploaded) a document.
    /// Create-exclusive with `AlreadyExists` ignored: the first writer wins
    /// and a later scanner on another machine cannot re-attribute the file.
    pub fn write_origin(&self, doc: &DocumentFacts) -> io::Result<()> {
        let origin = OriginInfo {
            version: FORMAT_VERSION,
            key: doc.key(),
            relative_path: doc.relative_path.clone(),
            machine_id: self.identity.id.clone(),
            machine_name: self.identity.name.clone(),
            user_name: self.identity.user.clone(),
            observed_at: self.clock.now(),
        };
        let path = self.origin_path(&origin.key);
        match fsatomic::create_exclusive(&path, &encode(&origin)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn read_origin(&self, key: &str) -> Option<OriginInfo> {
        match load::<OriginInfo>(&self.origin_path(key)) {
            Stored::Parsed(origin) => Some(origin),
            _ => None,
        }
    }

    /// Refreshes this machine's presence file, self-gated to
    /// `PRESENCE_REFRESH_SECONDS` so it does not spam the sync client.
    pub fn touch_presence(&self) -> io::Result<()> {
        let now = self.clock.now();
        {
            let touched = self
                .presence_touched_at
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if touched.is_some_and(|at| now - at < PRESENCE_REFRESH_SECONDS) {
                return Ok(());
            }
        }
        let presence = MachinePresence {
            version: FORMAT_VERSION,
            machine_id: self.identity.id.clone(),
            machine_name: self.identity.name.clone(),
            user_name: self.identity.user.clone(),
            last_seen_at: now,
        };
        let path = self
            .root
            .join("machines")
            .join(format!("{}.json", self.identity.id));
        fsatomic::replace_file(&path, &encode(&presence)?)?;
        *self
            .presence_touched_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(now);
        Ok(())
    }

    pub fn list_machines(&self) -> Vec<MachinePresence> {
        let mut machines = Vec::new();
        let Ok(entries) = fs::read_dir(self.root.join("machines")) else {
            return machines;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_dot_named(&path)
                || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            if let Stored::Parsed(presence) = load::<MachinePresence>(&path) {
                machines.push(presence);
            }
        }
        machines.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
        machines
    }

    /// Best-effort janitor for the shared directory.
    ///
    /// Removes done tombstones past `DONE_RETENTION_SECONDS`, claimed leases
    /// whose heartbeat has been silent that long (their machine is gone for
    /// good and nothing else ever deletes them), origin markers past the same
    /// retention, and — after a one-day grace so in-flight sync writes are
    /// never eaten — malformed files, sync conflict copies (valid JSON under
    /// the wrong filename), and leaked dot-prefixed temp files.
    pub fn prune(&self) {
        let now = self.clock.now();
        self.prune_claims(now);
        self.prune_named_dir::<OriginInfo>(&self.root.join("origins"), now, |origin| {
            (origin.key.clone(), origin.observed_at)
        });
        self.prune_named_dir::<MachinePresence>(&self.root.join("machines"), now, |presence| {
            (presence.machine_id.clone(), presence.last_seen_at)
        });
    }

    fn prune_claims(&self, now: i64) {
        let Ok(entries) = fs::read_dir(self.root.join("claims")) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match load::<ClaimInfo>(&path) {
                Stored::Parsed(claim) => {
                    if stem_of(&path) != Some(claim.key.as_str()) {
                        remove_if_older(&path, now, MALFORMED_RETENTION_SECONDS);
                        continue;
                    }
                    let reference = match claim.state {
                        ClaimState::Done => claim.done_at.unwrap_or(claim.claimed_at),
                        ClaimState::Claimed => claim.heartbeat_at,
                    };
                    if now - reference >= DONE_RETENTION_SECONDS {
                        let _ = fs::remove_file(&path);
                    }
                }
                Stored::Unreadable => remove_if_older(&path, now, MALFORMED_RETENTION_SECONDS),
                Stored::Missing => {}
            }
        }
    }

    fn prune_named_dir<T: DeserializeOwned + Versioned>(
        &self,
        dir: &Path,
        now: i64,
        identity_of: fn(&T) -> (String, i64),
    ) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match load::<T>(&path) {
                Stored::Parsed(value) => {
                    let (name, seen_at) = identity_of(&value);
                    if stem_of(&path) != Some(name.as_str()) {
                        remove_if_older(&path, now, MALFORMED_RETENTION_SECONDS);
                    } else if now - seen_at >= DONE_RETENTION_SECONDS {
                        let _ = fs::remove_file(&path);
                    }
                }
                Stored::Unreadable => remove_if_older(&path, now, MALFORMED_RETENTION_SECONDS),
                Stored::Missing => {}
            }
        }
    }

    fn fresh_claim(&self, doc: &DocumentFacts, key: &str) -> ClaimInfo {
        let now = self.clock.now();
        ClaimInfo {
            version: FORMAT_VERSION,
            key: key.to_string(),
            relative_path: doc.relative_path.clone(),
            size: doc.size,
            modified_at: doc.modified_secs,
            machine_id: self.identity.id.clone(),
            machine_name: self.identity.name.clone(),
            user_name: self.identity.user.clone(),
            state: ClaimState::Claimed,
            claimed_at: now,
            lease_expires_at: now + CLAIM_LEASE_SECONDS,
            heartbeat_at: now,
            done_at: None,
            outcome: None,
            result_filename: None,
        }
    }

    /// Takes over a stale claim by *renaming* it aside first: on one
    /// filesystem only one racer's rename can succeed, which turns the
    /// delete-then-create window into an atomic step. The renamed tombstone is
    /// re-checked against the claim we based the staleness decision on; if it
    /// changed in between (a last-instant renewal), it is put back and the
    /// takeover is abandoned. Across sync replicas none of this is watertight —
    /// the loser's next `verify` and the CAS apply machinery are the backstop.
    fn take_over(
        &self,
        path: &Path,
        key: &str,
        stale: &ClaimInfo,
        doc: &DocumentFacts,
    ) -> TakeOver {
        let tombstone = fsatomic::temp_sibling(path, "takeover");
        match fs::rename(path, &tombstone) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return TakeOver::Lost,
            Err(error) => return TakeOver::Failed(error),
        }
        match load::<ClaimInfo>(&tombstone) {
            Stored::Parsed(observed) if observed == *stale => {}
            _ => {
                if fs::rename(&tombstone, path).is_err() {
                    let _ = fs::remove_file(&tombstone);
                }
                return TakeOver::Lost;
            }
        }
        let result = match encode(&self.fresh_claim(doc, key)) {
            Ok(bytes) => match fsatomic::create_exclusive(path, &bytes) {
                Ok(()) => TakeOver::Acquired,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => TakeOver::Lost,
                Err(error) => TakeOver::Failed(error),
            },
            Err(error) => TakeOver::Failed(error),
        };
        let _ = fs::remove_file(&tombstone);
        result
    }

    fn claim_path(&self, key: &str) -> PathBuf {
        self.root.join("claims").join(format!("{key}.json"))
    }

    fn origin_path(&self, key: &str) -> PathBuf {
        self.root.join("origins").join(format!("{key}.json"))
    }
}

fn encode<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    serde_json::to_vec_pretty(value).map_err(io::Error::from)
}

fn stem_of(path: &Path) -> Option<&str> {
    path.file_stem().and_then(|value| value.to_str())
}

fn is_dot_named(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_none_or(|name| name.starts_with('.'))
}

/// Age checks for unreadable files fall back to the wall-clock mtime — the
/// only timestamp such a file has.
fn remove_if_older(path: &Path, now: i64, retention: i64) {
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64);
    if modified.is_some_and(|at| now - at >= retention) {
        let _ = fs::remove_file(path);
    }
}
