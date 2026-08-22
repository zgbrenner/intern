//! Shared-intake wiring for the Tauri host: the wire DTOs for the intake
//! commands and the `intake://changed` event, and the pipeline-backed
//! implementation of the intake crate's host boundary.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use intern_core::{OperationDirection, OperationReceipt, OperationStage, QueueStatus};
use intern_intake::{
    CloudLocation, CloudProviderKind, DoneOutcome, IntakeHost, IntakeStatus, ItemState,
    MachineIdentity, MachinePresence, PRESENCE_ACTIVE_WINDOW_SECONDS, classify, detect_cloud_roots,
};
use intern_queue::{Pipeline, PipelineItem, paths::canonical_file};
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
}

impl From<CloudProviderKind> for CloudProviderDto {
    fn from(kind: CloudProviderKind) -> Self {
        match kind {
            CloudProviderKind::OneDrivePersonal => Self::OneDrivePersonal,
            CloudProviderKind::OneDriveBusiness => Self::OneDriveBusiness,
            CloudProviderKind::SharePoint => Self::SharePoint,
        }
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
        folder: folder.to_owned(),
        machine_id: identity.id.clone(),
        machine_name: identity.name.clone(),
        cloud: classify_folder(folder),
        machines: Vec::new(),
        held_for_others: 0,
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
        folder,
        machines: status
            .machines
            .iter()
            .map(|machine| machine_dto(machine, now))
            .collect(),
        held_for_others: status.held_for_others,
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
    /// older completed rows for the same path are history.
    fn find_item(&self, path: &Path) -> Option<PipelineItem> {
        let canonical = std::fs::canonicalize(path).ok();
        self.pipeline.list().ok()?.into_iter().rfind(|item| {
            item.source_path.as_path() == path
                || canonical
                    .as_deref()
                    .is_some_and(|canonical| item.source_path.as_path() == canonical)
        })
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
