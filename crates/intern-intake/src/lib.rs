//! Shared intake folders: the multi-machine claim protocol, cloud sync-root
//! awareness, and the polling watcher that feeds documents to a host queue.
//!
//! Multiple machines may watch the same OneDrive/SharePoint-synced folder.
//! They coordinate through small JSON files in `<intake>/.intern/` that the
//! sync client replicates — best-effort leases with a done tombstone, origin
//! markers naming the uploading machine, and machine presence. The sync layer
//! is eventually consistent, so nothing here is a hard lock; the existing
//! fingerprint/CAS apply machinery in `intern-core` is the backstop that
//! keeps a lost race from ever double-renaming a document.

#![deny(unsafe_code)]

pub mod cloud;
pub mod coordination;
mod fsatomic;
pub mod identity;
pub mod scan;
pub mod watcher;

pub use cloud::{
    CloudLocation, CloudProviderKind, CloudRoot, EnvProbe, SystemEnv, classify, detect_cloud_roots,
    detect_cloud_roots_with,
};
pub use coordination::{
    AcquireOutcome, CLAIM_LEASE_SECONDS, CLAIM_RENEW_THRESHOLD_SECONDS, COURTESY_DELAY_SECONDS,
    ClaimInfo, ClaimState, ClaimStore, Clock, DONE_RETENTION_SECONDS, DocumentFacts, DoneOutcome,
    MachinePresence, OriginInfo, PRESENCE_ACTIVE_WINDOW_SECONDS, PRESENCE_REFRESH_SECONDS,
    SystemClock, document_key,
};
pub use identity::MachineIdentity;
pub use scan::{
    DEFAULT_SCAN_INTERVAL, IntakeConfig, IntakeHost, IntakeStatus, ItemState, StabilityTracker,
};
pub use watcher::IntakeWatcher;
