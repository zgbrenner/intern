//! Filename composition.
//!
//! Intern's whole visible output is one line of text in a folder listing, so
//! the shape is fixed and readable:
//!
//! ```text
//! YYYY-MM-DD <what the document is> <who it is between>.<original extension>
//! 2026-12-29 Notice of Termination for John Smith.pdf
//! 2026-04-01 Statement of Work between Acme and Vistage.pdf
//! ```
//!
//! When the name would be too long to scan, detail is shed from the least
//! identifying end first: the second party, then the party clause, then the
//! document type.

use std::collections::HashSet;

use crate::domain::{ComposedName, PartyRelation, ValidatedProposal};

/// Long enough to stay specific, short enough to read in a folder listing.
pub const MAX_FILENAME_CHARS: usize = 120;
const MIN_STEM_CHARS: usize = 4;

pub fn compose_filename(
    proposal: &ValidatedProposal,
    extension: &str,
    existing_names: &[&str],
) -> ComposedName {
    let extension = sanitize_extension(extension);
    let date = proposal
        .document_date
        .as_deref()
        .and_then(sanitize_segment)
        .unwrap_or_default();
    let document_type = proposal
        .document_type
        .as_deref()
        .map(|value| strip_duplicate_extension(value, &extension))
        .and_then(sanitize_segment)
        .unwrap_or_else(|| "Document".to_owned());
    let parties = proposal
        .parties
        .iter()
        .filter_map(|party| sanitize_segment(party))
        .collect::<Vec<_>>();

    let existing = existing_names
        .iter()
        .map(|value| windows_name_key(value))
        .collect::<HashSet<_>>();

    let mut collision_index = 1;
    loop {
        let suffix = if collision_index == 1 {
            String::new()
        } else {
            format!(" ({collision_index})")
        };
        let value = fit(
            &date,
            &document_type,
            &parties,
            proposal.party_relation,
            &suffix,
            &extension,
        );
        if !existing.contains(&windows_name_key(&value)) {
            return ComposedName {
                value,
                collision_index,
            };
        }
        collision_index += 1;
    }
}

/// Builds `date + type + party clause`, shedding detail until it fits.
fn fit(
    date: &str,
    document_type: &str,
    parties: &[String],
    relation: PartyRelation,
    suffix: &str,
    extension: &str,
) -> String {
    let extension_part = if extension.is_empty() {
        String::new()
    } else {
        format!(".{extension}")
    };
    let reserved = suffix.chars().count() + extension_part.chars().count();
    let available = MAX_FILENAME_CHARS
        .saturating_sub(reserved)
        .max(MIN_STEM_CHARS);

    let attempts = [
        stem(date, document_type, parties, relation),
        stem(
            date,
            document_type,
            parties
                .first()
                .map(std::slice::from_ref)
                .unwrap_or_default(),
            single_party_relation(relation),
        ),
        stem(date, document_type, &[], PartyRelation::None),
    ];
    for attempt in &attempts {
        if attempt.chars().count() <= available {
            return format!("{attempt}{suffix}{extension_part}");
        }
    }
    let mut truncated = attempts
        .last()
        .cloned()
        .unwrap_or_default()
        .chars()
        .take(available)
        .collect::<String>();
    while truncated.ends_with(' ') || truncated.ends_with('.') {
        truncated.pop();
    }
    if truncated.is_empty() {
        truncated.push_str("Document");
    }
    format!("{truncated}{suffix}{extension_part}")
}

/// "between" only makes sense with two sides.
fn single_party_relation(relation: PartyRelation) -> PartyRelation {
    match relation {
        PartyRelation::Between => PartyRelation::With,
        other => other,
    }
}

fn stem(date: &str, document_type: &str, parties: &[String], relation: PartyRelation) -> String {
    let mut value = String::new();
    if !date.is_empty() {
        value.push_str(date);
        value.push(' ');
    }
    value.push_str(document_type);
    if let Some(clause) = party_clause(parties, relation) {
        value.push(' ');
        value.push_str(&clause);
    }
    value.trim().to_owned()
}

