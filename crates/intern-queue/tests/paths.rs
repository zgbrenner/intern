use std::fs;

use std::path::Path;

use intern_queue::paths::{
    canonical_file, canonical_folder, canonical_model_file, collect_supported_files, display_path,
    is_cloud_reparse_tag, parse_item_id,
};
use tempfile::tempdir;

#[test]
fn canonical_file_accepts_only_existing_supported_regular_files() {
    let temp = tempdir().unwrap();
    let pdf = temp.path().join("Contract.PDF");
    let sheet = temp.path().join("sheet.xlsx");
    let email = temp.path().join("message.eml");
    let unsupported = temp.path().join("deck.pptx");
    fs::write(&pdf, b"pdf").unwrap();
    fs::write(&sheet, b"sheet").unwrap();
    fs::write(&email, b"email").unwrap();
    fs::write(&unsupported, b"deck").unwrap();

    assert_eq!(canonical_file(&pdf).unwrap(), pdf.canonicalize().unwrap());
    assert_eq!(
        canonical_file(&sheet).unwrap(),
        sheet.canonicalize().unwrap()
    );
    assert_eq!(
        canonical_file(&email).unwrap(),
        email.canonicalize().unwrap()
    );
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
fn cloud_files_reparse_tags_are_recognized_while_link_and_junction_tags_are_not() {
    for cloud_tag in [0x9000_001A_u32, 0x9000_101A, 0x9000_F01A] {
        assert!(is_cloud_reparse_tag(cloud_tag), "rejected {cloud_tag:#X}");
    }
    for other_tag in [0xA000_000C_u32, 0xA000_0003, 0] {
        assert!(!is_cloud_reparse_tag(other_tag), "accepted {other_tag:#X}");
    }
}

#[cfg(windows)]
#[test]
fn windows_junctions_stay_rejected_despite_the_cloud_placeholder_exemption() {
    use std::process::Command;

    let temp = tempdir().unwrap();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("doc.pdf"), b"pdf").unwrap();
    let junction = temp.path().join("junction");
    let created = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&target)
        .status()
        .unwrap();
    assert!(created.success());

    assert!(canonical_folder(&junction).is_err());
    let files = collect_supported_files(temp.path()).unwrap();
    assert_eq!(files, vec![target.join("doc.pdf").canonicalize().unwrap()]);
}

#[test]
fn item_ids_are_positive_canonical_decimal_strings() {
    assert_eq!(parse_item_id("42").unwrap(), 42);
    for value in ["", "0", "-1", " 1", "01", "+1", "1.0"] {
        assert!(parse_item_id(value).is_err(), "accepted {value:?}");
    }
}

/// `canonicalize` on Windows answers `\\?\C:\...`; a person reads `C:\...`.
/// The prefix is dropped only where the plain spelling is safe to use.
#[test]
fn display_paths_drop_the_verbatim_prefix_only_when_the_plain_form_is_safe() {
    assert_eq!(
        display_path(Path::new(r"\\?\C:\Users\pat\OneDrive - Contoso\Scans")),
        r"C:\Users\pat\OneDrive - Contoso\Scans"
    );
    assert_eq!(
        display_path(Path::new(r"\\?\UNC\fileserver\legal\intake")),
        r"\\fileserver\legal\intake"
    );
    assert_eq!(
        display_path(Path::new(r"C:\Users\pat\Scans")),
        r"C:\Users\pat\Scans",
        "a plain path is left alone"
    );
    assert_eq!(
        display_path(Path::new("/home/pat/scans")),
        "/home/pat/scans"
    );
    // A component ending in a space or a dot only survives under the prefix.
    assert_eq!(
        display_path(Path::new(r"\\?\C:\Users\pat\Scans.")),
        r"\\?\C:\Users\pat\Scans."
    );
    let deep = format!(r"\\?\C:\{}\file.pdf", "folder\\".repeat(40));
    assert_eq!(
        display_path(Path::new(&deep)),
        deep,
        "a path too long for the plain form keeps the prefix that makes it work"
    );
}
