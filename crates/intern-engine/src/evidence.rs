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

/// `normalize`, minus the punctuation that typography and typing scatter
/// through a name: commas, periods, apostrophes, and quotation marks.
///
/// "Vistage Worldwide Inc" and "Vistage Worldwide, Inc." are one company, and
/// a name a person would recognise as the same name should not be thrown out
/// of a filename over a comma.
pub fn normalize_loosely(value: &str) -> String {
    let stripped = normalize(value)
        .chars()
        .filter(|character| !matches!(character, '.' | ',' | '\'' | '"'))
        .collect::<String>();
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// True when `excerpt` appears inside a single kept block once punctuation is
/// disregarded on both sides. Used only for names, where punctuation is
/// typography rather than meaning.
pub fn digest_contains_loosely(digest: &DocumentDigest, excerpt: &str) -> bool {
    let excerpt = normalize_loosely(excerpt);
    !excerpt.is_empty()
        && digest
            .segments
            .iter()
            .any(|segment| normalize_loosely(segment).contains(&excerpt))
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
    !date_match_positions(iso_date, &normalize(excerpt)).is_empty()
}

/// Byte offsets in `normalized` (already `normalize`d text) where a spelling
/// of `iso_date` begins. Every offset is a real statement of that date; a
/// caller judging context - what wording introduces the date - needs all of
/// them, because one date can be stated twice on a line in different roles.
pub fn date_match_positions(iso_date: &str, normalized: &str) -> Vec<usize> {
    if iso_date.len() != 10 {
        return Vec::new();
    }
    let year = &iso_date[0..4];
    let month = &iso_date[5..7];
    let day = &iso_date[8..10];
    let Ok(month_number) = month.parse::<usize>() else {
        return Vec::new();
    };
    if !(1..=12).contains(&month_number) {
        return Vec::new();
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
    let short_year = &year[2..];

    // Numeric shapes. Day-first forms are accepted alongside month-first ones
    // because the check asks whether the date is written in the document, and
    // a European invoice writes 1 April as 01/04/2026; the model has already
    // decided which reading the document supports.
    let mut candidates = vec![
        iso_date.to_owned(),
        format!("{year}/{month}/{day}"),
        format!("{year}.{month}.{day}"),
        format!("{month}/{day}/{year}"),
        format!("{month_unpadded}/{day_unpadded}/{year}"),
        format!("{month}-{day}-{year}"),
        format!("{month_unpadded}-{day_unpadded}-{year}"),
        format!("{month}.{day}.{year}"),
        format!("{day}/{month}/{year}"),
        format!("{day_unpadded}/{month_unpadded}/{year}"),
        format!("{day}-{month}-{year}"),
        format!("{day_unpadded}-{month_unpadded}-{year}"),
        format!("{day}.{month}.{year}"),
        format!("{day_unpadded}.{month_unpadded}.{year}"),
        // Two-digit years, the way forms and invoices abbreviate them. The
        // boundary check below keeps "4/1/26" from matching inside "14/1/26".
        format!("{month_unpadded}/{day_unpadded}/{short_year}"),
        format!("{month}/{day}/{short_year}"),
        format!("{day_unpadded}/{month_unpadded}/{short_year}"),
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
            for day_form in [day_unpadded, day] {
                candidates.push(format!("{month_word} {day_form}, {year}"));
                candidates.push(format!("{month_word} {day_form} {year}"));
                candidates.push(format!("{month_word}-{day_form}-{year}"));
                candidates.push(format!("{day_form} {month_word} {year}"));
                candidates.push(format!("{day_form} {month_word}, {year}"));
                candidates.push(format!("{day_form}-{month_word}-{year}"));
            }
            candidates.push(format!("{month_word} {day_unpadded}{ordinal}, {year}"));
            candidates.push(format!("{month_word} {day_unpadded}{ordinal} {year}"));
            candidates.push(format!("{day_unpadded}{ordinal} {month_word} {year}"));
            candidates.push(format!("{day_unpadded}{ordinal} {month_word}, {year}"));
            candidates.push(format!(
                "{day_unpadded}{ordinal} day of {month_word}, {year}"
            ));
            candidates.push(format!(
                "{day_unpadded}{ordinal} day of {month_word} {year}"
            ));
            candidates.push(format!("{day_unpadded}{ordinal} of {month_word}, {year}"));
            candidates.push(format!("{day_unpadded}{ordinal} of {month_word} {year}"));
        }
    }
    let bytes = normalized.as_bytes();
    let mut positions = Vec::new();
    for candidate in &candidates {
        let candidate = normalize(candidate);
        if candidate.is_empty() {
            continue;
        }
        let mut from = 0;
        while let Some(found) = normalized[from..].find(&candidate) {
            let position = from + found;
            let end = position + candidate.len();
            // A spelling that runs straight into other digits is part of a
            // longer number, not this date: "12/1/2026" states December 1 and
            // must never support February 1 because "2/1/2026" sits inside it.
            let digit_before = position > 0 && bytes[position - 1].is_ascii_digit();
            let digit_after = bytes.get(end).is_some_and(u8::is_ascii_digit);
            if !digit_before && !digit_after && !positions.contains(&position) {
                positions.push(position);
            }
            from = position + candidate.chars().next().map_or(1, char::len_utf8);
        }
    }
    positions.sort_unstable();
    positions
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

    /// Every spelling here came from a real document shape: forms that
    /// zero-pad the day, British and European orders, dotted numerics, and
    /// the two-digit years invoices abbreviate to.
    #[test]
    fn the_spellings_documents_actually_use_all_support_the_same_date() {
        for spelling in [
            "April 1st 2026",
            "April 01, 2026",
            "1st April 2026",
            "1st April, 2026",
            "1 April, 2026",
            "the 1st of April, 2026",
            "the 1st of April 2026",
            "01/04/2026",
            "01-04-2026",
            "01.04.2026",
            "1.4.2026",
            "2026.04.01",
            "Date: 4/1/26",
            "Date: 04/01/26",
            "APRIL 1, 2026",
        ] {
            assert!(
                date_matches_evidence("2026-04-01", spelling),
                "{spelling} should support 2026-04-01"
            );
        }
    }

    /// A date is a whole token. "12/1/2026" states December 1 and contains
    /// the characters "2/1/2026", which must not make it support February 1.
    #[test]
    fn a_date_inside_a_longer_number_is_not_a_statement_of_that_date() {
        assert!(!date_matches_evidence("2026-02-01", "dated 12/1/2026"));
        assert!(!date_matches_evidence("2026-04-01", "reference 14/1/2026"));
        assert!(!date_matches_evidence("2026-04-01", "order 4/1/2026001"));
        assert!(!date_matches_evidence("2026-04-01", "code 24/1/26"));
        assert!(date_matches_evidence("2026-12-01", "dated 12/1/2026"));
        assert!(date_matches_evidence("2026-04-01", "(4/1/2026)"));
    }

    #[test]
    fn loose_matching_ignores_only_punctuation() {
        let digest = digest_of("by and between Vistage Worldwide, Inc. and Jane O'Brien");
        assert!(digest_contains_loosely(&digest, "Vistage Worldwide Inc"));
        assert!(digest_contains_loosely(&digest, "Vistage Worldwide, Inc."));
        assert!(digest_contains_loosely(&digest, "Jane OBrien"));
        assert!(!digest_contains_loosely(&digest, "Vistage Worldwide LLC"));
        assert!(!digest_contains_loosely(&digest, "Vistage Inc"));
        assert_eq!(normalize_loosely("  Acme,  Inc. "), "acme inc");
    }

    #[test]
    fn calendar_validity_is_enforced() {
        assert!(is_valid_iso_date("2024-02-29"));
        assert!(!is_valid_iso_date("2025-02-29"));
        assert!(!is_valid_iso_date("2026-13-01"));
        assert!(!is_valid_iso_date("2026-4-1"));
    }
}

/// Every date the document states, in the order it first states them, with
/// duplicates removed and the list capped so a long agreement's schedule of
/// dates does not become a wall of buttons. Drawn from the digest's date
/// lines, so each is a date a person could find on the page - which is what
/// makes it fit to offer a reviewer who has to give a document a date the
/// model did not.
pub fn stated_dates(digest: &DocumentDigest) -> Vec<String> {
    const MOST: usize = 8;
    let mut found: Vec<String> = Vec::new();
    for line in &digest.date_lines {
        for date in extract_stated_dates(line) {
            if !found.contains(&date) {
                found.push(date);
                if found.len() == MOST {
                    return found;
                }
            }
        }
    }
    found
}

/// Every ISO date a line states with a written month, a hyphenated written
/// month, or ISO/slash notation. Purely numeric forms like `3/4/2026` are
/// deliberately not extracted: without the document's locale they are
/// ambiguous, and this feeds a substitution that must never guess.
pub fn extract_stated_dates(line: &str) -> Vec<String> {
    const MONTHS: [&str; 12] = [
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
    ];
    fn month_number(token: &str) -> Option<usize> {
        let token = token.trim_end_matches('.');
        MONTHS.iter().position(|month| {
            *month == token
                || (token.len() >= 3 && month.len() > token.len() && month.starts_with(token))
        })
    }
    fn day_number(token: &str) -> Option<u32> {
        let digits = token.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        let day = digits.parse::<u32>().ok()?;
        ((1..=31).contains(&day) && digits.len() <= 2).then_some(day)
    }
    fn year_number(token: &str) -> Option<u32> {
        let token = token.trim_end_matches('.');
        let year = token.parse::<u32>().ok()?;
        ((1000..=2999).contains(&year) && token.len() == 4).then_some(year)
    }
    fn push(found: &mut Vec<String>, year: u32, month: usize, day: u32) {
        let iso = format!("{year:04}-{:02}-{day:02}", month + 1);
        if is_valid_iso_date(&iso) && !found.contains(&iso) {
            found.push(iso);
        }
    }

    let normalized = normalize(line);
    let tokens: Vec<&str> = normalized
        .split_whitespace()
        .map(|token| {
            token.trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '(' | ')' | '"' | '\''))
        })
        .collect();
    let mut found = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        // ISO 2026-04-01 and slashed 2026/04/01, possibly ending a sentence.
        let bare = token.trim_end_matches('.');
        if bare.len() == 10 && (bare.as_bytes()[4] == b'-' || bare.as_bytes()[4] == b'/') {
            let iso = bare.replace('/', "-");
            if is_valid_iso_date(&iso) && !found.contains(&iso) {
                found.push(iso);
            }
            continue;
        }
        // Hyphenated written month: 2-june-2023 or june-2-2023.
        let parts: Vec<&str> = bare.split('-').collect();
        if parts.len() == 3 {
            if let (Some(month), Some(day), Some(year)) = (
                month_number(parts[1]),
                day_number(parts[0]),
                year_number(parts[2]),
            ) {
                push(&mut found, year, month, day);
                continue;
            }
            if let (Some(month), Some(day), Some(year)) = (
                month_number(parts[0]),
                day_number(parts[1]),
                year_number(parts[2]),
            ) {
                push(&mut found, year, month, day);
                continue;
            }
        }
        let Some(month) = month_number(bare) else {
            continue;
        };
        // "june 2, 2023" / "june 2 2023" / "june 2nd, 2023"
        if let (Some(Some(day)), Some(Some(year))) = (
            tokens.get(index + 1).map(|t| day_number(t)),
            tokens.get(index + 2).map(|t| year_number(t)),
        ) {
            push(&mut found, year, month, day);
            continue;
        }
        // "2 june 2023" and "2nd day of june, 2023"
        let day_before = index
            .checked_sub(1)
            .and_then(|i| day_number(tokens[i]))
            .or_else(|| {
                index.checked_sub(3).and_then(|i| {
                    (tokens[i + 1] == "day" && tokens[i + 2] == "of")
                        .then(|| day_number(tokens[i]))
                        .flatten()
                })
            });
        if let (Some(day), Some(Some(year))) =
            (day_before, tokens.get(index + 1).map(|t| year_number(t)))
        {
            push(&mut found, year, month, day);
        }
    }
    found
}

