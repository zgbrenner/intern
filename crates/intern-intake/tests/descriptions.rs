mod common;

use std::{fs, path::PathBuf};

use common::identity;
use intern_intake::{
    CloudProviderKind, CloudRoot, DescriptionLedger, DescriptionRecord, FiledDocument, record_key,
};
use tempfile::TempDir;

fn filed(path: PathBuf) -> FiledDocument {
    FiledDocument {
        path,
        original_filename: "scan0012.pdf".to_string(),
        description: "  Statement of work between Ridgeline Cartography LLC and Vistage Worldwide, Inc. for the 2026 mapping engagement.  ".to_string(),
        document_date: Some("2026-04-01".to_string()),
        document_type: Some("Statement of Work".to_string()),
        parties: vec![
            "Ridgeline Cartography LLC".to_string(),
            "Vistage Worldwide, Inc.".to_string(),
        ],
        confidence: Some(0.93),
        filed_at: 1_757_000_000,
    }
}

const FILED_NAME: &str =
    "2026-04-01 Statement of Work between Ridgeline Cartography LLC and Vistage Worldwide, Inc.pdf";

#[test]
fn a_filed_document_gets_one_record_named_by_its_relative_path() {
    let destination = TempDir::new().unwrap();
    let ledger = DescriptionLedger::new(
        destination.path(),
        identity("here-machine", "Front desk"),
        Vec::new(),
    );
    let document_path = destination
        .path()
        .join("Contracts")
        .join("2026")
        .join(FILED_NAME);

    let record_path = ledger.record(&filed(document_path.clone())).unwrap();

    let expected_key = record_key(&format!("Contracts/2026/{FILED_NAME}"));
    assert_eq!(
        record_path,
        destination
            .path()
            .join(".intern")
            .join("descriptions")
            .join(format!("{expected_key}.json"))
    );
    assert_eq!(
        ledger.record_path(&document_path).as_deref(),
        Some(record_path.as_path())
    );
    assert!(
        destination
            .path()
            .join(".intern")
            .join("descriptions")
            .join("README.txt")
            .exists(),
        "the folder explains itself to whoever finds it"
    );

    let record: DescriptionRecord =
        serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(record.version, 1);
    assert_eq!(record.key, expected_key);
    assert_eq!(record.filename, FILED_NAME);
    assert_eq!(record.path, format!("Contracts/2026/{FILED_NAME}"));
    assert_eq!(record.library_path, None);
    assert_eq!(record.library, None);
    assert_eq!(record.provider, None);
    assert_eq!(
        record.description,
        "Statement of work between Ridgeline Cartography LLC and Vistage Worldwide, Inc. for the 2026 mapping engagement.",
        "the sentence is stored trimmed"
    );
    assert_eq!(record.document_date.as_deref(), Some("2026-04-01"));
    assert_eq!(record.document_type.as_deref(), Some("Statement of Work"));
    assert_eq!(record.parties.len(), 2);
    assert_eq!(record.confidence, Some(0.93));
    assert_eq!(record.original_filename, "scan0012.pdf");
    assert_eq!(record.filed_at, 1_757_000_000);
    assert_eq!(record.machine_id, "here-machine");
    assert_eq!(record.machine_name, "Front desk");
    assert_eq!(record.user_name, "tester");
    assert_eq!(ledger.read(&document_path), Some(record));
}

/// The wire shape a Power Automate flow parses. Field names are the contract;
/// a renamed field silently breaks every flow built on the recipe.
#[test]
fn the_record_is_camel_case_json_with_the_documented_fields() {
    let destination = TempDir::new().unwrap();
    let ledger = DescriptionLedger::new(destination.path(), identity("m", "n"), Vec::new());
    let record_path = ledger
        .record(&filed(destination.path().join(FILED_NAME)))
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&fs::read(record_path).unwrap()).unwrap();
    let object = json.as_object().unwrap();
    for field in [
        "version",
        "key",
        "filename",
        "path",
        "description",
        "documentDate",
        "documentType",
        "parties",
        "confidence",
        "originalFilename",
        "filedAt",
        "machineId",
        "machineName",
        "userName",
    ] {
        assert!(object.contains_key(field), "missing {field}: {json}");
    }
    assert!(
        !object.contains_key("libraryPath"),
        "absent rather than null outside a sync root"
    );
    assert_eq!(json["path"], FILED_NAME);
}

