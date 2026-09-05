//! Shared-intake wiring for the Tauri host: the wire DTOs for the intake
//! commands and the `intake://changed` event, and the pipeline-backed
//! implementation of the intake crate's host boundary.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use intern_core::{OperationDirection, OperationReceipt, OperationStage, QueueStatus};
use intern_intake::{
    CloudLocation, CloudProviderKind, CloudRoot, DescriptionLedger, DoneOutcome, IntakeHost,
    IntakeStatus, ItemState, MachineIdentity, MachinePresence, PRESENCE_ACTIVE_WINDOW_SECONDS,
    classify, detect_cloud_roots,
};
use intern_queue::{
    FiledDocument, FilingSink, Pipeline, PipelineItem, SettingsStore,
    paths::{canonical_file, display_path},
};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::commands::SchedulerMessage;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum CloudProviderDto {
    #[serde(rename = "onedrive_personal")]
    OneDrivePersonal,
    #[serde(rename = "onedrive_business")]
    OneDriveBusiness,
    #[serde(rename = "sharepoint")]
    SharePoint,
    #[serde(rename = "network_share")]
    NetworkShare,
}

impl From<CloudProviderKind> for CloudProviderDto {
    fn from(kind: CloudProviderKind) -> Self {
        match kind {
            CloudProviderKind::OneDrivePersonal => Self::OneDrivePersonal,
            CloudProviderKind::OneDriveBusiness => Self::OneDriveBusiness,
            CloudProviderKind::SharePoint => Self::SharePoint,
            CloudProviderKind::NetworkShare => Self::NetworkShare,
        }
    }
}

/// One sync root the sync client keeps on this machine, for Settings to
/// offer as a folder to watch or file into.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudRootDto {
    pub provider: CloudProviderDto,
    pub display_name: String,
    pub path: String,
}

impl From<CloudRoot> for CloudRootDto {
    fn from(root: CloudRoot) -> Self {
        Self {
            provider: root.kind.into(),
            display_name: root.display_name,
            path: display_path(&root.root),
        }
    }
}

/// The sync roots detected on this machine, SharePoint libraries first,
/// then OneDrive accounts, each group in path order, so the list reads the
/// same on every open.
pub(crate) fn list_cloud_roots() -> Vec<CloudRootDto> {
    let mut roots = detect_cloud_roots();
    roots.sort_by(|left, right| {
        rank(left.kind)
            .cmp(&rank(right.kind))
            .then_with(|| left.root.cmp(&right.root))
    });
    roots.into_iter().map(Into::into).collect()
}

