mod common;

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use common::{MockClock, facts_for, identity, wait_until};
use intern_intake::{
    COURTESY_DELAY_SECONDS, ClaimInfo, ClaimState, ClaimStore, DoneOutcome, IntakeConfig,
    IntakeHost, IntakeStatus, IntakeWatcher, ItemState,
};
use tempfile::TempDir;

/// In-memory host: records what the watcher hands over and answers
/// `item_state` from a scriptable map, defaulting to `Unknown` like a queue
/// that has never seen the path.
#[derive(Default)]
struct FakeHost {
    enqueued: Mutex<Vec<PathBuf>>,
    abandoned: Mutex<Vec<PathBuf>>,
    states: Mutex<HashMap<PathBuf, ItemState>>,
    statuses: Mutex<Vec<IntakeStatus>>,
    fail_enqueue: AtomicBool,
}

impl FakeHost {
    fn enqueued(&self) -> Vec<PathBuf> {
        self.enqueued.lock().unwrap().clone()
    }

    fn abandoned(&self) -> Vec<PathBuf> {
        self.abandoned.lock().unwrap().clone()
    }

    fn set_state(&self, path: &Path, state: ItemState) {
        self.states
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), state);
    }
}

impl IntakeHost for FakeHost {
    fn enqueue(&self, paths: &[PathBuf]) -> Result<(), String> {
        if self.fail_enqueue.load(Ordering::SeqCst) {
            return Err("the queue is unavailable".to_string());
        }
        let mut states = self.states.lock().unwrap();
        for path in paths {
            states.insert(path.clone(), ItemState::Active);
        }
        self.enqueued.lock().unwrap().extend(paths.iter().cloned());
        Ok(())
    }

    fn item_state(&self, path: &Path) -> ItemState {
        self.states
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .unwrap_or(ItemState::Unknown)
    }

    fn abandon(&self, path: &Path) {
        self.abandoned.lock().unwrap().push(path.to_path_buf());
    }

    fn status_changed(&self, status: &IntakeStatus) {
        self.statuses.lock().unwrap().push(status.clone());
    }
}

/// Deterministic harness: an hour-long scan interval means the loop only
/// moves when `step` wakes it, and the mock clock stamps every tick uniquely
/// so `step` can wait for exactly the scan it triggered.
struct Rig {
    temp: TempDir,
    clock: Arc<MockClock>,
    host: Arc<FakeHost>,
    watcher: IntakeWatcher,
}

impl Rig {
    fn start(process_others_uploads: bool, backlog_files: &[&str]) -> Rig {
        let temp = TempDir::new().unwrap();
        for name in backlog_files {
            fs::write(temp.path().join(name), b"backlog content").unwrap();
        }
        let clock = MockClock::at_real_now();
        let host = Arc::new(FakeHost::default());
        let mut config = IntakeConfig::new(temp.path(), vec!["pdf".to_string(), "txt".to_string()]);
        config.process_others_uploads = process_others_uploads;
        config.scan_interval = Duration::from_secs(3600);
        let watcher = IntakeWatcher::start_with_clock(
            config,
            identity("here-machine", "here"),
            host.clone(),
            clock.clone(),
        );
        wait_until("the initial scan", || {
            watcher.status().last_scan_at.is_some()
        });
        Rig {
            temp,
            clock,
            host,
            watcher,
        }
    }