#[cfg(test)]
mod stated_date_tests {
    use super::extract_stated_dates;

    #[test]
    fn extracts_the_written_and_iso_shapes_documents_use() {
        assert_eq!(
            extract_stated_dates("Issued under the Master Services Agreement dated June 2, 2023"),
            vec!["2023-06-02".to_owned()]
        );
        assert_eq!(
            extract_stated_dates(
                "This Statement of Work is effective as of April 1, 2026 and continues"
            ),
            vec!["2026-04-01".to_owned()]
        );
        assert_eq!(
            extract_stated_dates("Delivered 3 March 2026."),
            vec!["2026-03-03".to_owned()]
        );
        assert_eq!(
            extract_stated_dates("signed this 2nd day of June, 2023"),
            vec!["2023-06-02".to_owned()]
        );
        assert_eq!(
            extract_stated_dates("Due on 2026-04-01."),
            vec!["2026-04-01".to_owned()]
        );
        assert_eq!(
            extract_stated_dates("filed 2-June-2023"),
            vec!["2023-06-02".to_owned()]
        );
    }

    #[test]
    fn never_guesses_at_ambiguous_or_broken_shapes() {
        assert!(extract_stated_dates("due 3/4/2026").is_empty());
        assert!(extract_stated_dates("Invoice 2026 covers May and June").is_empty());
        assert!(extract_stated_dates("February 30, 2026 is not a date").is_empty());
        assert!(extract_stated_dates("see section 4, page 2023").is_empty());
    }
}
