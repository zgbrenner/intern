//! Plain text arrives in whatever encoding the program that wrote it used.
//! Notepad and PowerShell's redirection still write UTF-16 with a byte-order
//! mark, and a mark on a UTF-8 file is ordinary; none of that is a document
//! this worker may refuse or hand on with a stray glyph in front of it.

use std::path::PathBuf;

use intern_worker::extract::{
    CancellationToken, ExtractedDocument, ExtractionWarning, PageSource, extract_text,
};
use intern_worker::limits::ResourceLimits;
use tempfile::TempDir;

fn write(directory: &TempDir, bytes: &[u8]) -> PathBuf {
    let path = directory.path().join("notes.txt");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn extract(bytes: &[u8]) -> ExtractedDocument {
    let directory = tempfile::tempdir().unwrap();
    let path = write(&directory, bytes);
    extract_text(&path, &ResourceLimits::default(), &CancellationToken::new()).unwrap()
}

fn utf16_le(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    bytes
}

fn utf16_be(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFE, 0xFF];
    bytes.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
    bytes
}

#[test]
fn utf16_and_bom_text_files_are_read() {
    let expected = "Retainer notes\r\nBalance due: $4,200.00\r\n";

    let little_endian = extract(&utf16_le(expected));
    assert_eq!(little_endian.pages[0].text, expected);
    assert_eq!(little_endian.pages[0].source, PageSource::Text);
    assert!(little_endian.warnings.is_empty());

    let big_endian = extract(&utf16_be(expected));
    assert_eq!(big_endian.pages[0].text, expected);

    let mut with_utf8_mark = vec![0xEF, 0xBB, 0xBF];
    with_utf8_mark.extend_from_slice(expected.as_bytes());
    let marked = extract(&with_utf8_mark);
    assert_eq!(marked.pages[0].text, expected);
}

#[test]
fn plain_utf8_is_unchanged() {
    let document = extract("Retainer notes\nBalance due: $4,200.00\n".as_bytes());

    assert_eq!(
        document.pages[0].text,
        "Retainer notes\nBalance due: $4,200.00\n"
    );
    assert!(document.warnings.is_empty());
}

/// A legacy single-byte encoding is not decodable here, but losing the whole
/// document over one accented character is worse than reading it with the
/// character replaced and saying so.
#[test]
fn text_that_is_not_valid_utf8_is_read_lossily_and_flagged() {
    let document = extract(b"Fee schedule for Caf\xE9 Med\r\n");
    let text = &document.pages[0].text;

    assert!(text.starts_with("Fee schedule for Caf"), "{text}");
    assert!(text.contains("Med"), "{text}");
    assert_eq!(
        document.warnings,
        vec![ExtractionWarning::NativeTextCorrupt]
    );
}