    /// Triggers exactly one scan and waits for it to complete.
    fn step(&self) {
        let target = self.clock.advance(1);
        self.watcher.scan_now();
        wait_until("a scan tick", || {
            self.watcher.status().last_scan_at == Some(target)
        });
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.temp.path().join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn claim_file(&self, key: &str) -> PathBuf {
        self.temp
            .path()
            .join(".intern")
            .join("claims")
            .join(format!("{key}.json"))
    }

    fn read_claim(&self, key: &str) -> ClaimInfo {
        serde_json::from_slice(&fs::read(self.claim_file(key)).unwrap()).unwrap()
    }
}

#[test]
fn a_new_stable_file_is_claimed_enqueued_and_marked_done_after_the_host_finishes() {
    let rig = Rig::start(false, &[]);
    let path = rig.write("contract.pdf", b"agreement text");
    let key = facts_for(rig.temp.path(), "contract.pdf").key();

    rig.step();
    assert!(
        rig.host.enqueued().is_empty(),
        "a first sighting is not yet stable"
    );
    rig.step();
    assert_eq!(rig.host.enqueued(), vec![path.clone()]);

    let claim = rig.read_claim(&key);
    assert_eq!(claim.machine_id, "here-machine");
    assert_eq!(claim.state, ClaimState::Claimed);
    let store = ClaimStore::new(rig.temp.path(), identity("other", "elsewhere")).unwrap();
    assert_eq!(
        store.read_origin(&key).unwrap().machine_id,
        "here-machine",
        "a file appearing after watch start is attributed to this machine"
    );

    rig.host.set_state(
        &path,
        ItemState::Done {
            outcome: DoneOutcome::Renamed,
            result_filename: Some("2024 Contract.pdf".to_string()),
        },
    );
    rig.step();
    let done = rig.read_claim(&key);
    assert_eq!(done.state, ClaimState::Done);
    assert_eq!(done.outcome, Some(DoneOutcome::Renamed));
    assert_eq!(done.result_filename.as_deref(), Some("2024 Contract.pdf"));

    rig.step();
    let status = rig.watcher.status();
    assert_eq!(status.processed_here, 1);
    assert!(status.watching);
    assert_eq!(status.folder, rig.temp.path());
    assert!(
        status
            .machines
            .iter()
            .any(|machine| machine.machine_id == "here-machine"),
        "presence must include this machine: {status:?}"
    );
    assert!(
        rig.host.statuses.lock().unwrap().iter().any(|s| s.watching),
        "status_changed must have been reported to the host"
    );
}

#[test]
fn files_that_predate_the_watcher_are_held_for_others_in_mine_scope() {
    let rig = Rig::start(false, &["old-report.pdf"]);
    // Advance far past the courtesy delay: scope, not age, is what holds here.
    rig.clock.advance(10 * COURTESY_DELAY_SECONDS);
    rig.step();
    rig.step();
    assert!(rig.host.enqueued().is_empty());
    assert_eq!(rig.watcher.status().held_for_others, 1);
}

#[test]
fn anothers_upload_is_claimed_only_after_the_courtesy_delay_in_everyone_scope() {
    let rig = Rig::start(true, &[]);
    rig.step();
    let path = rig.write("their-scan.pdf", b"uploaded elsewhere");
    let other = ClaimStore::new(rig.temp.path(), identity("other-machine", "elsewhere")).unwrap();
    other
        .write_origin(&facts_for(rig.temp.path(), "their-scan.pdf"))
        .unwrap();

    rig.step();
    rig.step();
    assert!(
        rig.host.enqueued().is_empty(),
        "the uploader's machine gets first shot during the courtesy delay"
    );
    assert_eq!(rig.watcher.status().held_for_others, 1);

    rig.clock.advance(COURTESY_DELAY_SECONDS);
    rig.step();
    assert_eq!(rig.host.enqueued(), vec![path]);
    assert_eq!(rig.watcher.status().held_for_others, 0);
}

#[test]
fn anothers_upload_is_never_claimed_in_mine_scope_even_after_the_delay() {
    let rig = Rig::start(false, &[]);
    rig.step();
    rig.write("their-scan.pdf", b"uploaded elsewhere");
    let other = ClaimStore::new(rig.temp.path(), identity("other-machine", "elsewhere")).unwrap();
    other
        .write_origin(&facts_for(rig.temp.path(), "their-scan.pdf"))
        .unwrap();
    rig.clock.advance(10 * COURTESY_DELAY_SECONDS);
    rig.step();
    rig.step();
    assert!(rig.host.enqueued().is_empty());
    assert_eq!(rig.watcher.status().held_for_others, 1);
}

#[test]
fn a_backlog_file_is_claimed_in_everyone_scope_once_the_courtesy_delay_passes() {
    let rig = Rig::start(true, &["unattributed.pdf"]);
    rig.step();
    assert!(
        rig.host.enqueued().is_empty(),
        "still inside the courtesy delay"
    );
    rig.clock.advance(COURTESY_DELAY_SECONDS);
    rig.step();
    assert_eq!(
        rig.host.enqueued(),
        vec![rig.temp.path().join("unattributed.pdf")]
    );
}

#[test]
fn a_claim_lost_to_a_sync_conflict_makes_the_watcher_abandon_the_item() {
    let rig = Rig::start(false, &[]);
    rig.step();
    let path = rig.write("contested.pdf", b"contested content");
    let facts = facts_for(rig.temp.path(), "contested.pdf");
    let key = facts.key();
    rig.step();
    rig.step();
    assert_eq!(rig.host.enqueued(), vec![path.clone()]);

    let mut stolen = rig.read_claim(&key);
    stolen.machine_id = "other-machine".to_string();
    stolen.machine_name = "elsewhere".to_string();
    fs::write(
        rig.claim_file(&key),
        serde_json::to_vec_pretty(&stolen).unwrap(),
    )
    .unwrap();

    rig.step();
    assert_eq!(rig.host.abandoned(), vec![path]);
    rig.step();
    assert_eq!(
        rig.watcher.status().claimed_by_others,
        1,
        "after abandoning, the foreign claim is counted like any other"
    );
}

#[test]
fn a_claimed_file_deleted_by_the_user_leaves_a_removed_tombstone() {
    let rig = Rig::start(false, &[]);
    rig.step();
    let path = rig.write("withdrawn.pdf", b"changed their mind");
    let key = facts_for(rig.temp.path(), "withdrawn.pdf").key();
    rig.step();
    rig.step();
    assert_eq!(rig.host.enqueued(), vec![path.clone()]);

    fs::remove_file(&path).unwrap();
    rig.host.set_state(&path, ItemState::Unknown);
    rig.step();
    let claim = rig.read_claim(&key);
    assert_eq!(claim.state, ClaimState::Done);
    assert_eq!(claim.outcome, Some(DoneOutcome::Removed));
}

#[test]
fn a_file_changing_between_scans_is_not_claimed_until_it_settles() {
    let rig = Rig::start(false, &[]);
    rig.step();
    let path = rig.write("uploading.pdf", b"first chunk");
    rig.step();
    for chunk in 0..3 {
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "chunk {chunk}").unwrap();
        drop(file);
        rig.step();
        assert!(
            rig.host.enqueued().is_empty(),
            "a growing file must never be claimed (iteration {chunk})"
        );
    }
    rig.step();
    assert_eq!(
        rig.host.enqueued(),
        vec![path],
        "one quiet scan interval proves stability"
    );
}

