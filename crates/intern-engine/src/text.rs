//! Dependency-free text scanners shared by distillation and validation.
//!
//! Everything here is a hand-written scanner rather than a regular expression:
//! the whole document is swept several times per file, so these run on every
//! page of every document and must stay cheap and allocation-light.

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

const MONTH_ABBREVIATIONS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Returns the 1-based month number for a leading month word, if any.
pub fn leading_month(value: &str) -> Option<usize> {
    let lowered = value.trim_start();
    for (index, month) in MONTHS.iter().enumerate() {
        if lowered.len() >= month.len() && lowered[..month.len()].eq_ignore_ascii_case(month) {
            return Some(index + 1);
        }
    }
    for (index, month) in MONTH_ABBREVIATIONS.iter().enumerate() {
        if lowered.len() >= month.len()
            && lowered[..month.len()].eq_ignore_ascii_case(month)
            && lowered[month.len()..]
                .chars()
                .next()
                .is_none_or(|character| !character.is_ascii_alphabetic())
        {
            return Some(index + 1);
        }
    }
    None
}

fn is_year(value: &str) -> bool {
    value.len() == 4
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && matches!(value.parse::<u32>(), Ok(1900..=2199))
}

/// How far after a month word a year may sit and still belong to it.
const MONTH_TO_YEAR_WINDOW: usize = 16;

/// Counts date-like spans in `value`.
///
/// A date is anchored on a four-digit year or on a month word paired with a
/// nearby year, so `April 1, 2026`, `2026-04-01`, `3/14/2026`, and
/// `the 1st day of April, 2026` each count exactly once. Two-digit-year
/// numeric dates are recognised separately.
///
/// This is deliberately generous: a block that merely looks like it carries a
/// date is worth keeping so the model can decide what the date means. The
/// strict check happens later, against the evidence the model quotes back.
pub fn date_signal_count(value: &str) -> usize {
    let bytes = value.as_bytes();
    let mut years: Vec<(usize, bool)> = Vec::new();
    let mut months: Vec<usize> = Vec::new();
    let mut short_dates = 0;

    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_digit() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            let digits = &value[start..index];
            if is_year(digits) {
                years.push((start, false));
            } else if digits.len() <= 2 && short_numeric_date(&value[start..]) {
                short_dates += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_digit() || matches!(bytes[index], b'/' | b'-'))
                {
                    index += 1;
                }
            }
            continue;
        }
        if byte.is_ascii_alphabetic() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
                index += 1;
            }
            if (start == 0 || !bytes[start - 1].is_ascii_alphabetic())
                && leading_month(&value[start..index]).is_some()
            {
                months.push(index);
            }
            continue;
        }
        index += 1;
    }

    let mut count = short_dates;
    for month_end in months {
        if let Some(slot) = years.iter_mut().find(|(start, taken)| {
            !*taken && *start >= month_end && *start - month_end <= MONTH_TO_YEAR_WINDOW
        }) {
            slot.1 = true;
            count += 1;
        }
    }
    count + years.iter().filter(|(_, taken)| !*taken).count()
}

/// Matches `d/d/dd` shapes whose year is written with two digits.
fn short_numeric_date(rest: &str) -> bool {
    let mut groups = 0;
    let mut digits = 0;
    for character in rest.chars() {
        if character.is_ascii_digit() {
            digits += 1;
            if digits > 2 {
                return false;
            }
        } else if matches!(character, '/' | '-') && digits > 0 {
            groups += 1;
            digits = 0;
            if groups > 2 {
                return false;
            }
        } else {
            break;
        }
    }
    groups == 2 && digits == 2
}

/// Case-insensitive substring search without allocating a lowercase copy.
pub fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
    })
}

/// Counts how many of `needles` appear in `haystack`, case-insensitively.
pub fn count_cues(haystack: &str, needles: &[&str]) -> usize {
    needles
        .iter()
        .filter(|needle| contains_ignore_case(haystack, needle))
        .count()
}

