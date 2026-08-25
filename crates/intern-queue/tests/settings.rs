use std::fs;

use intern_queue::settings::{AppSettings, SettingsStore};
use tempfile::tempdir;

#[test]
fn settings_saved_before_the_intake_fields_existed_load_with_defaults() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("settings.json");
    fs::write(
        &path,
        br#"{ "destination": "/somewhere/out", "startMinimized": true, "automaticRename": true }"#,
    )
    .unwrap();

    let loaded = SettingsStore::new(&path).load().unwrap();

    assert_eq!(
        loaded,
        AppSettings {
            destination: "/somewhere/out".into(),
            start_minimized: true,
            automatic_rename: true,
            intake_folder: String::new(),
            intake_enabled: false,
            process_others_uploads: false,
            machine_label: String::new(),
            run_in_background: false,
            start_at_login: false,
        }
    );
}

#[test]
fn save_replaces_existing_content_atomically_and_round_trips_the_intake_fields() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("settings.json");
    let store = SettingsStore::new(&path);
    store.save(&AppSettings::default()).unwrap();

    let settings = AppSettings {
        destination: "/somewhere/out".into(),
        start_minimized: false,
        automatic_rename: true,
        intake_folder: "/somewhere/intake".into(),
        intake_enabled: true,
        process_others_uploads: true,
        machine_label: "study desk".into(),
        run_in_background: true,
        start_at_login: true,
    };
    store.save(&settings).unwrap();

    assert_eq!(store.load().unwrap(), settings);
    assert!(!temp.path().join("settings.json.tmp").exists());
    let written = fs::read_to_string(&path).unwrap();
    for key in [
        "intakeFolder",
        "intakeEnabled",
        "processOthersUploads",
        "machineLabel",
        "runInBackground",
        "startAtLogin",
    ] {
        assert!(written.contains(key), "missing camelCase key {key}");
    }
}
