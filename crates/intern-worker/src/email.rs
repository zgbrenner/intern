//! Email (.eml) extraction.
//!
//! An email becomes one page: a deterministic header block — `From`, `To`,
//! `Cc`, `Date` (as written in the message), `Subject`, then `Sent` as the
//! RFC 3339 form of the parsed `Date` — a blank line, the text/plain body
//! (falling back to naively de-tagged text/html), and finally one
//! `Attachment: <filename>` line per attachment. Attachments are listed,
//! never extracted. The `Date` line survives verbatim so downstream
//! validation can find the sent date in the document text.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use mailparse::{DispositionType, MailHeaderMap, ParsedMail};

use crate::extract::{
    CancellationToken, ExtractedDocument, ExtractedPage, ExtractionError, PageSource,
};
use crate::limits::ResourceLimits;

/// Header names emitted before the body, in this exact order. `Sent` follows
/// them when the `Date` header parses.
const EMITTED_HEADERS: [&str; 5] = ["From", "To", "Cc", "Date", "Subject"];

pub fn extract_eml(
    path: &Path,
    limits: &ResourceLimits,
    cancel: &CancellationToken,
) -> Result<ExtractedDocument, ExtractionError> {
    cancel.check()?;
    let bytes = read_bounded(path, limits, cancel)?;
    let mail = mailparse::parse_mail(&bytes)
        .map_err(|error| ExtractionError::parse_failed(format!("email did not parse: {error}")))?;
    cancel.check()?;
    let text = render_email(&mail);
    Ok(ExtractedDocument {
        pages: vec![ExtractedPage {
            page_number: 1,
            text,
            source: PageSource::Text,
            ocr_confidence: None,
            vision_escalated: false,
        }],
        warnings: vec![],
        truncated: false,
        optional_image: None,
    })
}