/// True when the value looks like a document identifier such as `INV-2048`.
pub fn contains_identifier(value: &str) -> bool {
    value
        .split(|character: char| character.is_whitespace())
        .any(|token| {
            let token = token.trim_matches(|character: char| !character.is_ascii_alphanumeric());
            let letters = token.bytes().filter(u8::is_ascii_alphabetic).count();
            let digits = token.bytes().filter(u8::is_ascii_digit).count();
            letters >= 2 && digits >= 2 && token.len() <= 24
        })
}

/// True when the value contains a currency amount.
pub fn contains_money(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        matches!(byte, b'$' | b'\xa3')
            && bytes
                .get(index + 1..)
                .is_some_and(|rest| rest.iter().take(3).any(u8::is_ascii_digit))
    }) || contains_ignore_case(value, "USD ")
        || contains_ignore_case(value, "EUR ")
}

/// Replaces runs of digits with `#` so page footers collapse onto each other.
pub fn digit_masked(value: &str) -> String {
    let mut masked = String::with_capacity(value.len());
    let mut in_digits = false;
    for character in value.chars() {
        if character.is_ascii_digit() {
            if !in_digits {
                masked.push('#');
                in_digits = true;
            }
        } else {
            in_digits = false;
            masked.extend(character.to_lowercase());
        }
    }
    masked
}

/// Splits a long paragraph on sentence boundaries into chunks of at most
/// `max_characters`, keeping every character verbatim.
pub fn split_sentences(value: &str, max_characters: usize) -> Vec<String> {
    if value.chars().count() <= max_characters {
        return vec![value.to_owned()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut sentence = String::new();
    for character in value.chars() {
        sentence.push(character);
        let terminal = matches!(character, '.' | '!' | '?' | ';' | '\n');
        if !terminal {
            continue;
        }
        if current.chars().count() + sentence.chars().count() > max_characters
            && !current.is_empty()
        {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(&sentence);
        sentence.clear();
    }
    current.push_str(&sentence);
    // A single sentence longer than the budget is split on a character boundary
    // rather than dropped; the verbatim text is preserved either way.
    while current.chars().count() > max_characters * 2 {
        let head = current.chars().take(max_characters).collect::<String>();
        current = current.chars().skip(max_characters).collect();
        chunks.push(head);
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(value.to_owned());
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_the_date_shapes_business_documents_actually_use() {
        assert_eq!(date_signal_count("Effective Date: 2026-04-01"), 1);
        assert_eq!(date_signal_count("dated 3/14/2026"), 1);
        assert_eq!(date_signal_count("executed on April 1, 2026"), 1);
        assert_eq!(date_signal_count("as of the 1st day of April, 2026"), 1);
        assert_eq!(date_signal_count("Sept. 14, 2025 and Jan 5, 2026"), 2);
        assert_eq!(date_signal_count("due 3/14/25"), 1);
        assert_eq!(date_signal_count("no dates in this sentence at all"), 0);
        assert_eq!(date_signal_count("invoice total 1,248.00 dollars"), 0);
        assert_eq!(date_signal_count("payment may be made in full"), 0);
    }

    #[test]
    fn identifier_and_money_signals_ignore_ordinary_prose() {
        assert!(contains_identifier("Invoice INV-2048 is enclosed."));
        assert!(!contains_identifier("The parties agree as follows."));
        assert!(contains_money("Total due: $1,248.00"));
        assert!(!contains_money("Total due on receipt"));
    }

    #[test]
    fn digit_masking_collapses_running_page_footers() {
        assert_eq!(digit_masked("Page 3 of 10"), digit_masked("Page 7 of 10"));
        assert_ne!(digit_masked("Page 3 of 10"), digit_masked("Exhibit A"));
    }

    #[test]
    fn sentence_splitting_preserves_every_character() {
        let paragraph = "One sentence here. Another sentence there. A third one follows.";
        let chunks = split_sentences(paragraph, 25);
        assert!(chunks.len() > 1);
        assert_eq!(chunks.concat(), paragraph);
    }
}
