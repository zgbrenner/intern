//! The polling watcher thread: walks the intake folder, runs the claim
//! protocol for each stable file, and feeds newly claimed documents to the
//! host queue.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::{self, JoinHandle},
    time::Instant,
};

use crate::{
    coordination::{
        AcquireOutcome, COURTESY_DELAY_SECONDS, ClaimState, ClaimStore, Clock, DocumentFacts,
        DoneOutcome, SystemClock,
    },
    identity::MachineIdentity,
    scan::{
        FileFacts, Hydration, IntakeConfig, IntakeHost, IntakeStatus, ItemState, StabilityTracker,
        SystemHydration, is_conflict_copy, walk_intake,
    },
};

pub struct IntakeWatcher {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

struct Shared {
    control: Mutex<Control>,
    wake: Condvar,
    status: Mutex<IntakeStatus>,
}

struct Control {
    config: IntakeConfig,
    generation: u64,
    wake: bool,
    shutdown: bool,
}

impl IntakeWatcher {
    pub fn start(
        config: IntakeConfig,
        identity: MachineIdentity,
        host: Arc<dyn IntakeHost>,
    ) -> IntakeWatcher {
        Self::start_with_clock(config, identity, host, Arc::new(SystemClock))
    }

    pub fn start_with_clock(
        config: IntakeConfig,
        identity: MachineIdentity,
        host: Arc<dyn IntakeHost>,
        clock: Arc<dyn Clock>,
    ) -> IntakeWatcher {
        Self::start_with_seams(config, identity, host, clock, Arc::new(SystemHydration))
    }

    pub fn start_with_seams(
        config: IntakeConfig,
        identity: MachineIdentity,
        host: Arc<dyn IntakeHost>,
        clock: Arc<dyn Clock>,
        hydration: Arc<dyn Hydration>,
    ) -> IntakeWatcher {
        let shared = Arc::new(Shared {
            status: Mutex::new(IntakeStatus::idle(config.intake_root.clone())),
            control: Mutex::new(Control {
                config,
                generation: 0,
                wake: false,
                shutdown: false,
            }),
            wake: Condvar::new(),
        });
        let thread = thread::Builder::new()
            .name("intern-intake-watcher".to_string())
            .spawn({
                let shared = shared.clone();
                move || run(&shared, &identity, host, clock, hydration)
            })
            .expect("intake watcher thread could not be spawned");
        IntakeWatcher {
            shared,
            thread: Some(thread),
        }
    }

    pub fn status(&self) -> IntakeStatus {
        lock(&self.shared.status).clone()
    }

    /// Wakes the loop for an immediate scan.
    pub fn scan_now(&self) {
        lock(&self.shared.control).wake = true;
        self.shared.wake.notify_all();
    }

