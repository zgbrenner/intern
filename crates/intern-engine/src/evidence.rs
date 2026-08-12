//! Literal-evidence checking.
//!
//! Every fact that can reach a filename must be backed by a verbatim excerpt
//! the model quotes from the digest. Because distillation keeps text verbatim,
//! "the model quoted something that is actually in the document" is a check
//! Intern can make locally and cheaply, and it is the main reason a proposal
//! can be trusted without a human reading it.

use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

use crate::distill::DocumentDigest;

/// Folds case, normalizes Unicode, unifies quote characters, and collapses
/// whitespace so that a quote survives PDF and OCR typography differences.
pub fn normalize(value: &str) -> String {
    let normalized = value.nfkc().case_fold().map(|character| match character {
        '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' | '\u{2032}' => '\'',
        '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' | '\u{2033}' => '"',
        '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
        '\u{00a0}' | '\u{2007}' | '\u{202f}' => ' ',
        other => other,
    });

    let mut result = String::with_capacity(value.len());
    let mut pending_space = false;
    for character in normalized {
        if character.is_whitespace() {
            pending_space = !result.is_empty();
        } else {
            if pending_space {
                result.push(' ');
                pending_space = false;
            }
            result.push(character);
        }
    }
    result
}

/// True when `excerpt` appears verbatim inside a single kept block.
///
/// Checking per block rather than against the joined digest means a quote can
/// never be "supported" by text that straddles an elision marker.
pub fn digest_contains(digest: &DocumentDigest, excerpt: &str) -> bool {
    let excerpt = normalize(excerpt);
    !excerpt.is_empty()
        && digest
            .segments
            .iter()
            .any(|segment| normalize(segment).contains(&excerpt))
}

/// True when the quoted evidence both contains the claimed field value and is
/// itself present in the digest.
pub fn evidence_supports(digest: &DocumentDigest, excerpt: &str, field: &str) -> bool {
    let normalized_excerpt = normalize(excerpt);
    let normalized_field = normalize(field);
    !normalized_field.is_empty()
        && normalized_excerpt.contains(&normalized_field)
        && digest
            .segments
            .iter()
            .any(|segment| normalize(segment).contains(&normalized_excerpt))
}

/// True when the ISO date is written, in some ordinary human form, somewhere in
/// the document itself.
///
/// This is the check that matters. A small model paraphrases its own quotes -
/// it will answer "This Agreement is effective as of February 14, 2025" for a
/// document whose line actually reads "Effective date: February 14, 2025" - so
/// gating on the exact wrapper wording throws away correct dates. Gating on
/// whether the *date* is really in the document does not.
pub fn digest_contains_date(digest: &DocumentDigest, iso_date: &str) -> bool {
    digest
        .segments
        .iter()
        .any(|segment| date_matches_evidence(iso_date, segment))
}

/// True when the ISO date is written, in some ordinary human form, inside the
/// given text.
pub fn date_matches_evidence(iso_date: &str, excerpt: &str) -> bool {
    if iso_date.len() != 10 {
        return false;
    }
    let year = &iso_date[0..4];
    let month = &iso_date[5..7];
    let day = &iso_date[8..10];
    let Ok(month_number) = month.parse::<usize>() else {
        return false;
    };
    if !(1..=12).contains(&month_number) {
        return false;
    }
    let month_name = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ][month_number - 1];
    let month_unpadded = month.trim_start_matches('0');
    let day_unpadded = day.trim_start_matches('0');
    let ordinal = ordinal_suffix(day_unpadded);
    let normalized = normalize(excerpt);

    let mut candidates = vec![
        iso_date.to_owned(),
        format!("{year}/{month}/{day}"),
        format!("{month}/{day}/{year}"),
        format!("{month_unpadded}/{day_unpadded}/{year}"),
        format!("{month}-{day}-{year}"),
        format!("{month_unpadded}-{day_unpadded}-{year}"),
        format!("{day_unpadded}/{month_unpadded}/{year}"),
    ];
    // Documents abbreviate months as "Sep", "Sept", "Sept.", or write them out;
    // all of those support the same ISO date.
    let mut spellings = vec![month_name.to_owned()];
    for length in [3, 4] {
        if month_name.len() > length {
            let abbreviation = month_name[..length].to_owned();
            if !spellings.contains(&abbreviation) {
                spellings.push(abbreviation);
            }
        }
    }
    for spelling in spellings {
        for suffix in ["", "."] {
            let month_word = format!("{spelling}{suffix}");
            candidates.push(format!("{month_word} {day_unpadded}, {year}"));
            candidates.push(format!("{month_word} {day_unpadded} {year}"));
            candidates.push(format!("{month_word} {day_unpadded}{ordinal}, {year}"));
            candidates.push(format!("{month_word}-{day_unpadded}-{year}"));
            candidates.push(format!("{day_unpadded} {month_word} {year}"));
            candidates.push(format!("{day_unpadded}-{month_word}-{year}"));
            candidates.push(format!(
                "{day_unpadded}{ordinal} day of {month_word}, {year}"
            ));
            candidates.push(format!(
                "{day_unpadded}{ordinal} day of {month_word} {year}"
            ));
        }
    }
    candidates
        .iter()
        .any(|candidate| normalized.contains(&normalize(candidate)))
}

