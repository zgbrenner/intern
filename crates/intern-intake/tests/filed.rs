mod common;

use std::fs;

use common::{MockClock, identity, real_now};
use intern_intake::{
    ClaimStore, FILED_RETENTION_SECONDS, FiledIndex, FiledMarker, MachineIdentity,
};
use tempfile::TempDir;

const HASH: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
const FILED_NAME: &str = "2026-03-02 Master Services Agreement between Acme and Globex.pdf";

fn index(root: &TempDir, identity: MachineIdentity) -> FiledIndex {
    FiledIndex::new(root.path(), identity)
}

#[test]
fn a_filed_document_leaves_a_marker_that_any_machine_can_read_back() {
    let intake = TempDir::new().unwrap();
    let front_desk = index(&intake, identity("aaa", "Front desk"));
    let source = intake.path().join("scans").join("scan0012.pdf");

    let path = front_desk
        .record(HASH, &source, FILED_NAME, 1_757_000_000)
        .unwrap();

    assert_eq!(
        path,
        intake
            .path()
            .join(".intern")
            .join("filed")
            .join(format!("{HASH}.json"))
    );
    assert_eq!(front_desk.directory(), path.parent().unwrap());

    // Another machine, reading the same folder through sync.
    let laptop = index(&intake, identity("bbb", "Laptop"));
    let marker = laptop.lookup(HASH).expect("the marker is shared");
    assert_eq!(marker.version, 1);
    assert_eq!(marker.content_hash, HASH);
    assert_eq!(marker.filename, FILED_NAME);
    assert_eq!(marker.relative_path, "scans/scan0012.pdf");
    assert_eq!(marker.machine_id, "aaa");
    assert_eq!(marker.machine_name, "Front desk");
    assert_eq!(marker.user_name, "tester");
    assert_eq!(marker.filed_at, 1_757_000_000);

    // The wire shape: camelCase, and nothing about the document but its name.
    let json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let object = json.as_object().unwrap();
    for field in [
        "version",
        "contentHash",
        "filename",
        "relativePath",
        "machineId",
        "machineName",
        "userName",
        "filedAt",
    ] {
        assert!(object.contains_key(field), "missing {field}: {json}");
    }
    assert_eq!(object.len(), 8, "no undocumented fields: {json}");
    assert!(
        !fs::read_dir(front_desk.directory())
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with('.')),
        "no temp file is left behind"
    );
}

#[test]
fn refiling_the_same_content_replaces_the_marker_whoever_wrote_it() {
    let intake = TempDir::new().unwrap();
    let front_desk = index(&intake, identity("aaa", "Front desk"));
    let laptop = index(&intake, identity("bbb", "Laptop"));
    let first = front_desk
        .record(HASH, &intake.path().join("a.pdf"), "First name.pdf", 1)
        .unwrap();
    let second = laptop
        .record(HASH, &intake.path().join("b.pdf"), "Second name.pdf", 2)
        .unwrap();
    assert_eq!(first, second, "one content hash, one marker");
    let marker = front_desk.lookup(HASH).unwrap();
    assert_eq!(marker.filename, "Second name.pdf");
    assert_eq!(marker.machine_name, "Laptop");
    assert_eq!(marker.relative_path, "b.pdf");
}

#[test]
fn a_marker_is_retracted_only_by_the_machine_that_wrote_it() {
    let intake = TempDir::new().unwrap();
    let front_desk = index(&intake, identity("aaa", "Front desk"));
    let laptop = index(&intake, identity("bbb", "Laptop"));
    let path = front_desk
        .record(HASH, &intake.path().join("scan.pdf"), FILED_NAME, 1)
        .unwrap();

    assert!(
        !laptop.retract(HASH).unwrap(),
        "an undo on another machine is not an undo of this filing"
    );
    assert!(path.exists());
    assert!(front_desk.retract(HASH).unwrap());
    assert!(!path.exists());
    assert!(
        !front_desk.retract(HASH).unwrap(),
        "retracting twice is a no-op"
    );
    assert_eq!(laptop.lookup(HASH), None);
}

