use std::path::PathBuf;

use intern_worker::email::extract_eml;
use intern_worker::extract::{CancellationToken, ExtractedDocument, ExtractionError, PageSource};
use intern_worker::limits::ResourceLimits;
use tempfile::TempDir;

fn write_eml(directory: &TempDir, bytes: &[u8]) -> PathBuf {
    let path = directory.path().join("message.eml");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn extract(bytes: &[u8]) -> Result<ExtractedDocument, ExtractionError> {
    let directory = tempfile::tempdir().unwrap();
    let path = write_eml(&directory, bytes);
    extract_eml(&path, &ResourceLimits::default(), &CancellationToken::new())
}

const NESTED_MULTIPART: &[u8] = b"From: Alice Example <alice@example.com>\r\n\
To: Bob <bob@example.com>\r\n\
Cc: carol@example.com\r\n\
Date: Thu, 21 Aug 2025 09:15:00 -0400\r\n\
Subject: Q3 invoice attached\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
\r\n\
--outer\r\n\
Content-Type: multipart/alternative; boundary=\"inner\"\r\n\
\r\n\
--inner\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hi Bob,\r\n\
\r\n\
The Q3 invoice is attached.\r\n\
--inner\r\n\
Content-Type: text/html\r\n\
\r\n\
<p>Hi Bob,</p>\r\n\
--inner--\r\n\
--outer\r\n\
Content-Type: application/pdf; name=\"invoice.pdf\"\r\n\
Content-Disposition: attachment; filename=\"invoice.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0=\r\n\
--outer--\r\n";

#[test]
fn the_header_block_is_emitted_in_fixed_order_before_the_body() {
    let document = extract(NESTED_MULTIPART).unwrap();

    assert_eq!(document.pages.len(), 1);
    assert_eq!(document.pages[0].page_number, 1);
    assert_eq!(document.pages[0].source, PageSource::Text);
    assert!(document.warnings.is_empty());
    assert_eq!(
        document.pages[0].text,
        "From: Alice Example <alice@example.com>\n\
         To: Bob <bob@example.com>\n\
         Cc: carol@example.com\n\
         Date: Thu, 21 Aug 2025 09:15:00 -0400\n\
         Subject: Q3 invoice attached\n\
         Sent: 2025-08-21T13:15:00Z\n\
         \n\
         Hi Bob,\n\
         \n\
         The Q3 invoice is attached.\n\
         \n\
         Attachment: invoice.pdf\n"
    );
}

#[test]
fn the_date_header_the_engine_validates_against_survives_verbatim() {
    let document = extract(NESTED_MULTIPART).unwrap();
    let text = &document.pages[0].text;

    assert!(
        text.contains("Date: Thu, 21 Aug 2025 09:15:00 -0400"),
        "{text}"
    );
    assert!(text.contains("Sent: 2025-08-21T13:15:00Z"), "{text}");
}

#[test]
fn nested_multiparts_yield_the_plain_body_and_list_the_attachment_without_extracting_it() {
    let document = extract(NESTED_MULTIPART).unwrap();
    let text = &document.pages[0].text;

    assert!(text.contains("The Q3 invoice is attached."), "{text}");
    assert!(!text.contains("<p>"), "{text}");
    assert!(text.ends_with("Attachment: invoice.pdf\n"), "{text}");
    assert!(!text.contains("JVBERi0="), "{text}");
}

#[test]
fn missing_headers_are_omitted_rather_than_emitted_empty() {
    let document = extract(
        b"From: alice@example.com\r\n\
          Subject: No date on this one\r\n\
          \r\n\
          Just a line of text.\r\n",
    )
    .unwrap();

    assert_eq!(
        document.pages[0].text,
        "From: alice@example.com\n\
         Subject: No date on this one\n\
         \n\
         Just a line of text.\n"
    );
}

#[test]
fn an_unparseable_date_header_keeps_the_verbatim_line_but_omits_the_sent_line() {
    let document = extract(
        b"From: alice@example.com\r\n\
          Date: sometime last Tuesday\r\n\
          Subject: Vague\r\n\
          \r\n\
          Body.\r\n",
    )
    .unwrap();
    let text = &document.pages[0].text;

    assert!(text.contains("Date: sometime last Tuesday"), "{text}");
    assert!(!text.contains("Sent:"), "{text}");
}

#[test]
fn an_html_only_email_falls_back_to_naively_detagged_text() {
    let document = extract(
        b"From: newsletter@example.com\r\n\
          Date: Mon, 2 Jun 2025 08:00:00 +0000\r\n\
          Subject: Weekly digest\r\n\
          Content-Type: text/html; charset=utf-8\r\n\
          \r\n\
          <html><body><p>Dear reader,</p><p>Rates &amp; terms changed.</p></body></html>\r\n",
    )
    .unwrap();
    let text = &document.pages[0].text;

    assert!(
        text.contains("Dear reader,\n\nRates & terms changed."),
        "{text}"
    );
    assert!(!text.contains('<'), "{text}");
}

#[test]
fn a_malformed_message_reports_a_parse_error_instead_of_panicking() {
    let error =
        extract(b" : this first header line starts with a space\r\n\r\nbody\r\n").unwrap_err();

    assert_eq!(error.code(), "PARSE_FAILED");
    assert!(!error.retryable());
}