fn party_clause(parties: &[String], relation: PartyRelation) -> Option<String> {
    if parties.is_empty() || relation == PartyRelation::None {
        return None;
    }
    // Only "between" joins two names. A notice is "for John Smith", not "for
    // John Smith and the company that sent it"; an invoice is "from Acme", not
    // "from Acme and the customer".
    let names = match (relation, parties) {
        (PartyRelation::Between, [first, second, ..]) => format!("{first} and {second}"),
        (_, [only, ..]) => only.clone(),
        (_, []) => return None,
    };
    Some(format!("{} {names}", relation.as_str()))
}

fn sanitize_extension(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('.')
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .take(16)
        .collect()
}

fn strip_duplicate_extension<'a>(value: &'a str, extension: &str) -> &'a str {
    if extension.is_empty() {
        return value;
    }
    let mut stripped = value;
    let suffix_length = extension.len() + 1;
    loop {
        let Some(start) = stripped.len().checked_sub(suffix_length) else {
            break;
        };
        let Some(suffix) = stripped.get(start..) else {
            break;
        };
        if suffix.starts_with('.') && suffix[1..].eq_ignore_ascii_case(extension) {
            let Some(prefix) = stripped.get(..start) else {
                break;
            };
            stripped = prefix;
        } else {
            break;
        }
    }
    stripped
}

fn sanitize_segment(value: &str) -> Option<String> {
    let mut output = String::new();
    let mut pending_space = false;
    for character in value.chars() {
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if character.is_control()
            || is_bidi_control(character)
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        {
            continue;
        }
        if pending_space {
            output.push(' ');
            pending_space = false;
        }
        output.push(character);
    }
    while output.ends_with(' ') || output.ends_with('.') {
        output.pop();
    }
    if output.is_empty() {
        return None;
    }
    if is_reserved_device_name(&output) {
        output.insert(0, '_');
    }
    Some(output)
}