#[test]
fn documents_from_outside_the_intake_folder_and_bad_hashes_are_refused() {
    let intake = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    let front_desk = index(&intake, identity("aaa", "Front desk"));

    assert!(front_desk.covers(&intake.path().join("deep").join("scan.pdf")));
    assert!(!front_desk.covers(&elsewhere.path().join("scan.pdf")));
    assert!(
        !front_desk.covers(intake.path()),
        "the root itself is not a document"
    );

    let outside = front_desk
        .record(HASH, &elsewhere.path().join("scan.pdf"), FILED_NAME, 1)
        .unwrap_err();
    assert_eq!(outside.kind(), std::io::ErrorKind::InvalidInput);

    for bad in [
        "",
        "abc",
        "../../etc/passwd",
        &"A".repeat(64),
        "sha256:abcdef",
    ] {
        let error = front_desk
            .record(bad, &intake.path().join("scan.pdf"), FILED_NAME, 1)
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{bad:?}");
        assert_eq!(front_desk.lookup(bad), None);
        assert!(!front_desk.retract(bad).unwrap());
    }
    assert!(
        !intake.path().join(".intern").exists(),
        "a refused record creates nothing"
    );
}

#[test]
fn a_marker_under_the_wrong_name_or_a_future_version_reads_as_nothing() {
    let intake = TempDir::new().unwrap();
    let front_desk = index(&intake, identity("aaa", "Front desk"));
    let directory = front_desk.directory().to_path_buf();
    fs::create_dir_all(&directory).unwrap();
    let other_hash = "1".repeat(64);
    let marker = FiledMarker {
        version: 1,
        content_hash: other_hash.clone(),
        filename: FILED_NAME.to_string(),
        relative_path: "scan.pdf".to_string(),
        machine_id: "aaa".to_string(),
        machine_name: "Front desk".to_string(),
        user_name: "tester".to_string(),
        filed_at: 1,
    };
    // A sync conflict copy: valid JSON that names a different hash.
    fs::write(
        directory.join(format!("{HASH}.json")),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    assert_eq!(front_desk.lookup(HASH), None);
    assert_eq!(
        front_desk.lookup(&other_hash),
        None,
        "never read from another file"
    );

    let future = FiledMarker {
        version: 2,
        content_hash: other_hash.clone(),
        ..marker
    };
    fs::write(
        directory.join(format!("{other_hash}.json")),
        serde_json::to_vec(&future).unwrap(),
    )
    .unwrap();
    assert_eq!(front_desk.lookup(&other_hash), None);
    fs::write(
        directory.join(format!("{}.json", "2".repeat(64))),
        b"{not json",
    )
    .unwrap();
    assert_eq!(front_desk.lookup(&"2".repeat(64)), None);
}

#[test]
fn prune_keeps_markers_for_a_year_and_clears_conflict_copies_after_a_day() {
    let intake = TempDir::new().unwrap();
    // The conflict copy's age is its wall-clock mtime, so the clock starts
    // at the real time.
    let now = real_now();
    let clock = MockClock::at(now);
    let store = ClaimStore::with_clock(intake.path(), identity("aaa", "Front desk"), clock.clone())
        .unwrap();
    assert!(
        intake.path().join(".intern").join("filed").is_dir(),
        "the claim store lays out the filed directory with the others"
    );
    let front_desk = index(&intake, identity("aaa", "Front desk"));
    let old_hash = "a".repeat(64);
    let recent_hash = "b".repeat(64);
    let old = front_desk
        .record(
            &old_hash,
            &intake.path().join("old.pdf"),
            "Old.pdf",
            now - FILED_RETENTION_SECONDS,
        )
        .unwrap();
    let recent = front_desk
        .record(
            &recent_hash,
            &intake.path().join("recent.pdf"),
            "Recent.pdf",
            now - FILED_RETENTION_SECONDS + 7 * 24 * 3600,
        )
        .unwrap();
    // A conflict copy: this machine's own marker under the sync client's name.
    let conflict = front_desk
        .directory()
        .join(format!("{recent_hash}-FRONT-DESK.json"));
    fs::copy(&recent, &conflict).unwrap();

    store.prune();
    assert!(!old.exists(), "a year-old marker is gone");
    assert!(recent.exists(), "a marker inside the year stays");
    assert!(
        conflict.exists(),
        "a fresh conflict copy gets a day of grace"
    );

    clock.advance(24 * 3600 + 1);
    store.prune();
    assert!(
        !conflict.exists(),
        "the conflict copy is cleared after a day"
    );
    assert!(recent.exists());
}