#[test]
fn an_enqueue_failure_releases_the_claim_so_a_later_scan_can_retry() {
    let rig = Rig::start(false, &[]);
    rig.step();
    rig.host.fail_enqueue.store(true, Ordering::SeqCst);
    let path = rig.write("retry.pdf", b"try me twice");
    let key = facts_for(rig.temp.path(), "retry.pdf").key();
    rig.step();
    rig.step();
    assert!(rig.host.enqueued().is_empty());
    assert!(
        !rig.claim_file(&key).exists(),
        "a claim without a queued item would deadlock the document"
    );
    assert!(
        rig.watcher
            .status()
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("ENQUEUE_FAILED")),
        "status: {:?}",
        rig.watcher.status()
    );

    rig.host.fail_enqueue.store(false, Ordering::SeqCst);
    rig.step();
    assert_eq!(rig.host.enqueued(), vec![path]);
    assert!(rig.claim_file(&key).exists());
    assert_eq!(
        rig.watcher.status().error,
        None,
        "a clean scan clears the error"
    );
}

#[test]
fn a_file_claimed_by_another_machine_is_counted_and_left_alone() {
    let rig = Rig::start(true, &[]);
    rig.step();
    rig.write("busy-elsewhere.pdf", b"already being processed");
    let facts = facts_for(rig.temp.path(), "busy-elsewhere.pdf");
    let other = ClaimStore::new(rig.temp.path(), identity("other-machine", "elsewhere")).unwrap();
    other.write_origin(&facts).unwrap();
    assert!(matches!(
        other.acquire(&facts),
        intern_intake::AcquireOutcome::Acquired
    ));
    rig.clock.advance(10 * COURTESY_DELAY_SECONDS);
    rig.step();
    rig.step();
    assert!(rig.host.enqueued().is_empty());
    let status = rig.watcher.status();
    assert_eq!(status.claimed_by_others, 1);
    assert_eq!(status.held_for_others, 0);
    let claim = rig.read_claim(&facts.key());
    assert_eq!(claim.machine_id, "other-machine");
}

