mod common;

use std::fs;

use intern_intake::MachineIdentity;
use tempfile::TempDir;

fn is_lower_hex_32(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[test]
fn the_machine_id_is_created_once_and_survives_reloads() {
    let temp = TempDir::new().unwrap();
    let first = MachineIdentity::load_or_create(temp.path(), "").unwrap();
    assert!(is_lower_hex_32(&first.id), "id was {:?}", first.id);
    let on_disk = fs::read_to_string(temp.path().join("machine-id")).unwrap();
    assert_eq!(on_disk.trim(), first.id);

    let second = MachineIdentity::load_or_create(temp.path(), "").unwrap();
    assert_eq!(second.id, first.id);
}

#[test]
fn two_data_dirs_never_share_a_machine_id() {
    let left = TempDir::new().unwrap();
    let right = TempDir::new().unwrap();
    let first = MachineIdentity::load_or_create(left.path(), "").unwrap();
    let second = MachineIdentity::load_or_create(right.path(), "").unwrap();
    assert_ne!(first.id, second.id);
}

#[test]
fn a_corrupt_machine_id_file_is_regenerated_in_place() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("machine-id"), "not hex at all\n").unwrap();
    let regenerated = MachineIdentity::load_or_create(temp.path(), "").unwrap();
    assert!(is_lower_hex_32(&regenerated.id));
    let reloaded = MachineIdentity::load_or_create(temp.path(), "").unwrap();
    assert_eq!(reloaded.id, regenerated.id);
}

#[test]
fn the_label_overrides_the_hostname_and_a_blank_label_falls_back() {
    let temp = TempDir::new().unwrap();
    let labeled = MachineIdentity::load_or_create(temp.path(), "  Front Desk  ").unwrap();
    assert_eq!(labeled.name, "Front Desk");

    let unlabeled = MachineIdentity::load_or_create(temp.path(), "").unwrap();
    assert!(
        !unlabeled.name.trim().is_empty(),
        "hostname fallback must produce a name"
    );
    assert!(
        !unlabeled.user.trim().is_empty(),
        "user fallback must produce a name"
    );
    assert_eq!(
        unlabeled.id, labeled.id,
        "the label never changes the durable id"
    );
}
