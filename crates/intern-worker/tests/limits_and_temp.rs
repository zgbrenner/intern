use std::time::Duration;

use intern_worker::limits::{
    MAX_DECOMPRESSED_OFFICE_BYTES, MAX_EXTRACTION_DURATION, MAX_PAGE_COUNT,
    MAX_PAGE_MEGAPIXELS, MAX_RESIDENT_RENDERED_PAGES, MAX_SOURCE_BYTES, MAX_TEMP_BYTES,
    ResourceLimits,
};
use intern_worker::extract::load_oriented_image;
use intern_worker::temp::TempWorkspace;

#[test]
fn default_limits_enforce_the_worker_resource_contract() {
    let limits = ResourceLimits::default();

    assert_eq!(MAX_SOURCE_BYTES, 1_073_741_824);
    assert_eq!(MAX_PAGE_COUNT, 500);
    assert_eq!(MAX_DECOMPRESSED_OFFICE_BYTES, 1_073_741_824);
    assert_eq!(MAX_TEMP_BYTES, 2_147_483_648);
    assert_eq!(MAX_PAGE_MEGAPIXELS, 25);
    assert_eq!(MAX_EXTRACTION_DURATION, Duration::from_secs(30 * 60));
    assert_eq!(MAX_RESIDENT_RENDERED_PAGES, 1);
    assert_eq!(limits.max_source_bytes, MAX_SOURCE_BYTES);
    assert_eq!(limits.max_page_count, MAX_PAGE_COUNT);
    assert!(limits.validate_source_size(MAX_SOURCE_BYTES).is_ok());
    assert_eq!(
        limits.validate_source_size(MAX_SOURCE_BYTES + 1).unwrap_err().code(),
        "RESOURCE_LIMIT_EXCEEDED"
    );
    assert!(limits.validate_page_count(MAX_PAGE_COUNT).is_ok());
    assert_eq!(
        limits.validate_page_count(MAX_PAGE_COUNT + 1).unwrap_err().code(),
        "RESOURCE_LIMIT_EXCEEDED"
    );
    assert!(limits.validate_page_pixels(5_000, 5_000).is_ok());
    assert_eq!(
        limits.validate_page_pixels(5_001, 5_000).unwrap_err().code(),
        "RESOURCE_LIMIT_EXCEEDED"
    );
}

#[test]
fn temporary_workspace_is_deleted_on_drop() {
    let path = {
        let workspace = TempWorkspace::create("cleanup-test", MAX_TEMP_BYTES).unwrap();
        let path = workspace.path().to_path_buf();
        std::fs::write(workspace.path().join("page.png"), b"temporary").unwrap();
        assert!(path.exists());
        path
    };

    assert!(!path.exists());
}

#[test]
fn temporary_workspace_refuses_writes_beyond_budget() {
    let workspace = TempWorkspace::create("budget-test", 3).unwrap();
    let error = workspace.write("too-large.bin", b"1234").unwrap_err();

    assert_eq!(error.code(), "RESOURCE_LIMIT_EXCEEDED");
    assert!(!workspace.path().join("too-large.bin").exists());
}

#[test]
fn temporary_workspace_accepts_only_normal_relative_components() {
    let workspace = TempWorkspace::create("path-test", MAX_TEMP_BYTES).unwrap();
    assert!(workspace.write("nested/file.bin", b"ok").is_ok());
    for path in [
        "",
        ".",
        "./file.bin",
        "../file.bin",
        "/rooted.bin",
        r"\rooted.bin",
        r"C:\rooted.bin",
        "C:/rooted.bin",
        r"C:relative.bin",
        r"\\server\share\file.bin",
    ] {
        assert_eq!(workspace.write(path, b"no").unwrap_err().code(), "PARSE_FAILED", "{path}");
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn append_png_chunk(bytes: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc_data = kind.to_vec();
    crc_data.extend_from_slice(data);
    bytes.extend_from_slice(&crc_data);
    bytes.extend_from_slice(&crc32(&crc_data).to_be_bytes());
}

fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    append_png_chunk(&mut bytes, b"IHDR", &header);
    append_png_chunk(&mut bytes, b"IDAT", &[]);
    append_png_chunk(&mut bytes, b"IEND", &[]);
    bytes
}

#[test]
fn encoded_dimensions_over_twenty_five_megapixels_fail_before_pixel_decode() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("oversized.png");
    std::fs::write(&path, png_header(5_001, 5_000)).unwrap();

    let error = load_oriented_image(&path, &ResourceLimits::default()).unwrap_err();

    assert_eq!(error.code(), "RESOURCE_LIMIT_EXCEEDED");
}

#[test]
fn exactly_twenty_five_megapixels_passes_the_header_limit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("boundary.png");
    std::fs::write(&path, png_header(5_000, 5_000)).unwrap();

    let error = load_oriented_image(&path, &ResourceLimits::default()).unwrap_err();

    assert_eq!(error.code(), "PARSE_FAILED");
}