#[test]
fn the_record_names_the_library_and_the_path_inside_it_when_the_destination_is_synced() {
    let home = TempDir::new().unwrap();
    let library = home.path().join("Contoso").join("Legal - Documents");
    let destination = library.join("Contracts");
    let roots = vec![
        CloudRoot {
            kind: CloudProviderKind::OneDriveBusiness,
            display_name: "OneDrive – Contoso".to_string(),
            root: home.path().join("OneDrive - Contoso"),
        },
        CloudRoot {
            kind: CloudProviderKind::SharePoint,
            display_name: "Contoso".to_string(),
            root: library.clone(),
        },
    ];
    let ledger = DescriptionLedger::new(&destination, identity("m", "n"), roots);
    let document_path = destination.join("2026").join(FILED_NAME);
    let record_path = ledger.record(&filed(document_path.clone())).unwrap();
    assert!(record_path.starts_with(destination.join(".intern").join("descriptions")));

    let record = ledger.read(&document_path).unwrap();
    assert_eq!(record.path, format!("2026/{FILED_NAME}"));
    assert_eq!(
        record.library_path.as_deref(),
        Some(format!("Contracts/2026/{FILED_NAME}").as_str())
    );
    assert_eq!(record.library.as_deref(), Some("Contoso"));
    assert_eq!(record.provider.as_deref(), Some("sharepoint"));
}

#[test]
fn refiling_the_same_path_replaces_the_record_and_an_undo_retracts_it() {
    let destination = TempDir::new().unwrap();
    let ledger = DescriptionLedger::new(destination.path(), identity("m", "n"), Vec::new());
    let document_path = destination.path().join(FILED_NAME);
    let first = ledger.record(&filed(document_path.clone())).unwrap();
    let mut revised = filed(document_path.clone());
    revised.description = "A revised sentence about the same document.".to_string();
    let second = ledger.record(&revised).unwrap();
    assert_eq!(first, second, "one document, one record");
    assert_eq!(
        ledger.read(&document_path).unwrap().description,
        "A revised sentence about the same document."
    );
    let records = fs::read_dir(ledger.directory())
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|value| value == "json")
        })
        .count();
    assert_eq!(records, 1);

    assert!(ledger.retract(&document_path).unwrap());
    assert!(!first.exists());
    assert!(
        !ledger.retract(&document_path).unwrap(),
        "retracting twice is a no-op"
    );
    assert_eq!(ledger.read(&document_path), None);
    assert!(
        fs::read_dir(ledger.directory())
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().starts_with('.')),
        "no temp file is left behind"
    );
}

/// The records live inside the destination library, so the sync client
/// replicates them and can leave a conflict copy of one behind. An undo that
/// removes only the record's own name leaves the copy in the library, where
/// the flow that fills the column reads it exactly like a record - and writes
/// the description of a document that is no longer filed.
#[test]
fn an_undo_retracts_the_copies_of_a_record_the_sync_client_left_too() {
    let destination = TempDir::new().unwrap();
    let ledger =
        DescriptionLedger::new(destination.path(), identity("m", "Front desk"), Vec::new());
    let document_path = destination.path().join(FILED_NAME);
    let record = ledger.record(&filed(document_path.clone())).unwrap();
    let key = record.file_stem().unwrap().to_string_lossy().into_owned();
    let conflicted = record.with_file_name(format!("{key}-DESKTOP-A1B2C3.json"));
    let numbered = record.with_file_name(format!("{key} (2).json"));
    fs::copy(&record, &conflicted).unwrap();
    fs::copy(&record, &numbered).unwrap();

    assert!(ledger.retract(&document_path).unwrap());

    assert!(!record.exists());
    assert!(!conflicted.exists(), "the conflict copy is still a record");
    assert!(!numbered.exists(), "so is the numbered copy");
    // Nothing else in the folder is touched: another document's record is not
    // this undo's business, and neither is the README.
    assert!(
        destination
            .path()
            .join(".intern")
            .join("descriptions")
            .join("README.txt")
            .exists()
    );
}

