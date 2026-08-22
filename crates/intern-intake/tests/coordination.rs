mod common;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Barrier,
    thread,
};

use common::{MockClock, facts_for, identity, real_now};
use intern_intake::{
    AcquireOutcome, CLAIM_LEASE_SECONDS, CLAIM_RENEW_THRESHOLD_SECONDS, ClaimInfo, ClaimState,
    ClaimStore, DONE_RETENTION_SECONDS, DocumentFacts, DoneOutcome, MachinePresence,
    PRESENCE_REFRESH_SECONDS, document_key,
};
use tempfile::TempDir;

fn doc(relative: &str) -> DocumentFacts {
    DocumentFacts {
        relative_path: relative.to_string(),
        size: 1234,
        modified_secs: 1_700_000_000,
    }
}

fn claim_file(root: &Path, key: &str) -> PathBuf {
    root.join(".intern")
        .join("claims")
        .join(format!("{key}.json"))
}

fn read_claim(root: &Path, key: &str) -> ClaimInfo {
    serde_json::from_slice(&fs::read(claim_file(root, key)).unwrap()).unwrap()
}

fn foreign_claim(key: &str, doc: &DocumentFacts, machine_id: &str, now: i64) -> ClaimInfo {
    ClaimInfo {
        version: 1,
        key: key.to_string(),
        relative_path: doc.relative_path.clone(),
        size: doc.size,
        modified_at: doc.modified_secs,
        machine_id: machine_id.to_string(),
        machine_name: "elsewhere".to_string(),
        user_name: "someone".to_string(),
        state: ClaimState::Claimed,
        claimed_at: now,
        lease_expires_at: now + CLAIM_LEASE_SECONDS,
        heartbeat_at: now,
        done_at: None,
        outcome: None,
        result_filename: None,
    }
}

#[test]
fn document_key_normalizes_case_and_separators_but_not_size_or_mtime() {
    let key = document_key("Sub\\Contract.PDF", 10, 20);
    assert_eq!(key, document_key("sub/contract.pdf", 10, 20));
    assert_ne!(key, document_key("sub/contract.pdf", 11, 20));
    assert_ne!(key, document_key("sub/contract.pdf", 10, 21));
    assert_eq!(key.len(), 64);
    assert!(
        key.bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    );
}