fn rank(kind: CloudProviderKind) -> u8 {
    match kind {
        CloudProviderKind::SharePoint => 0,
        CloudProviderKind::OneDriveBusiness => 1,
        CloudProviderKind::OneDrivePersonal => 2,
        CloudProviderKind::NetworkShare => 3,
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudLocationDto {
    pub provider: CloudProviderDto,
    pub display_name: String,
}

impl From<CloudLocation> for CloudLocationDto {
    fn from(location: CloudLocation) -> Self {
        Self {
            provider: location.kind.into(),
            display_name: location.display_name,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntakeMachineDto {
    pub machine_id: String,
    pub machine_name: String,
    pub user_name: String,
    pub last_seen_at: i64,
    pub active: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntakeStatusDto {
    pub enabled: bool,
    pub watching: bool,
    pub folder: String,
    pub machine_id: String,
    pub machine_name: String,
    pub cloud: Option<CloudLocationDto>,
    pub machines: Vec<IntakeMachineDto>,
    pub held_for_others: u32,
    pub sync_conflicts: u32,
    pub awaiting_hydration: u32,
    pub unreadable_folders: u32,
    pub claimed_by_others: u32,
    pub processed_here: u32,
    pub last_scan_at: Option<i64>,
    pub error: Option<String>,
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// A presence stamped in the future (sync-layer clock skew) still counts as
/// active rather than flickering off until the clocks agree.
pub(crate) fn presence_active(last_seen_at: i64, now: i64) -> bool {
    now - last_seen_at <= PRESENCE_ACTIVE_WINDOW_SECONDS
}

/// Roots are detected fresh on every call; the probe is a handful of env and
/// directory reads.
pub(crate) fn classify_folder(path: &str) -> Option<CloudLocationDto> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    classify(Path::new(trimmed), &detect_cloud_roots()).map(Into::into)
}

fn machine_dto(machine: &MachinePresence, now: i64) -> IntakeMachineDto {
    IntakeMachineDto {
        machine_id: machine.machine_id.clone(),
        machine_name: machine.machine_name.clone(),
        user_name: machine.user_name.clone(),
        last_seen_at: machine.last_seen_at,
        active: presence_active(machine.last_seen_at, now),
    }
}

/// Builds the wire status. With no live `watcher` status (intake disabled, or
/// the watcher failed to start) everything scan-derived stays zeroed and
/// `error` carries the recorded startup failure; a live status supplies the
/// folder, counters, and its own error instead.
pub(crate) fn status_dto(
    enabled: bool,
    identity: &MachineIdentity,
    folder: &str,
    watcher: Option<&IntakeStatus>,
    error: Option<String>,
    now: i64,
) -> IntakeStatusDto {
    let base = IntakeStatusDto {
        enabled,
        watching: false,
        folder: display_path(Path::new(folder)),
        machine_id: identity.id.clone(),
        machine_name: identity.name.clone(),
        cloud: classify_folder(folder),
        machines: Vec::new(),
        held_for_others: 0,
        sync_conflicts: 0,
        awaiting_hydration: 0,
        unreadable_folders: 0,
        claimed_by_others: 0,
        processed_here: 0,
        last_scan_at: None,
        error,
    };
    let Some(status) = watcher else {
        return base;
    };
    let folder = status.folder.to_string_lossy().into_owned();
    IntakeStatusDto {
        watching: status.watching,
        cloud: classify_folder(&folder),
        folder: display_path(&status.folder),
        machines: status
            .machines
            .iter()
            .map(|machine| machine_dto(machine, now))
            .collect(),
        held_for_others: status.held_for_others,
        sync_conflicts: status.sync_conflicts,
        awaiting_hydration: status.awaiting_hydration,
        unreadable_folders: status.unreadable_folders,
        claimed_by_others: status.claimed_by_others,
        processed_here: status.processed_here,
        last_scan_at: status.last_scan_at,
        error: status.error.clone(),
        ..base
    }
}

/// Maps a queue item's fate onto the intake claim protocol.
///
/// Completed rule: an apply receipt that reached `Complete` — the same test
/// the queue DTO uses for "undoable" — means the source file was physically
/// renamed out of the intake folder, so it reports `Renamed` with the
/// receipt's destination leaf (falling back to the proposal filename). Any
/// other completed item (keep-original, or an apply that was later undone)
/// left the source in place and reports `KeptOriginal`.
pub(crate) fn item_fate(
    status: QueueStatus,
    receipt: Option<&OperationReceipt>,
    proposal_filename: Option<&str>,
) -> ItemState {
    match status {
        QueueStatus::Queued
        | QueueStatus::Extracting
        | QueueStatus::Analyzing
        | QueueStatus::Ready
        | QueueStatus::Applying => ItemState::Active,
        QueueStatus::NeedsReview => ItemState::NeedsReview,
        QueueStatus::Failed => ItemState::Failed,
        QueueStatus::Canceled => ItemState::Unknown,
        QueueStatus::Completed => {
            let applied = receipt.filter(|receipt| {
                receipt.direction == OperationDirection::Apply
                    && receipt.stage == OperationStage::Complete
            });
            match applied {
                Some(receipt) => ItemState::Done {
                    outcome: DoneOutcome::Renamed,
                    result_filename: receipt
                        .destination
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                        .or_else(|| proposal_filename.map(str::to_owned)),
                },
                None => ItemState::Done {
                    outcome: DoneOutcome::KeptOriginal,
                    result_filename: None,
                },
            }
        }
    }
}

/// The intake crate's view of the app: documents go into the existing
/// pipeline, and status changes become `intake://changed` events.
pub(crate) struct PipelineIntakeHost {
    pipeline: Arc<Pipeline>,
    scheduler: Sender<SchedulerMessage>,
    model_ready: Arc<AtomicBool>,
    app: AppHandle,
    identity: MachineIdentity,
}

impl PipelineIntakeHost {
    pub(crate) fn new(
        pipeline: Arc<Pipeline>,
        scheduler: Sender<SchedulerMessage>,
        model_ready: Arc<AtomicBool>,
        app: AppHandle,
        identity: MachineIdentity,
    ) -> Self {
        Self {
            pipeline,
            scheduler,
            model_ready,
            app,
            identity,
        }
    }

    /// The queue stores the canonical path handed to `enqueue`, and the
    /// watcher's paths derive from the canonical intake root, so a live file
    /// matches either literally or after canonicalization. A path that no
    /// longer canonicalizes (the apply already renamed it away) still matches
    /// literally — that is what lets a finished item report `Done` instead of
    /// `Unknown`. The newest matching item is the source's current fate;
    /// older completed rows for the same path are history. One indexed
    /// lookup per file: this is asked once per document on every scan.
    fn find_item(&self, path: &Path) -> Option<PipelineItem> {
        self.pipeline.find_by_source_path(path).ok().flatten()
    }
}

impl IntakeHost for PipelineIntakeHost {
    fn enqueue(&self, paths: &[PathBuf]) -> Result<(), String> {
        let mut canonical = Vec::with_capacity(paths.len());
        for path in paths {
            canonical.push(canonical_file(path).map_err(|error| error.code)?);
        }
        self.pipeline
            .enqueue_files(&canonical)
            .map_err(|error| error.code)?;
        // The scheduler ignores wakes until the model is ready, and setup
        // sends its own wake on becoming ready, so gating here only avoids a
        // pointless message — mirroring AppState::schedule.
        if self.model_ready.load(Ordering::SeqCst) {
            let _ = self.scheduler.send(SchedulerMessage::Wake);
        }
        Ok(())
    }

    fn item_state(&self, path: &Path) -> ItemState {
        match self.find_item(path) {
            Some(item) => item_fate(
                item.status,
                item.receipt.as_ref(),
                item.proposal
                    .as_ref()
                    .map(|record| record.filename.as_str()),
            ),
            None => ItemState::Unknown,
        }
    }

    fn abandon(&self, path: &Path) {
        if let Some(item) = self.find_item(path) {
            // Best effort: a terminal or mid-apply item is not cancelable and
            // the pipeline will say so; the claim protocol only needs pending
            // work withdrawn.
            let _ = self.pipeline.cancel(item.id);
        }
    }

    fn status_changed(&self, status: &IntakeStatus) {
        let dto = status_dto(
            true,
            &self.identity,
            &status.folder.to_string_lossy(),
            Some(status),
            None,
            now_unix(),
        );
        let _ = self.app.emit("intake://changed", dto);
    }
}

/// What the description records are doing, for Settings.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DescriptionsStatusDto {
    /// The setting, as saved.
    pub enabled: bool,
    /// Where records go: `<destination>/.intern/descriptions`, or empty when
    /// no destination is configured.
    pub folder: String,
    /// Records written since Intern started.
    pub recorded_this_session: u32,
    pub last_recorded_at: Option<i64>,
    /// The last write that failed, as `CODE: detail`, until the next success.
    pub last_error: Option<String>,
}

#[derive(Debug, Default)]
struct LedgerCounters {
    recorded: u32,
    last_recorded_at: Option<i64>,
    last_error: Option<String>,
}

/// The queue's filing sink: writes a description record for every completed
/// rename into the destination folder's ledger, and removes it again when the
/// rename is undone. Reads the settings on every call, so switching the
/// feature on or changing the destination takes effect at the next rename
/// without restarting anything. Failures never reach the rename that caused
/// them; they are kept here for Settings to show.
pub(crate) struct LedgerSink {
    settings: SettingsStore,
    data_dir: PathBuf,
    app: Mutex<Option<AppHandle>>,
    counters: Mutex<LedgerCounters>,
}

impl LedgerSink {
    pub(crate) fn new(settings: SettingsStore, data_dir: PathBuf) -> Self {
        Self {
            settings,
            data_dir,
            app: Mutex::new(None),
            counters: Mutex::new(LedgerCounters::default()),
        }
    }

    /// Attaches the app so record writes can announce themselves as
    /// `descriptions://changed` events; before this, they are silent.
    pub(crate) fn attach(&self, app: AppHandle) {
        if let Ok(mut slot) = self.app.lock() {
            *slot = Some(app);
        }
    }

    /// The ledger for the current settings, or `None` when records are
    /// switched off or there is no destination to keep them in.
    fn ledger(&self) -> Option<DescriptionLedger> {
        let settings = self.settings.load().ok()?;
        let destination = settings.destination.trim();
        if !settings.record_descriptions || destination.is_empty() {
            return None;
        }
        let identity =
            MachineIdentity::load_or_create(&self.data_dir, &settings.machine_label).ok()?;
        Some(DescriptionLedger::new(
            PathBuf::from(destination),
            identity,
            detect_cloud_roots(),
        ))
    }

    pub(crate) fn status(&self) -> DescriptionsStatusDto {
        let settings = self.settings.load().unwrap_or_default();
        let destination = settings.destination.trim();
        let folder = if destination.is_empty() {
            String::new()
        } else {
            display_path(&DescriptionLedger::directory_under(Path::new(destination)))
        };
        let counters = self
            .counters
            .lock()
            .map(|counters| LedgerCounters {
                recorded: counters.recorded,
                last_recorded_at: counters.last_recorded_at,
                last_error: counters.last_error.clone(),
            })
            .unwrap_or_default();
        DescriptionsStatusDto {
            enabled: settings.record_descriptions,
            folder,
            recorded_this_session: counters.recorded,
            last_recorded_at: counters.last_recorded_at,
            last_error: counters.last_error,
        }
    }

    /// Writes records for every document the queue has filed and not undone.
    /// Returns how many were written and how many failed; the last failure
    /// is kept for Settings.
    pub(crate) fn backfill(&self, documents: &[FiledDocument]) -> (u32, u32) {
        let Some(ledger) = self.ledger() else {
            return (0, 0);
        };
        let mut written = 0;
        let mut failed = 0;
        for document in documents {
            match ledger.record(&record_of(document)) {
                Ok(_) => {
                    written += 1;
                    self.note_success();
                }
                Err(error) => {
                    failed += 1;
                    self.note_failure(format!("DESCRIPTION_WRITE_FAILED: {error}"));
                }
            }
        }
        self.announce();
        (written, failed)
    }

    fn note_success(&self) {
        if let Ok(mut counters) = self.counters.lock() {
            counters.recorded += 1;
            counters.last_recorded_at = Some(now_unix());
            counters.last_error = None;
        }
    }

    fn note_failure(&self, error: String) {
        if let Ok(mut counters) = self.counters.lock() {
            counters.last_error = Some(error);
        }
    }

    fn announce(&self) {
        if let Ok(app) = self.app.lock()
            && let Some(app) = app.as_ref()
        {
            let _ = app.emit("descriptions://changed", self.status());
        }
    }
}

fn record_of(document: &FiledDocument) -> intern_intake::FiledDocument {
    intern_intake::FiledDocument {
        path: document.destination.clone(),
        original_filename: document
            .source_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        description: document.description.clone(),
        document_date: document.proposal.document_date.clone(),
        document_type: document.proposal.document_type.clone(),
        parties: document.proposal.parties.clone(),
        confidence: Some(document.proposal.confidence),
        filed_at: document.filed_at,
    }
}

impl FilingSink for LedgerSink {
    fn filed(&self, document: &FiledDocument) {
        let Some(ledger) = self.ledger() else {
            return;
        };
        match ledger.record(&record_of(document)) {
            Ok(_) => self.note_success(),
            Err(error) => self.note_failure(format!("DESCRIPTION_WRITE_FAILED: {error}")),
        }
        self.announce();
    }

    fn unfiled(&self, destination: &Path) {
        let Some(ledger) = self.ledger() else {
            return;
        };
        if let Err(error) = ledger.retract(destination) {
            self.note_failure(format!("DESCRIPTION_RETRACT_FAILED: {error}"));
        }
        self.announce();
    }
}