fn ordinal_suffix(day: &str) -> &'static str {
    match day.parse::<u32>() {
        Ok(11..=13) => "th",
        Ok(value) if value % 10 == 1 => "st",
        Ok(value) if value % 10 == 2 => "nd",
        Ok(value) if value % 10 == 3 => "rd",
        _ => "th",
    }
}

/// True when the value is a real calendar date in ISO `YYYY-MM-DD` form.
pub fn is_valid_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    if bytes
        .iter()
        .enumerate()
        .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return false;
    }
    let parse = |range: std::ops::Range<usize>| value[range].parse::<u32>().ok();
    let (Some(year), Some(month), Some(day)) = (parse(0..4), parse(5..7), parse(8..10)) else {
        return false;
    };
    if !(1000..=2999).contains(&year) {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let maximum = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=maximum).contains(&day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distill::{DigestBudget, distill, source_from_text};

    fn digest_of(text: &str) -> DocumentDigest {
        distill(&source_from_text(text), DigestBudget::default())
    }

    #[test]
    fn quotes_are_matched_through_typography_differences() {
        let digest = digest_of("The Company (\u{201c}Acme\u{201d}) shall\u{00a0}deliver.");
        assert!(digest_contains(
            &digest,
            "The Company (\"Acme\") shall deliver."
        ));
    }

    #[test]
    fn evidence_must_contain_the_claimed_value() {
        let digest = digest_of("This Statement of Work is between Acme and Vistage.");
        assert!(evidence_supports(
            &digest,
            "between Acme and Vistage",
            "Acme"
        ));
        assert!(!evidence_supports(
            &digest,
            "between Acme and Vistage",
            "Northwind"
        ));
    }

    #[test]
    fn a_quote_absent_from_the_document_is_rejected() {
        let digest = digest_of("This Statement of Work is between Acme and Vistage.");
        assert!(!digest_contains(&digest, "between Acme and Northwind"));
    }

    #[test]
    fn a_date_is_checked_against_the_document_not_the_models_wording() {
        let digest = digest_of("EMPLOYMENT AGREEMENT\n\nEffective date: February 14, 2025\n");
        // The model paraphrases its own quote; the date is still really there.
        assert!(digest_contains_date(&digest, "2025-02-14"));
        assert!(!digest_contains_date(&digest, "2025-02-15"));
    }

    #[test]
    fn iso_dates_are_matched_against_human_date_forms() {
        assert!(date_matches_evidence(
            "2026-04-01",
            "effective as of April 1, 2026"
        ));
        assert!(date_matches_evidence(
            "2026-04-01",
            "as of the 1st day of April, 2026"
        ));
        assert!(date_matches_evidence("2025-09-14", "dated 9/14/2025"));
        assert!(date_matches_evidence("2025-09-14", "Sept. 14, 2025"));
        assert!(date_matches_evidence("2026-01-05", "2026-01-05"));
        assert!(!date_matches_evidence(
            "2026-04-01",
            "effective as of April 2, 2026"
        ));
        assert!(!date_matches_evidence(
            "2026-04-01",
            "the term of this agreement"
        ));
    }

    #[test]
    fn calendar_validity_is_enforced() {
        assert!(is_valid_iso_date("2024-02-29"));
        assert!(!is_valid_iso_date("2025-02-29"));
        assert!(!is_valid_iso_date("2026-13-01"));
        assert!(!is_valid_iso_date("2026-4-1"));
    }
}