#[test]
fn machines_racing_the_same_document_produce_exactly_one_claim() {
    let temp = TempDir::new().unwrap();
    let clock = MockClock::at(1_000_000);
    let document = doc("race.pdf");
    let barrier = Barrier::new(4);
    let stores: Vec<ClaimStore> = (0..4)
        .map(|index| {
            ClaimStore::with_clock(
                temp.path(),
                identity(&format!("machine-{index}"), "racer"),
                clock.clone(),
            )
            .unwrap()
        })
        .collect();
    let outcomes: Vec<AcquireOutcome> = thread::scope(|scope| {
        let handles: Vec<_> = stores
            .iter()
            .map(|store| {
                scope.spawn(|| {
                    barrier.wait();
                    store.acquire(&document)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });
    let winners: Vec<usize> = outcomes
        .iter()
        .enumerate()
        .filter(|(_, outcome)| matches!(outcome, AcquireOutcome::Acquired))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(winners.len(), 1, "outcomes: {outcomes:?}");
    assert!(
        outcomes
            .iter()
            .filter(|outcome| !matches!(outcome, AcquireOutcome::Acquired))
            .all(|outcome| matches!(outcome, AcquireOutcome::HeldByOther(_))),
        "losers must observe the winner's claim: {outcomes:?}"
    );
    let claim = read_claim(temp.path(), &document.key());
    assert_eq!(claim.machine_id, format!("machine-{}", winners[0]));
}

#[test]
fn takeover_requires_both_an_expired_lease_and_a_stale_heartbeat() {
    let temp = TempDir::new().unwrap();
    let start = 1_000_000;
    let clock = MockClock::at(start);
    let owner =
        ClaimStore::with_clock(temp.path(), identity("aaa", "owner"), clock.clone()).unwrap();
    let rival =
        ClaimStore::with_clock(temp.path(), identity("bbb", "rival"), clock.clone()).unwrap();
    let document = doc("contested.pdf");
    let key = document.key();
    assert!(matches!(owner.acquire(&document), AcquireOutcome::Acquired));

    clock.set(start + CLAIM_LEASE_SECONDS - 1);
    assert!(
        matches!(rival.acquire(&document), AcquireOutcome::HeldByOther(_)),
        "a live lease must block the rival"
    );

    // Lease long expired but the heartbeat is recent — the picture a machine
    // with a skewed clock (or a renewal still stuck in the sync client's
    // upload queue) would present. Takeover must stay blocked.
    let mut skewed = read_claim(temp.path(), &key);
    skewed.lease_expires_at = start - 10;
    skewed.heartbeat_at = start + CLAIM_LEASE_SECONDS - 60;
    fs::write(
        claim_file(temp.path(), &key),
        serde_json::to_vec_pretty(&skewed).unwrap(),
    )
    .unwrap();
    clock.set(start + CLAIM_LEASE_SECONDS + 1);
    assert!(matches!(
        rival.acquire(&document),
        AcquireOutcome::HeldByOther(_)
    ));

    clock.set(skewed.heartbeat_at + CLAIM_LEASE_SECONDS + 1);
    assert!(matches!(rival.acquire(&document), AcquireOutcome::Acquired));
    let claim = read_claim(temp.path(), &key);
    assert_eq!(claim.machine_id, "bbb");
    assert!(!owner.verify(&key), "the original owner must see the loss");
}

#[test]
fn two_machines_racing_a_stale_takeover_admit_exactly_one_new_owner() {
    let temp = TempDir::new().unwrap();
    let start = 1_000_000;
    let clock = MockClock::at(start);
    let dead = ClaimStore::with_clock(temp.path(), identity("ccc", "dead"), clock.clone()).unwrap();
    let document = doc("orphaned.pdf");
    assert!(matches!(dead.acquire(&document), AcquireOutcome::Acquired));
    clock.set(start + 2 * CLAIM_LEASE_SECONDS);

    let contenders = [
        ClaimStore::with_clock(temp.path(), identity("machine-a", "a"), clock.clone()).unwrap(),
        ClaimStore::with_clock(temp.path(), identity("machine-b", "b"), clock.clone()).unwrap(),
    ];
    let barrier = Barrier::new(2);
    let outcomes: Vec<AcquireOutcome> = thread::scope(|scope| {
        let handles: Vec<_> = contenders
            .iter()
            .map(|store| {
                scope.spawn(|| {
                    barrier.wait();
                    store.acquire(&document)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });
    let acquired = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, AcquireOutcome::Acquired))
        .count();
    assert_eq!(acquired, 1, "outcomes: {outcomes:?}");
    let claim = read_claim(temp.path(), &document.key());
    assert_ne!(claim.machine_id, "ccc");
}

#[test]
fn a_done_tombstone_prevents_any_further_claims() {
    let temp = TempDir::new().unwrap();
    let clock = MockClock::at(1_000_000);
    let owner =
        ClaimStore::with_clock(temp.path(), identity("aaa", "owner"), clock.clone()).unwrap();
    let rival =
        ClaimStore::with_clock(temp.path(), identity("bbb", "rival"), clock.clone()).unwrap();
    let document = doc("finished.pdf");
    let key = document.key();
    assert!(matches!(owner.acquire(&document), AcquireOutcome::Acquired));
    owner
        .mark_done(&key, DoneOutcome::Renamed, Some("2024 Contract.pdf"))
        .unwrap();

    // Even after every lease horizon has passed, done is forever.
    clock.advance(10 * CLAIM_LEASE_SECONDS);
    match rival.acquire(&document) {
        AcquireOutcome::Done(claim) => {
            assert_eq!(claim.outcome, Some(DoneOutcome::Renamed));
            assert_eq!(claim.result_filename.as_deref(), Some("2024 Contract.pdf"));
            assert_eq!(claim.machine_id, "aaa");
        }
        other => panic!("expected Done, got {other:?}"),
    }
    assert!(matches!(owner.acquire(&document), AcquireOutcome::Done(_)));
}

#[test]
fn verify_fails_once_the_claim_file_is_overwritten_by_another_machine() {
    let temp = TempDir::new().unwrap();
    let clock = MockClock::at(1_000_000);
    let owner =
        ClaimStore::with_clock(temp.path(), identity("aaa", "owner"), clock.clone()).unwrap();
    let document = doc("stolen.pdf");
    let key = document.key();
    assert!(matches!(owner.acquire(&document), AcquireOutcome::Acquired));
    assert!(owner.verify(&key));

    // A sync conflict resolution replaces our claim with the other machine's.
    let foreign = foreign_claim(&key, &document, "bbb", 1_000_000);
    fs::write(
        claim_file(temp.path(), &key),
        serde_json::to_vec_pretty(&foreign).unwrap(),
    )
    .unwrap();
    assert!(!owner.verify(&key));
    assert!(owner.renew(&key).is_err());
    assert!(
        owner.release(&key).is_err(),
        "release must not delete a foreign claim"
    );
    assert!(claim_file(temp.path(), &key).exists());
}

#[test]
fn renew_self_gates_until_less_than_the_threshold_remains() {
    let temp = TempDir::new().unwrap();
    let start = 1_000_000;
    let clock = MockClock::at(start);
    let owner =
        ClaimStore::with_clock(temp.path(), identity("aaa", "owner"), clock.clone()).unwrap();
    let document = doc("busy.pdf");
    let key = document.key();
    assert!(matches!(owner.acquire(&document), AcquireOutcome::Acquired));

    clock.set(start + 100);
    owner.renew(&key).unwrap();
    let untouched = read_claim(temp.path(), &key);
    assert_eq!(untouched.lease_expires_at, start + CLAIM_LEASE_SECONDS);
    assert_eq!(untouched.heartbeat_at, start);

    clock.set(start + CLAIM_LEASE_SECONDS - CLAIM_RENEW_THRESHOLD_SECONDS + 1);
    owner.renew(&key).unwrap();
    let renewed = read_claim(temp.path(), &key);
    assert_eq!(
        renewed.lease_expires_at,
        start + CLAIM_LEASE_SECONDS - CLAIM_RENEW_THRESHOLD_SECONDS + 1 + CLAIM_LEASE_SECONDS
    );
    assert_eq!(
        renewed.heartbeat_at,
        start + CLAIM_LEASE_SECONDS - CLAIM_RENEW_THRESHOLD_SECONDS + 1
    );
}

#[test]
fn release_deletes_only_a_claim_this_machine_owns_and_is_idempotent() {
    let temp = TempDir::new().unwrap();
    let clock = MockClock::at(1_000_000);
    let owner =
        ClaimStore::with_clock(temp.path(), identity("aaa", "owner"), clock.clone()).unwrap();
    let document = doc("released.pdf");
    let key = document.key();
    assert!(matches!(owner.acquire(&document), AcquireOutcome::Acquired));
    owner.release(&key).unwrap();
    assert!(!claim_file(temp.path(), &key).exists());
    owner.release(&key).unwrap();
    assert!(matches!(owner.acquire(&document), AcquireOutcome::Acquired));
}

#[test]
fn origin_markers_are_create_once_and_the_first_writer_wins() {
    let temp = TempDir::new().unwrap();
    let clock = MockClock::at(1_000_000);
    let first =
        ClaimStore::with_clock(temp.path(), identity("aaa", "first"), clock.clone()).unwrap();
    let second =
        ClaimStore::with_clock(temp.path(), identity("bbb", "second"), clock.clone()).unwrap();
    let document = doc("uploaded.pdf");
    let key = document.key();
    first.write_origin(&document).unwrap();
    second.write_origin(&document).unwrap();
    let origin = second.read_origin(&key).unwrap();
    assert_eq!(origin.machine_id, "aaa");
    assert_eq!(origin.relative_path, "uploaded.pdf");
}

#[test]
fn malformed_and_future_version_claim_files_read_as_unreadable_never_panic() {
    let temp = TempDir::new().unwrap();
    let clock = MockClock::at(1_000_000);
    let store =
        ClaimStore::with_clock(temp.path(), identity("aaa", "here"), clock.clone()).unwrap();
    let document = doc("garbled.pdf");
    let key = document.key();

    fs::write(claim_file(temp.path(), &key), b"{ not json").unwrap();
    assert!(store.read(&key).is_none());
    assert!(!store.verify(&key));
    match store.acquire(&document) {
        AcquireOutcome::Failed(error) => {
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData)
        }
        other => panic!("expected Failed, got {other:?}"),
    }

    let mut future = foreign_claim(&key, &document, "aaa", 1_000_000);
    future.version = 2;
    fs::write(
        claim_file(temp.path(), &key),
        serde_json::to_vec_pretty(&future).unwrap(),
    )
    .unwrap();
    assert!(store.read(&key).is_none());
    assert!(
        !store.verify(&key),
        "a future-version claim must not count as ours"
    );
    assert!(matches!(
        store.acquire(&document),
        AcquireOutcome::Failed(_)
    ));
}

#[test]
fn prune_removes_expired_done_claims_dead_leases_and_conflict_copies() {
    let temp = TempDir::new().unwrap();
    let start = real_now();
    let clock = MockClock::at(start);
    let store =
        ClaimStore::with_clock(temp.path(), identity("aaa", "here"), clock.clone()).unwrap();

    let old_done = doc("old-done.pdf");
    assert!(matches!(store.acquire(&old_done), AcquireOutcome::Acquired));
    store
        .mark_done(&old_done.key(), DoneOutcome::Renamed, None)
        .unwrap();
    let abandoned = doc("abandoned.pdf");
    assert!(matches!(
        store.acquire(&abandoned),
        AcquireOutcome::Acquired
    ));
    let claims_dir = temp.path().join(".intern").join("claims");
    let conflict_copy = claims_dir.join(format!("{} (1).json", old_done.key()));
    fs::copy(claim_file(temp.path(), &old_done.key()), &conflict_copy).unwrap();
    let malformed = claims_dir.join("not-a-real-claim.json");
    fs::write(&malformed, b"sync conflict garbage").unwrap();

    clock.set(start + DONE_RETENTION_SECONDS + 1);
    let fresh_done = doc("fresh-done.pdf");
    assert!(matches!(
        store.acquire(&fresh_done),
        AcquireOutcome::Acquired
    ));
    store
        .mark_done(&fresh_done.key(), DoneOutcome::KeptOriginal, None)
        .unwrap();
    let fresh_claimed = doc("fresh-claimed.pdf");
    assert!(matches!(
        store.acquire(&fresh_claimed),
        AcquireOutcome::Acquired
    ));

    store.prune();
    assert!(!claim_file(temp.path(), &old_done.key()).exists());
    assert!(
        !claim_file(temp.path(), &abandoned.key()).exists(),
        "a claimed lease silent for the whole retention window belongs to a dead machine"
    );
    assert!(!conflict_copy.exists());
    assert!(!malformed.exists());
    assert!(claim_file(temp.path(), &fresh_done.key()).exists());
    assert!(claim_file(temp.path(), &fresh_claimed.key()).exists());
    assert!(temp.path().join(".intern").join("README.txt").exists());
}

#[test]
fn prune_gives_fresh_malformed_files_a_day_of_grace() {
    let temp = TempDir::new().unwrap();
    let clock = MockClock::at_real_now();
    let store =
        ClaimStore::with_clock(temp.path(), identity("aaa", "here"), clock.clone()).unwrap();
    let malformed = temp
        .path()
        .join(".intern")
        .join("claims")
        .join("half-synced.json");
    fs::write(&malformed, b"{ \"version\":").unwrap();

    store.prune();
    assert!(
        malformed.exists(),
        "a just-written file may be a sync transfer in progress"
    );

    clock.advance(2 * 24 * 3600);
    store.prune();
    assert!(!malformed.exists());
}

#[test]
fn presence_is_listed_across_machines_and_refresh_is_self_gated() {
    let temp = TempDir::new().unwrap();
    let start = 1_000_000;
    let clock = MockClock::at(start);
    let here =
        ClaimStore::with_clock(temp.path(), identity("aaa", "front-desk"), clock.clone()).unwrap();
    let there =
        ClaimStore::with_clock(temp.path(), identity("bbb", "back-office"), clock.clone()).unwrap();
    here.touch_presence().unwrap();
    there.touch_presence().unwrap();
    fs::write(
        temp.path()
            .join(".intern")
            .join("machines")
            .join("broken.json"),
        b"not json",
    )
    .unwrap();

    let machines: Vec<MachinePresence> = here.list_machines();
    assert_eq!(
        machines.len(),
        2,
        "the malformed presence file must be ignored"
    );
    assert_eq!(machines[0].machine_id, "aaa");
    assert_eq!(machines[0].machine_name, "front-desk");
    assert_eq!(machines[1].machine_id, "bbb");
    assert_eq!(machines[1].last_seen_at, start);

    clock.set(start + PRESENCE_REFRESH_SECONDS - 1);
    here.touch_presence().unwrap();
    assert_eq!(here.list_machines()[0].last_seen_at, start);

    clock.set(start + PRESENCE_REFRESH_SECONDS);
    here.touch_presence().unwrap();
    assert_eq!(
        here.list_machines()[0].last_seen_at,
        start + PRESENCE_REFRESH_SECONDS
    );
}

#[test]
fn a_recreated_store_reacquires_its_own_surviving_claim() {
    // A crash and restart must not lock the machine out of its own lease.
    let temp = TempDir::new().unwrap();
    let clock = MockClock::at(1_000_000);
    let document = doc("restart.pdf");
    {
        let store =
            ClaimStore::with_clock(temp.path(), identity("aaa", "here"), clock.clone()).unwrap();
        assert!(matches!(store.acquire(&document), AcquireOutcome::Acquired));
    }
    let reopened =
        ClaimStore::with_clock(temp.path(), identity("aaa", "here"), clock.clone()).unwrap();
    assert!(matches!(
        reopened.acquire(&document),
        AcquireOutcome::Acquired
    ));
    assert!(reopened.verify(&document.key()));
}

#[test]
fn facts_helper_matches_the_documented_key_recipe() {
    // Guards the tests' own facts_for helper against drifting from the store.
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("a.pdf"), b"content").unwrap();
    let facts = facts_for(temp.path(), "a.pdf");
    assert_eq!(
        facts.key(),
        document_key("a.pdf", facts.size, facts.modified_secs)
    );
}
