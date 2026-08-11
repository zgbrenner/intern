use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

use crate::DocumentPacket;

pub fn normalize_for_evidence(value: &str) -> String {
    let normalized = value.nfkc().case_fold().map(|character| {
        match character {
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            other => other,
        }
    });

    let mut result = String::new();
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

pub fn packet_contains(packet: &DocumentPacket, evidence: &str) -> bool {
    let evidence = normalize_for_evidence(evidence);
    !evidence.is_empty()
        && packet.text_segments.iter().any(|segment| normalize_for_evidence(segment).contains(&evidence))
}

pub(crate) fn evidence_supports(packet: &DocumentPacket, evidence: &str, field: &str) -> bool {
    let normalized_evidence = normalize_for_evidence(evidence);
    let normalized_field = normalize_for_evidence(field);
    !normalized_field.is_empty()
        && normalized_evidence.contains(&normalized_field)
        && packet.text_segments.iter().any(|segment| {
            normalize_for_evidence(segment).contains(&normalized_evidence)
        })
}