#[test]
fn update_config_rearms_on_a_new_folder_and_rebuilds_the_backlog() {
    let rig = Rig::start(false, &[]);
    rig.step();
    let old_path = rig.write("first.pdf", b"first folder");
    rig.step();
    rig.step();
    assert_eq!(rig.host.enqueued(), vec![old_path]);

    let second = TempDir::new().unwrap();
    fs::write(second.path().join("pre-existing.pdf"), b"was already here").unwrap();
    let mut config = IntakeConfig::new(second.path(), vec!["pdf".to_string()]);
    config.scan_interval = Duration::from_secs(3600);
    rig.watcher.update_config(config);
    wait_until("the watcher to adopt the new folder", || {
        rig.watcher.status().folder == second.path()
    });
    rig.step();
    rig.step();
    let status = rig.watcher.status();
    assert_eq!(status.folder, second.path());
    assert_eq!(
        status.held_for_others, 1,
        "the new folder's pre-existing file is backlog again: {status:?}"
    );
    assert_eq!(rig.host.enqueued().len(), 1, "nothing new was enqueued");
}

#[test]
fn status_changed_fires_only_on_material_changes_not_every_tick() {
    let rig = Rig::start(false, &["held.pdf"]);
    rig.step();
    rig.step();
    let reported = rig.host.statuses.lock().unwrap().len();
    rig.step();
    rig.step();
    rig.step();
    assert_eq!(
        rig.host.statuses.lock().unwrap().len(),
        reported,
        "ticks that only advance last_scan_at must not wake the host"
    );
}

#[test]
fn skip_rules_ignore_dotfiles_office_locks_unsupported_and_empty_files() {
    let rig = Rig::start(false, &[]);
    rig.step();
    rig.write(".hidden.pdf", b"dotfile");
    rig.write("~$lock.pdf", b"office lock");
    rig.write("notes.xyz", b"unsupported extension");
    rig.write("empty.pdf", b"");
    fs::create_dir(rig.temp.path().join("nested")).unwrap();
    let nested = rig.temp.path().join("nested").join("deep.txt");
    fs::write(&nested, b"nested but supported").unwrap();
    rig.step();
    rig.step();
    assert_eq!(rig.host.enqueued(), vec![nested]);
    let status = rig.watcher.status();
    assert_eq!(status.held_for_others, 0);
    assert_eq!(status.claimed_by_others, 0);
}

#[test]
fn dropping_the_watcher_joins_the_scan_thread() {
    let rig = Rig::start(false, &[]);
    rig.step();
    let Rig {
        temp,
        clock,
        host,
        watcher,
    } = rig;
    // A hang here (a detached or stuck thread) fails the test by timeout.
    drop(watcher);
    drop(clock);
    drop(host);
    drop(temp);
}