fn read_bounded(
    path: &Path,
    limits: &ResourceLimits,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, ExtractionError> {
    let metadata = std::fs::metadata(path).map_err(ExtractionError::io)?;
    limits.validate_source_size(metadata.len())?;
    let file = File::open(path).map_err(ExtractionError::io)?;
    let mut reader = BufReader::new(file).take(limits.max_source_bytes + 1);
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        cancel.check()?;
        let read = reader.read(&mut buffer).map_err(ExtractionError::io)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    limits.validate_source_size(bytes.len() as u64)?;
    Ok(bytes)
}

fn render_email(mail: &ParsedMail) -> String {
    let mut lines = Vec::new();
    for name in EMITTED_HEADERS {
        if let Some(value) = mail.headers.get_first_value(name) {
            let value = single_line(&value);
            if !value.is_empty() {
                lines.push(format!("{name}: {value}"));
            }
        }
    }
    // `dateparse` returns Ok(0) when it never finds a day/month/year in the
    // value, so an exact 0 is treated as "did not parse" rather than as the
    // Unix epoch — no real email is sent at that instant.
    if let Some(date) = mail.headers.get_first_value("Date")
        && let Ok(epoch) = mailparse::dateparse(&date)
        && epoch != 0
    {
        lines.push(format!("Sent: {}", rfc3339_utc(epoch)));
    }

    let mut parts = MessageParts::default();
    collect_parts(mail, &mut parts);
    let body = match (parts.plain, parts.html) {
        (Some(plain), _) => plain,
        (None, Some(html)) => html_to_text(&html),
        (None, None) => String::new(),
    };
    let body = body.replace("\r\n", "\n").replace('\r', "\n");

    let mut text = lines.join("\n");
    text.push('\n');
    let body = body.trim_matches(['\n', '\r']).trim_end();
    if !body.is_empty() {
        text.push('\n');
        text.push_str(body);
        text.push('\n');
    }
    if !parts.attachments.is_empty() {
        text.push('\n');
        for attachment in &parts.attachments {
            text.push_str(&format!("Attachment: {}\n", single_line(attachment)));
        }
    }
    text
}

#[derive(Default)]
struct MessageParts {
    plain: Option<String>,
    html: Option<String>,
    attachments: Vec<String>,
}

/// Walks the (possibly nested) MIME tree depth-first, keeping the first
/// text/plain and first text/html bodies and listing every attachment.
fn collect_parts(part: &ParsedMail, parts: &mut MessageParts) {
    let disposition = part.get_content_disposition();
    if disposition.disposition == DispositionType::Attachment {
        let filename = disposition
            .params
            .get("filename")
            .or_else(|| part.ctype.params.get("name"))
            .cloned()
            .unwrap_or_else(|| "(unnamed)".to_owned());
        parts.attachments.push(filename);
        return;
    }
    if part.ctype.mimetype.starts_with("multipart/") {
        for subpart in &part.subparts {
            collect_parts(subpart, parts);
        }
        return;
    }
    match part.ctype.mimetype.as_str() {
        "text/plain" if parts.plain.is_none() => parts.plain = part.get_body().ok(),
        "text/html" if parts.html.is_none() => parts.html = part.get_body().ok(),
        _ => {}
    }
}

/// Collapses folded or multi-line header values onto one line so the header
/// block always has exactly one line per header.
fn single_line(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned()
}

/// Formats a Unix timestamp as an RFC 3339 UTC instant, e.g.
/// `2025-08-21T13:15:00Z`.
fn rfc3339_utc(epoch_seconds: i64) -> String {
    let days = epoch_seconds.div_euclid(86_400);
    let seconds_of_day = epoch_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = seconds_of_day % 3_600 / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Days-since-1970-01-01 to a proleptic Gregorian civil date
/// (Howard Hinnant's `civil_from_days` algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Naive HTML-to-text: drops `<style>`/`<script>` blocks, turns structural
/// tags into line breaks, strips every other tag, and decodes the handful of
/// entities that matter in prose.
fn html_to_text(html: &str) -> String {
    let without_blocks = strip_container(&strip_container(html, "script"), "style");
    let mut text = String::with_capacity(without_blocks.len());
    let mut rest = without_blocks.as_str();
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('>') else {
            rest = "";
            break;
        };
        let tag = after[..close]
            .trim_start_matches('/')
            .split([' ', '\t', '\n', '\r', '/'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(
            tag.as_str(),
            "br" | "p" | "div" | "tr" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
        ) {
            text.push('\n');
        }
        rest = &after[close + 1..];
    }
    text.push_str(rest);
    collapse_blank_lines(&decode_entities(&text))
}

/// Removes `<tag ...> ... </tag>` spans case-insensitively, including the tags.
fn strip_container(html: &str, tag: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}");
    let mut result = String::with_capacity(html.len());
    let mut cursor = 0;
    while let Some(found) = lower[cursor..].find(&open) {
        let start = cursor + found;
        result.push_str(&html[cursor..start]);
        cursor = match lower[start..].find(&close) {
            Some(end) => match lower[start + end..].find('>') {
                Some(closing) => start + end + closing + 1,
                None => lower.len(),
            },
            None => lower.len(),
        };
    }
    result.push_str(&html[cursor..]);
    result
}

fn decode_entities(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(ampersand) = rest.find('&') {
        result.push_str(&rest[..ampersand]);
        let after = &rest[ampersand..];
        let entity_end = after[..after.len().min(12)].find(';');
        let Some(end) = entity_end else {
            result.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix('#')
                .and_then(|digits| digits.parse::<u32>().ok())
                .and_then(char::from_u32),
        };
        match decoded {
            Some(character) => {
                result.push(character);
                rest = &after[end + 1..];
            }
            None => {
                result.push('&');
                rest = &after[1..];
            }
        }
    }
    result.push_str(rest);
    result
}

fn collapse_blank_lines(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut blank_run = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            blank_run += 1;
            continue;
        }
        if !result.is_empty() {
            result.push('\n');
            if blank_run > 0 {
                result.push('\n');
            }
        }
        blank_run = 0;
        result.push_str(line);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epochs_format_as_rfc3339_utc_across_leap_years_and_the_epoch_boundary() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(951_827_696), "2000-02-29T12:34:56Z");
        assert_eq!(rfc3339_utc(1_755_782_100), "2025-08-21T13:15:00Z");
        assert_eq!(rfc3339_utc(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn naive_html_detagging_breaks_on_structure_and_decodes_common_entities() {
        let html = "<html><style>p{color:red}</style><body>\
             <p>Dear&nbsp;Bob,</p><p>Fees &amp; taxes are &lt;due&gt;.</p>\
             <script>alert(1)</script></body></html>";
        assert_eq!(html_to_text(html), "Dear Bob,\n\nFees & taxes are <due>.");
    }
}
