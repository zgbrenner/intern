use std::fs;

use intern_engine::HostedProvider;
use intern_queue::settings::{AppSettings, DestinationLayout, ModelSource, SettingsStore};
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
            destination_layout: DestinationLayout::Flat,
            start_minimized: true,
            automatic_rename: true,
            intake_folder: String::new(),
            intake_enabled: false,
            intake_local_only: false,
            process_others_uploads: false,
            machine_label: String::new(),
            run_in_background: false,
            start_at_login: false,
            record_descriptions: false,
            model_source: ModelSource::Local,
            hosted_provider: HostedProvider::Anthropic,
            hosted_base_url: String::new(),
            hosted_model: String::new(),
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
        destination_layout: DestinationLayout::YearType,
        start_minimized: false,
        automatic_rename: true,
        intake_folder: "/somewhere/intake".into(),
        intake_enabled: true,
        intake_local_only: true,
        process_others_uploads: true,
        machine_label: "study desk".into(),
        run_in_background: true,
        start_at_login: true,
        record_descriptions: true,
        model_source: ModelSource::Hosted,
        hosted_provider: HostedProvider::OpenAiCompatible,
        hosted_base_url: "https://gateway.example.com/v1".into(),
        hosted_model: "filing-model".into(),
    };
    store.save(&settings).unwrap();

    assert_eq!(store.load().unwrap(), settings);
    assert!(!temp.path().join("settings.json.tmp").exists());
    let written = fs::read_to_string(&path).unwrap();
    for key in [
        "intakeFolder",
        "intakeEnabled",
        "intakeLocalOnly",
        "processOthersUploads",
        "machineLabel",
        "runInBackground",
        "startAtLogin",
        "recordDescriptions",
        "\"destinationLayout\": \"year_type\"",
        "\"modelSource\": \"hosted\"",
        "\"hostedProvider\": \"openai_compatible\"",
        "\"hostedBaseUrl\": \"https://gateway.example.com/v1\"",
        "\"hostedModel\": \"filing-model\"",
    ] {
        assert!(written.contains(key), "missing camelCase key {key}");
    }
}

#[test]
fn every_layout_round_trips_through_its_snake_case_name() {
    for layout in DestinationLayout::ALL {
        let json = serde_json::to_string(&layout).unwrap();
        assert_eq!(json, format!("\"{}\"", layout.as_str()));
        assert_eq!(
            serde_json::from_str::<DestinationLayout>(&json).unwrap(),
            layout
        );
    }
    assert_eq!(DestinationLayout::default(), DestinationLayout::Flat);
}

#[test]
fn a_settings_file_written_with_a_byte_order_mark_still_loads() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("settings.json");
    // What Windows PowerShell's `Set-Content -Encoding utf8` writes.
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(
        br#"{ "destination": "/somewhere/out", "startMinimized": true, "intakeEnabled": true }"#,
    );
    fs::write(&path, bytes).unwrap();

    let loaded = SettingsStore::new(&path).load().unwrap();

    assert_eq!(loaded.destination, "/somewhere/out");
    assert!(loaded.intake_enabled);
}

#[test]
fn one_value_that_is_not_understood_does_not_discard_the_rest_of_the_file() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("settings.json");
    fs::write(
        &path,
        br#"{ "destination": "/somewhere/out", "destinationLayout": "none", "intakeEnabled": true }"#,
    )
    .unwrap();

    let loaded = SettingsStore::new(&path).load_with_report().unwrap();

    assert_eq!(loaded.settings.destination, "/somewhere/out");
    assert!(loaded.settings.intake_enabled);
    assert_eq!(loaded.settings.destination_layout, DestinationLayout::Flat);
    // And the field that was dropped is named, so a person can be told which
    // line of their file to look at.
    assert_eq!(loaded.unreadable, ["destinationLayout"]);
}

#[test]
fn a_file_that_is_not_a_json_object_is_still_refused() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("settings.json");
    fs::write(&path, b"{ this is not json").unwrap();

    // Nothing can be salvaged a field at a time from a document that does not
    // parse, and the refusal is what stops the interface saving defaults over
    // whatever the file was meant to say.
    let error = SettingsStore::new(&path).load().unwrap_err();

    assert_eq!(error.code, "SETTINGS_INVALID");
}

#[test]
fn a_field_the_reader_requires_and_the_file_does_not_have_is_reported() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("settings.json");
    fs::write(
        &path,
        br#"{ "startMinimized": true, "automaticRename": true }"#,
    )
    .unwrap();

    let loaded = SettingsStore::new(&path).load_with_report().unwrap();

    // Filing into the source folder is a real configuration, so a destination
    // that fell out of the file must not be mistaken for one somebody chose.
    assert_eq!(loaded.settings.destination, "");
    assert!(loaded.settings.automatic_rename);
    assert_eq!(loaded.unreadable, ["destination"]);
}