/// Windows hands the same folder out under two spellings - `C:\Users\...` from
/// a settings dialog and `\\?\C:\Users\...` from `canonicalize` - and a record
/// written under one has to be found and retracted under the other.
/// The same folder again, under the 8.3 short name Windows still hands out on
/// a volume where short-name generation is on. A destination reached one way
/// and a document reached the other must be one document, or an undo leaves
/// the record behind and the SharePoint column describes a file that is no
/// longer filed.
#[cfg(windows)]
#[test]
fn a_record_is_found_through_the_short_name_spelling_of_the_destination() {
    let parent = TempDir::new().unwrap();
    let long = parent.path().join("destination-with-a-very-long-name");
    fs::create_dir_all(&long).unwrap();
    let Some(short) = short_path(&long) else {
        // Short-name generation is off for this volume, so there is no second
        // spelling to disagree about and nothing to prove here.
        return;
    };
    assert_ne!(
        short, long,
        "the short spelling must differ from the long one"
    );

    let ledger = DescriptionLedger::new(&short, identity("m", "Front desk"), Vec::new());
    let record = ledger.record(&filed(long.join(FILED_NAME))).unwrap();
    assert!(record.exists());

    let under_long = long.join(FILED_NAME);
    assert_eq!(ledger.record_path(&under_long).as_ref(), Some(&record));
    assert!(ledger.read(&under_long).is_some());
    assert!(ledger.retract(&under_long).unwrap());
    assert!(!record.exists());
}

/// The 8.3 spelling of an existing directory, or `None` where the volume does
/// not make them.
#[cfg(windows)]
fn short_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    unsafe extern "system" {
        fn GetShortPathNameW(long: *const u16, short: *mut u16, len: u32) -> u32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut buffer = vec![0_u16; 1024];
    // SAFETY: both pointers are to buffers this call owns for its duration,
    // and the length is the capacity of the output buffer in code units.
    let written = unsafe { GetShortPathNameW(wide.as_ptr(), buffer.as_mut_ptr(), 1024) };
    if written == 0 || written as usize >= buffer.len() {
        return None;
    }
    let short = std::ffi::OsString::from_wide(&buffer[..written as usize]);
    let short = std::path::PathBuf::from(short);
    (short != path).then_some(short)
}

#[cfg(windows)]
#[test]
fn a_record_is_found_through_the_verbatim_spelling_of_the_destination() {
    let destination = TempDir::new().unwrap();
    let ledger =
        DescriptionLedger::new(destination.path(), identity("m", "Front desk"), Vec::new());
    let document_path = destination.path().join(FILED_NAME);
    let record = ledger.record(&filed(document_path)).unwrap();

    let verbatim = fs::canonicalize(destination.path())
        .unwrap()
        .join(FILED_NAME);
    assert!(
        verbatim.to_string_lossy().starts_with(r"\\?\"),
        "canonicalize is expected to produce the verbatim spelling"
    );
    assert_eq!(ledger.record_path(&verbatim).as_ref(), Some(&record));
    assert!(ledger.read(&verbatim).is_some());
    assert!(ledger.retract(&verbatim).unwrap());
    assert!(!record.exists());
}

#[test]
fn a_document_outside_the_destination_is_refused_not_recorded_somewhere_odd() {
    let destination = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    let ledger = DescriptionLedger::new(destination.path(), identity("m", "n"), Vec::new());
    let error = ledger
        .record(&filed(elsewhere.path().join(FILED_NAME)))
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!destination.path().join(".intern").exists());
    assert_eq!(ledger.record_path(&elsewhere.path().join(FILED_NAME)), None);
    assert!(!ledger.retract(&elsewhere.path().join(FILED_NAME)).unwrap());
}

#[test]
fn record_keys_agree_across_casing_and_separators() {
    assert_eq!(
        record_key("Contracts\\2026\\Agreement.PDF"),
        record_key("contracts/2026/agreement.pdf")
    );
    assert_ne!(record_key("a.pdf"), record_key("b.pdf"));
    assert_eq!(record_key("a.pdf").len(), 32);
    assert!(
        record_key("a.pdf")
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
}