    /// Re-arms the running watcher on a new configuration. Per-folder scan
    /// state (backlog, stability, owned claims) is discarded, exactly as a
    /// stop-and-start would discard it.
    pub fn update_config(&self, config: IntakeConfig) {
        {
            let mut control = lock(&self.shared.control);
            control.config = config;
            control.generation += 1;
            control.wake = true;
        }
        self.shared.wake.notify_all();
    }
}

/// The thread must never be detached: a detached scanner would keep claiming
/// documents for a host that is shutting down.
impl Drop for IntakeWatcher {
    fn drop(&mut self) {
        lock(&self.shared.control).shutdown = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct ScanState {
    store: Option<ClaimStore>,
    stability: StabilityTracker,
    /// Relative paths already present when watching started. Files that
    /// predate the watcher have no known uploader, so "mine" scope leaves
    /// them alone rather than guessing.
    backlog: HashSet<String>,
    backlog_recorded: bool,
    /// Claims this machine believes it holds, so a takeover or sync conflict
    /// that rewrites a claim file is noticed and the local item abandoned.
    owned: HashMap<String, PathBuf>,
    /// Claims kept open because the document failed while its content was
    /// still in the cloud. Their fate is decided once the bytes arrive.
    awaiting_hydration: HashSet<String>,
}

fn run(
    shared: &Shared,
    identity: &MachineIdentity,
    host: Arc<dyn IntakeHost>,
    clock: Arc<dyn Clock>,
    hydration: Arc<dyn Hydration>,
) {
    let mut state = ScanState::default();
    let mut state_generation = 0_u64;
    let mut last_reported: Option<IntakeStatus> = None;
    loop {
        let (config, generation) = {
            let mut control = lock(&shared.control);
            if control.shutdown {
                break;
            }
            control.wake = false;
            (control.config.clone(), control.generation)
        };
        if generation != state_generation {
            state = ScanState::default();
            state_generation = generation;
        }
        let status = scan_once(
            &config,
            identity,
            host.as_ref(),
            &clock,
            hydration.as_ref(),
            &mut state,
        );
        *lock(&shared.status) = status.clone();
        if last_reported
            .as_ref()
            .is_none_or(|previous| previous.materially_differs(&status))
        {
            host.status_changed(&status);
            last_reported = Some(status);
        }
        let deadline = Instant::now() + config.scan_interval;
        let mut control = lock(&shared.control);
        while !control.wake && !control.shutdown {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            control = shared
                .wake
                .wait_timeout(control, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

fn scan_once(
    config: &IntakeConfig,
    identity: &MachineIdentity,
    host: &dyn IntakeHost,
    clock: &Arc<dyn Clock>,
    hydration: &dyn Hydration,
    state: &mut ScanState,
) -> IntakeStatus {
    // Stamped at the start of the walk: the timestamp then vouches that
    // everything on disk up to that instant has been observed.
    let scan_started = clock.now();
    let mut status = IntakeStatus::idle(config.intake_root.clone());
    status.last_scan_at = Some(scan_started);
    let ScanState {
        store,
        stability,
        backlog,
        backlog_recorded,
        owned,
        awaiting_hydration,
    } = state;
    if store.is_none() {
        match ClaimStore::with_clock(&config.intake_root, identity.clone(), clock.clone()) {
            Ok(created) => *store = Some(created),
            Err(error) => {
                status.error = Some(format!("INTAKE_COORDINATION_UNAVAILABLE: {error}"));
                return status;
            }
        }
    }
    let store = store.as_ref().expect("claim store was just created");

    let files = match walk_intake(&config.intake_root, &config.extensions) {
        Ok(files) => files,
        Err(error) => {
            status.error = Some(format!("INTAKE_FOLDER_UNAVAILABLE: {error}"));
            return status;
        }
    };

    // Read before the walk is processed so a conflict copy is recognised on the
    // same scan it appears, and include this machine: OneDrive names the losing
    // side of a conflict after whichever machine wrote it, which is often us.
    let mut machines: Vec<String> = store
        .list_machines()
        .into_iter()
        .map(|presence| presence.machine_name)
        .collect();
    machines.push(identity.name.clone());

    let mut scanner = Scanner {
        config,
        identity,
        host,
        hydration,
        machines,
        clock: clock.as_ref(),
        store,
        stability,
        backlog,
        record_backlog: !*backlog_recorded,
        owned,
        awaiting_hydration,
        status: &mut status,
        visited: HashSet::new(),
        live: HashSet::new(),
    };
    for facts in &files {
        scanner.process_file(facts);
    }
    scanner.finish_unseen_owned();
    let live = scanner.live;
    *backlog_recorded = true;
    stability.retain_live(&live);

    if let Err(error) = store.touch_presence() {
        status.error = Some(format!("PRESENCE_WRITE_FAILED: {error}"));
    }
    store.prune();
    status.machines = store.list_machines();
    status
}

struct Scanner<'a> {
    config: &'a IntakeConfig,
    identity: &'a MachineIdentity,
    host: &'a dyn IntakeHost,
    hydration: &'a dyn Hydration,
    machines: Vec<String>,
    clock: &'a dyn Clock,
    store: &'a ClaimStore,
    stability: &'a mut StabilityTracker,
    backlog: &'a mut HashSet<String>,
    record_backlog: bool,
    owned: &'a mut HashMap<String, PathBuf>,
    awaiting_hydration: &'a mut HashSet<String>,
    status: &'a mut IntakeStatus,
    visited: HashSet<String>,
    live: HashSet<PathBuf>,
}

impl Scanner<'_> {
    fn process_file(&mut self, facts: &FileFacts) {
        self.live.insert(facts.path.clone());
        // A conflict copy is the sync client's bookkeeping, not a new document.
        // Naming it would file a second copy of something already filed, so it
        // is counted and left where it is for a person to resolve.
        if is_conflict_copy(&facts.path, &self.machines) {
            self.status.sync_conflicts += 1;
            return;
        }
        if self.record_backlog {
            self.backlog.insert(facts.relative_path.clone());
        }
        if !self
            .stability
            .observe(&facts.path, facts.size, facts.modified_secs)
        {
            return;
        }
        let doc = DocumentFacts {
            relative_path: facts.relative_path.clone(),
            size: facts.size,
            modified_secs: facts.modified_secs,
        };
        let key = doc.key();
        self.visited.insert(key.clone());

        if self.owned.contains_key(&key) {
            if self.store.verify(&key) {
                self.manage_owned(&key, &facts.path, true);
            } else {
                self.owned.remove(&key);
                self.host.abandon(&facts.path);
            }
            return;
        }

        match self.store.read(&key) {
            Some(claim) if claim.machine_id == self.identity.id => match claim.state {
                ClaimState::Claimed => {
                    // A claim from a previous run of this machine: adopt it and
                    // let the host's item state drive it forward again.
                    self.owned.insert(key.clone(), facts.path.clone());
                    self.manage_owned(&key, &facts.path, true);
                }
                ClaimState::Done => self.status.processed_here += 1,
            },
            Some(claim) => {
                if claim.state == ClaimState::Claimed {
                    self.status.claimed_by_others += 1;
                }
            }
            None => self.consider_unclaimed(&doc, &key, facts),
        }
    }

    /// Scope and ownership rules for a file nobody has claimed.
    ///
    /// The courtesy delay gives the uploader's own machine first shot at its
    /// files even in "everyone" scope: origin markers and fresh files travel
    /// through the sync layer with the same latency, so claiming a
    /// seconds-old file here would routinely beat the uploader's own claim
    /// and shuttle the result across machines for no reason.
    fn consider_unclaimed(&mut self, doc: &DocumentFacts, key: &str, facts: &FileFacts) {
        let mut new_local = false;
        let claimable = match self.store.read_origin(key) {
            Some(origin) if origin.machine_id == self.identity.id => true,
            Some(_) => self.others_claimable(facts),
            // No origin marker: a file already present when watching started
            // has an unknown uploader and is treated like someone else's;
            // one that appeared later must have been put here locally.
            None if self.backlog.contains(&facts.relative_path) => self.others_claimable(facts),
            None => {
                new_local = true;
                true
            }
        };
        if !claimable {
            self.status.held_for_others += 1;
            return;
        }
        if new_local && let Err(error) = self.store.write_origin(doc) {
            self.status.error = Some(format!("ORIGIN_WRITE_FAILED: {error}"));
        }
        self.attempt_claim(doc, key, &facts.path);
    }

    fn others_claimable(&self, facts: &FileFacts) -> bool {
        self.config.process_others_uploads
            && self.clock.now() - facts.modified_secs >= COURTESY_DELAY_SECONDS
    }

    fn attempt_claim(&mut self, doc: &DocumentFacts, key: &str, path: &Path) {
        match self.store.acquire(doc) {
            AcquireOutcome::Acquired => {
                if !self.store.verify(key) {
                    // Replaced under us before anything was enqueued; whoever
                    // owns the surviving claim keeps the document.
                    return;
                }
                match self.host.enqueue(&[path.to_path_buf()]) {
                    Ok(()) => {
                        self.owned.insert(key.to_string(), path.to_path_buf());
                    }
                    Err(message) => {
                        let _ = self.store.release(key);
                        self.status.error = Some(format!("ENQUEUE_FAILED: {message}"));
                    }
                }
            }
            AcquireOutcome::HeldByOther(_) => self.status.claimed_by_others += 1,
            AcquireOutcome::Done(_) => {}
            AcquireOutcome::Failed(error) => {
                self.status.error = Some(format!("CLAIM_IO_FAILED: {error}"));
            }
        }
    }

    /// Drives an owned claim according to what the host reports about the
    /// item. `file_present` distinguishes a released document (still on disk,
    /// claimable again) from one the user deleted (tombstoned as `Removed`
    /// so the claim does not linger as a claimed lease forever).
    fn manage_owned(&mut self, key: &str, path: &Path, file_present: bool) {
        match self.host.item_state(path) {
            ItemState::Active | ItemState::NeedsReview => {
                if self.store.renew(key).is_err() {
                    self.owned.remove(key);
                    self.host.abandon(path);
                }
            }
            ItemState::Done {
                outcome,
                result_filename,
            } => self.finish_owned(key, outcome, result_filename.as_deref()),
            ItemState::Failed => self.finish_failed(key, path),
            ItemState::Unknown => {
                if file_present {
                    let _ = self.store.release(key);
                } else {
                    let _ = self.store.mark_done(key, DoneOutcome::Removed, None);
                }
                self.owned.remove(key);
            }
        }
    }

    /// A document that failed while its bytes were still in the cloud has not
    /// been judged - nothing ever read it. Tombstoning it there would strand a
    /// perfectly good document behind a laptop that happened to be offline, and
    /// the tombstone outlives the trip. So the claim is held, not closed, and
    /// the verdict waits for the content.
    ///
    /// Once the bytes arrive the claim is released rather than retried in
    /// place: the next scan re-acquires it and the document goes through the
    /// pipeline again, now with something to read. A second failure with the
    /// content local is a real failure and tombstones normally, so this can
    /// forgive a document exactly once per trip through the cloud.
    fn finish_failed(&mut self, key: &str, path: &Path) {
        if self.hydration.is_dehydrated(path) {
            // Counted only once the lease is actually held: a document we just
            // abandoned is not one we are waiting on.
            if self.store.renew(key).is_err() {
                self.owned.remove(key);
                self.awaiting_hydration.remove(key);
                self.host.abandon(path);
                return;
            }
            self.awaiting_hydration.insert(key.to_owned());
            self.status.awaiting_hydration += 1;
            return;
        }
        if self.awaiting_hydration.remove(key) {
            let _ = self.store.release(key);
            self.owned.remove(key);
            return;
        }
        self.finish_owned(key, DoneOutcome::Failed, None);
    }

    fn finish_owned(&mut self, key: &str, outcome: DoneOutcome, result_filename: Option<&str>) {
        match self.store.mark_done(key, outcome, result_filename) {
            Ok(()) => {
                self.owned.remove(key);
                self.status.processed_here += 1;
            }
            Err(error) => self.status.error = Some(format!("CLAIM_UPDATE_FAILED: {error}")),
        }
    }

    /// Owned claims whose file was not seen at the same key this scan: the
    /// file was deleted, or changed and now lives under a new key. The claim
    /// still needs shepherding or it would sit as a claimed lease until some
    /// other machine's takeover math has to deal with it.
    fn finish_unseen_owned(&mut self) {
        let unseen: Vec<(String, PathBuf)> = self
            .owned
            .iter()
            .filter(|(key, _)| !self.visited.contains(*key))
            .map(|(key, path)| (key.clone(), path.clone()))
            .collect();
        for (key, path) in unseen {
            if !self.store.verify(&key) {
                self.owned.remove(&key);
                self.host.abandon(&path);
                continue;
            }
            self.manage_owned(&key, &path, path.exists());
        }
        // A claim we no longer hold - deleted, taken over, abandoned - is not
        // one we are waiting on the cloud for. Without this the set grows for
        // the life of the process and a later file landing on the same key
        // would inherit a stale forgiveness.
        let owned = &*self.owned;
        self.awaiting_hydration.retain(|key| owned.contains_key(key));
    }
}