fn is_bidi_control(character: char) -> bool {
    matches!(character as u32, 0x061c | 0x200e..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
}

fn is_reserved_device_name(value: &str) -> bool {
    let stem = value
        .split('.')
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem.strip_prefix("COM").is_some_and(is_reserved_number)
        || stem.strip_prefix("LPT").is_some_and(is_reserved_number)
}

fn is_reserved_number(value: &str) -> bool {
    value.len() == 1 && matches!(value.as_bytes()[0], b'1'..=b'9')
}

fn windows_name_key(value: &str) -> String {
    value.trim_end_matches([' ', '.']).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DateRole, Evidence};

    fn proposal(
        date: Option<&str>,
        document_type: Option<&str>,
        parties: &[&str],
        relation: PartyRelation,
    ) -> ValidatedProposal {
        ValidatedProposal {
            document_type: document_type.map(str::to_owned),
            document_date: date.map(str::to_owned),
            date_role: date.map(|_| DateRole::Effective),
            parties: parties.iter().map(|value| (*value).to_owned()).collect(),
            party_relation: relation,
            description: "A description.".into(),
            confidence: 0.9,
            evidence: Evidence::default(),
        }
    }

    fn name(proposal: &ValidatedProposal, extension: &str) -> String {
        compose_filename(proposal, extension, &[]).value
    }

    #[test]
    fn produces_the_documented_shape() {
        assert_eq!(
            name(
                &proposal(
                    Some("2026-12-29"),
                    Some("Notice of Termination"),
                    &["John Smith"],
                    PartyRelation::For
                ),
                "pdf"
            ),
            "2026-12-29 Notice of Termination for John Smith.pdf"
        );
        assert_eq!(
            name(
                &proposal(
                    Some("2026-04-01"),
                    Some("Statement of Work"),
                    &["Acme", "Vistage"],
                    PartyRelation::Between
                ),
                "pdf"
            ),
            "2026-04-01 Statement of Work between Acme and Vistage.pdf"
        );
        assert_eq!(
            name(
                &proposal(
                    Some("2025-09-14"),
                    Some("Amendment to Consulting Agreement"),
                    &["Jane Smith"],
                    PartyRelation::With
                ),
                "pdf"
            ),
            "2025-09-14 Amendment to Consulting Agreement with Jane Smith.pdf"
        );
        assert_eq!(
            name(
                &proposal(
                    Some("2026-01-05"),
                    Some("Invoice"),
                    &["Acme Corporation"],
                    PartyRelation::From
                ),
                "pdf"
            ),
            "2026-01-05 Invoice from Acme Corporation.pdf"
        );
    }

    #[test]
    fn only_an_agreement_joins_two_names() {
        assert_eq!(
            name(
                &proposal(
                    Some("2026-12-29"),
                    Some("Notice of Termination"),
                    &["John Smith", "Northstar Lantern Works LLC"],
                    PartyRelation::For
                ),
                "pdf"
            ),
            "2026-12-29 Notice of Termination for John Smith.pdf"
        );
        assert_eq!(
            name(
                &proposal(
                    Some("2026-01-05"),
                    Some("Invoice"),
                    &["Acme Corporation", "Vistage Worldwide, Inc."],
                    PartyRelation::From
                ),
                "pdf"
            ),
            "2026-01-05 Invoice from Acme Corporation.pdf"
        );
    }

    #[test]
    fn the_extension_is_always_preserved() {
        let value = name(
            &proposal(
                Some("2026-01-05"),
                Some("Invoice"),
                &[],
                PartyRelation::None,
            ),
            "DOCX",
        );
        assert!(value.ends_with(".docx"));
    }

    #[test]
    fn no_parties_leaves_a_clean_name() {
        assert_eq!(
            name(
                &proposal(
                    Some("2025-05-07"),
                    Some("Meeting Minutes"),
                    &[],
                    PartyRelation::None
                ),
                "md"
            ),
            "2025-05-07 Meeting Minutes.md"
        );
    }

    #[test]
    fn a_missing_type_still_produces_a_usable_name() {
        assert_eq!(
            name(
                &proposal(Some("2025-05-07"), None, &[], PartyRelation::None),
                "pdf"
            ),
            "2025-05-07 Document.pdf"
        );
    }

    #[test]
    fn overlong_names_shed_the_second_party_before_the_type() {
        let value = name(
            &proposal(
                Some("2026-04-01"),
                Some("Master Professional Services and Technology Implementation Agreement"),
                &[
                    "Northstar Lantern Works Limited Liability Company",
                    "Copper Wren Design Incorporated",
                ],
                PartyRelation::Between,
            ),
            "pdf",
        );
        assert!(value.chars().count() <= MAX_FILENAME_CHARS);
        assert!(value.contains("Master Professional Services"));
        assert!(!value.contains("Copper Wren"));
    }

    #[test]
    fn windows_hostile_characters_and_device_names_are_neutralised() {
        let value = name(
            &proposal(
                Some("2026-04-01"),
                Some("Invoice: 3/4 <draft>"),
                &["CON"],
                PartyRelation::From,
            ),
            "pdf",
        );
        assert!(!value.contains(':') && !value.contains('/') && !value.contains('<'));
        assert!(value.contains("_CON"));
    }

    #[test]
    fn collisions_get_a_numeric_suffix() {
        let candidate = proposal(
            Some("2026-01-05"),
            Some("Invoice"),
            &[],
            PartyRelation::None,
        );
        let composed = compose_filename(&candidate, "pdf", &["2026-01-05 Invoice.pdf"]);
        assert_eq!(composed.value, "2026-01-05 Invoice (2).pdf");
        assert_eq!(composed.collision_index, 2);
    }

    #[test]
    fn a_type_carrying_the_extension_does_not_double_it() {
        let value = name(
            &proposal(
                Some("2026-01-05"),
                Some("Invoice.pdf"),
                &[],
                PartyRelation::None,
            ),
            "pdf",
        );
        assert_eq!(value, "2026-01-05 Invoice.pdf");
    }
}
