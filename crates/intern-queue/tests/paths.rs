use std::fs;

use intern_queue::paths::{
    canonical_file, canonical_folder, canonical_model_file, collect_supported_files, parse_item_id,
};
use tempfile::tempdir;

#[test]
fn canonical_file_accepts_only_existing_supported_regular_files() {
    let temp = tempdir().unwrap();
    let pdf = temp.path().join("Contract.PDF");
    let unsupported = temp.path().join("sheet.xlsx");
    fs::write(&pdf, b"pdf").unwrap();
    fs::write(&unsupported, b"sheet").unwrap();

    assert_eq!(canonical_file(&pdf).unwrap(), pdf.canonicalize().unwrap());
    assert!(canonical_file(&unsupported).is_err());
    assert!(canonical_file(&temp.path().join("missing.pdf")).is_err());
}

#[test]
fn canonical_model_file_accepts_only_nonempty_regular_gguf_without_following_links() {
    let temp = tempdir().unwrap();
    let model = temp.path().join("model.GGUF");
    let wrong_extension = temp.path().join("model.bin");
    let empty = temp.path().join("empty.gguf");
    fs::write(&model, b"model").unwrap();
    fs::write(&wrong_extension, b"model").unwrap();
    fs::write(&empty, b"").unwrap();

    assert_eq!(
        canonical_model_file(&model).unwrap(),
        model.canonicalize().unwrap()
    );
    assert!(canonical_model_file(&wrong_extension).is_err());
    assert!(canonical_model_file(&empty).is_err());
    #[cfg(unix)]
    {
        let link = temp.path().join("linked.gguf");
        std::os::unix::fs::symlink(&model, &link).unwrap();
        assert!(canonical_model_file(&link).is_err());
    }
}

#[test]
fn folder_expansion_is_recursive_deterministic_and_skips_hidden_lock_zero_byte_and_links() {
    let temp = tempdir().unwrap();
    let nested = temp.path().join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(temp.path().join("b.txt"), b"b").unwrap();
    fs::write(nested.join("a.md"), b"a").unwrap();
    fs::write(temp.path().join(".hidden.pdf"), b"hidden").unwrap();
    fs::write(temp.path().join("~$lock.docx"), b"lock").unwrap();
    fs::write(temp.path().join("zero.pdf"), b"").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&nested, temp.path().join("linked")).unwrap();

    let root = canonical_folder(temp.path()).unwrap();
    let files = collect_supported_files(&root).unwrap();

    assert_eq!(
        files,
        vec![
            temp.path().join("b.txt").canonicalize().unwrap(),
            nested.join("a.md").canonicalize().unwrap()
        ]
    );
}

#[test]
fn item_ids_are_positive_canonical_decimal_strings() {
    assert_eq!(parse_item_id("42").unwrap(), 42);
    for value in ["", "0", "-1", " 1", "01", "+1", "1.0"] {
        assert!(parse_item_id(value).is_err(), "accepted {value:?}");
    }
}
