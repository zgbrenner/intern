//! An Outlook message is a compound file of MAPI property streams. One is
//! built here from the specification rather than copied from a mailbox, so
//! the test carries no real message and stays byte-deterministic.

use std::io::Write;

use intern_worker::email::extract_msg;
use intern_worker::extract::CancellationToken;
use intern_worker::limits::ResourceLimits;
use tempfile::tempdir;

/// 2026-03-04T15:22:10Z as a FILETIME: 100-nanosecond ticks since 1601.
const SUBMIT_TIME: u64 = 134_171_113_300_000_000;

fn utf16(value: &str) -> Vec<u8> {
    value.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A `__properties_version1.0` stream: the header for its storage kind,
/// then 16-byte entries of tag (type, id), flags, and value.
fn properties(root: bool, entries: &[(u16, u16, [u8; 8])]) -> Vec<u8> {
    let mut bytes = vec![0u8; if root { 32 } else { 8 }];
    for (kind, id, value) in entries {
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&id.to_le_bytes());
        bytes.extend_from_slice(&6u32.to_le_bytes());
        bytes.extend_from_slice(value);
    }
    bytes
}

fn long(value: u32) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&value.to_le_bytes());
    bytes
}

fn write_stream<F: std::io::Read + std::io::Write + std::io::Seek>(
    file: &mut cfb::CompoundFile<F>,
    path: &str,
    bytes: &[u8],
) {
    let mut stream = file.create_stream(path).unwrap();
    stream.write_all(bytes).unwrap();
}

fn forwarded_invoice(path: &std::path::Path) {
    // Outlook writes version 3 compound files (512-byte sectors), and that
    // is the version msg_parser reads.
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    let mut file = cfb::CompoundFile::create_with_version(cfb::Version::V3, handle).unwrap();
    write_stream(
        &mut file,
        "/__substg1.0_0037001F",
        &utf16("FW: Invoice INV-7741 for January"),
    );
    write_stream(&mut file, "/__substg1.0_0C1A001F", &utf16("Dana Ruiz"));
    write_stream(
        &mut file,
        "/__substg1.0_5D01001F",
        &utf16("dana.ruiz@ridgeline.example"),
    );
    write_stream(
        &mut file,
        "/__substg1.0_0E04001F",
        &utf16("Priya Nandakumar"),
    );
    write_stream(
        &mut file,
        "/__substg1.0_1000001F",
        &utf16(
            "Priya,\r\n\r\nForwarding the January invoice from Acme Corporation, $1,248.00, due \
             February 4, 2026. Please file it with the Vistage Worldwide, Inc. engagement.\r\n\r\nDana",
        ),
    );
    write_stream(
        &mut file,
        "/__properties_version1.0",
        &properties(
            true,
            &[
                (0x0040, 0x0039, SUBMIT_TIME.to_le_bytes()),
                (0x0040, 0x0E06, SUBMIT_TIME.to_le_bytes()),
            ],
        ),
    );
    file.create_storage("/__recip_version1.0_#00000000")
        .unwrap();
    write_stream(
        &mut file,
        "/__recip_version1.0_#00000000/__substg1.0_3001001F",
        &utf16("Priya Nandakumar"),
    );
    write_stream(
        &mut file,
        "/__recip_version1.0_#00000000/__substg1.0_39FE001F",
        &utf16("priya@vistage.example"),
    );
    write_stream(
        &mut file,
        "/__recip_version1.0_#00000000/__properties_version1.0",
        &properties(false, &[(0x0003, 0x0C15, long(1))]),
    );
    file.create_storage("/__recip_version1.0_#00000001")
        .unwrap();
    write_stream(
        &mut file,
        "/__recip_version1.0_#00000001/__substg1.0_3001001F",
        &utf16("Marcus Reyes"),
    );
    write_stream(
        &mut file,
        "/__recip_version1.0_#00000001/__substg1.0_39FE001F",
        &utf16("marcus@reyestolliver.example"),
    );
    write_stream(
        &mut file,
        "/__recip_version1.0_#00000001/__properties_version1.0",
        &properties(false, &[(0x0003, 0x0C15, long(2))]),
    );
    file.create_storage("/__attach_version1.0_#00000000")
        .unwrap();
    write_stream(
        &mut file,
        "/__attach_version1.0_#00000000/__substg1.0_3707001F",
        &utf16("INV-7741.pdf"),
    );
    write_stream(
        &mut file,
        "/__attach_version1.0_#00000000/__properties_version1.0",
        &properties(false, &[(0x0003, 0x3705, long(1))]),
    );
    file.flush().unwrap();
}

#[test]
fn an_outlook_message_becomes_the_same_page_an_eml_would() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("forwarded-invoice.msg");
    forwarded_invoice(&path);

    let extracted =
        extract_msg(&path, &ResourceLimits::default(), &CancellationToken::new()).unwrap();
    assert_eq!(extracted.pages.len(), 1);
    let text = &extracted.pages[0].text;
    let header = text.split("\n\n").next().unwrap_or_default();
    assert_eq!(
        header,
        "From: Dana Ruiz <dana.ruiz@ridgeline.example>\n\
         To: Priya Nandakumar <priya@vistage.example>\n\
         Cc: Marcus Reyes <marcus@reyestolliver.example>\n\
         Date: 2026-03-04 15:22:10 UTC\n\
         Subject: FW: Invoice INV-7741 for January\n\
         Sent: 2026-03-04T15:22:10Z",
        "{text}"
    );
    assert!(
        text.contains("Forwarding the January invoice from Acme Corporation"),
        "{text}"
    );
    assert!(text.ends_with("Attachment: INV-7741.pdf\n"), "{text}");
}

#[test]
fn a_file_that_is_not_a_compound_file_is_a_parse_failure_not_a_crash() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("not-really.msg");
    std::fs::write(
        &path,
        b"Subject: this is plain text pretending to be Outlook\r\n\r\nHello",
    )
    .unwrap();
    let error =
        extract_msg(&path, &ResourceLimits::default(), &CancellationToken::new()).unwrap_err();
    assert_eq!(error.code(), "PARSE_FAILED");
}
