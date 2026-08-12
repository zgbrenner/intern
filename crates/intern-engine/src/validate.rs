//! Turning a raw model reply into something safe to rename a file with.
//!
//! The goal is calibration, not timidity. Facts that can be checked literally
//! against the document are checked hard; everything else is left alone. A
//! proposal only goes to review when a *specific* thing is wrong with it, so
//! the review queue stays meaningful instead of collecting every long contract.

use crate::distill::DocumentDigest;
use crate::domain::{
    ModelProposal, PartyRelation, ProposalStatus, ReviewReason, ValidatedProposal,
    ValidationOutcome,
};
use crate::evidence::{digest_contains, digest_contains_date, is_valid_iso_date, normalize};

/// Below this self-reported confidence a proposal goes to review even when
/// every literal check passed.
pub const READY_CONFIDENCE: f32 = 0.60;
/// A description may say a little more than the filename, but only a little.
pub const MAX_DESCRIPTION_WORDS: usize = 42;
/// Share of a document type's significant words that must appear in its quote.
const TYPE_OVERLAP: f32 = 0.6;

/// Capitalised words that routinely open or punctuate a description and are not
/// claims about the document.
const GENERIC_CAPITALS: &[&str] = &[
    "a", "an", "and", "as", "at", "between", "by", "for", "from", "in", "of", "on", "the", "this",
    "to", "with", "it", "its", "their",
];

pub fn validate(candidate: ModelProposal, digest: &DocumentDigest) -> ValidationOutcome {
    let mut reasons = Vec::new();

    let (document_type, type_supported) = validate_document_type(&candidate, digest);
    if !type_supported {
        push(&mut reasons, ReviewReason::TypeUnsupported);
    }
    if document_type.is_none() && type_supported {
        push(&mut reasons, ReviewReason::TypeMissing);
    }

    let (document_date, date_role, date_supported) = validate_date(&candidate, digest);
    if !date_supported {
        push(&mut reasons, ReviewReason::DateUnsupported);
    }
    if document_date.is_none() && date_supported {
        push(&mut reasons, ReviewReason::DateMissing);
    }

    let (parties, parties_supported) = validate_parties(&candidate, digest);
    if !parties_supported {
        push(&mut reasons, ReviewReason::PartyUnsupported);
    }
    let party_relation = if parties.is_empty() {
        PartyRelation::None
    } else if candidate.party_relation == PartyRelation::Between && parties.len() < 2 {
        // "between" needs two sides; one surviving party reads as "with".
        PartyRelation::With
    } else {
        candidate.party_relation
    };

    let description = validate_description(&candidate.description, digest, &mut reasons);

    if !candidate.confidence.is_finite() || candidate.confidence < READY_CONFIDENCE {
        push(&mut reasons, ReviewReason::LowConfidence);
    }
    if candidate.needs_review {
        push(&mut reasons, ReviewReason::ModelRequestedReview);
    }
    if digest
        .parser_warnings
        .iter()
        .any(|warning| warning.field_affecting)
    {
        push(&mut reasons, ReviewReason::ParserWarning);
    }

    let status = if reasons.is_empty() {
        ProposalStatus::Ready
    } else {
        ProposalStatus::NeedsReview
    };
    ValidationOutcome {
        proposal: ValidatedProposal {
            document_type,
            document_date,
            date_role,
            parties,
            party_relation,
            description,
            confidence: candidate.confidence,
            evidence: candidate.evidence,
        },
        status,
        reasons,
    }
}

/// A document type is accepted when most of its own significant words are
/// actually in the document.
///
/// Exact substring matching would reject "First Amendment to Consulting
/// Agreement" for a document headed "FIRST AMENDMENT TO THE CONSULTING
/// AGREEMENT", which is the right answer. Checking the words against the
/// document accepts that, and still rejects "Settlement Agreement" for a
/// statement of work, because "settlement" is nowhere in it.
fn validate_document_type(
    candidate: &ModelProposal,
    digest: &DocumentDigest,
) -> (Option<String>, bool) {
    let Some(document_type) = candidate
        .document_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (None, true);
    };
    let words = normalize(document_type);
    let significant = words
        .split_whitespace()
        .filter(|word| word.len() > 2 && !GENERIC_CAPITALS.contains(word))
        .collect::<Vec<_>>();
    if significant.is_empty() {
        return (None, false);
    }
    let matched = significant
        .iter()
        .filter(|word| digest_contains(digest, word))
        .count();
    if (matched as f32) < significant.len() as f32 * TYPE_OVERLAP {
        return (None, false);
    }
    (Some(document_type.to_owned()), true)
}

/// A date is accepted when it is a real calendar date that is written, in some
/// ordinary human form, in the document.
///
/// The model's quoted line is kept for the reviewer to read but is not the
/// gate: small models paraphrase their own quotes, and a correct date should
/// not be thrown away because the sentence around it was reworded.
fn validate_date(
    candidate: &ModelProposal,
    digest: &DocumentDigest,
) -> (Option<String>, Option<crate::domain::DateRole>, bool) {
    let Some(date) = candidate
        .document_date
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (None, None, true);
    };
    if !is_valid_iso_date(date) || !digest_contains_date(digest, date) {
        return (None, None, false);
    }
    (Some(date.to_owned()), candidate.date_role, true)
}

/// A party is accepted when its name appears verbatim in the document.
fn validate_parties(candidate: &ModelProposal, digest: &DocumentDigest) -> (Vec<String>, bool) {
    let mut kept = Vec::new();
    let mut all_supported = true;
    for party in &candidate.parties {
        let party = party.trim();
        if party.is_empty() {
            all_supported = false;
            continue;
        }
        if digest_contains(digest, party) {
            if !kept.iter().any(|existing: &String| existing == party) {
                kept.push(party.to_owned());
            }
        } else {
            all_supported = false;
        }
    }
    kept.truncate(2);
    (kept, all_supported)
}

fn validate_description(
    description: &str,
    digest: &DocumentDigest,
    reasons: &mut Vec<ReviewReason>,
) -> String {
    let trimmed = description.trim();
    let mut sentence = trimmed.to_owned();
    // Keep the first sentence; a small model sometimes keeps going.
    for (index, character) in trimmed.char_indices() {
        if matches!(character, '.' | '!' | '?')
            && index + character.len_utf8() < trimmed.len()
            && !is_abbreviation_period(trimmed, index)
        {
            sentence = trimmed[..index + character.len_utf8()].to_owned();
            break;
        }
    }
    if sentence.split_whitespace().count() > MAX_DESCRIPTION_WORDS {
        let mut words = sentence
            .split_whitespace()
            .take(MAX_DESCRIPTION_WORDS)
            .collect::<Vec<_>>();
        if let Some(last) = words.last_mut() {
            *last = last.trim_end_matches([',', ';', '.', '!', '?']);
        }
        sentence = format!("{}.", words.join(" "));
        push(reasons, ReviewReason::DescriptionInvalid);
    }
    if !is_usable_sentence(&sentence) {
        push(reasons, ReviewReason::DescriptionInvalid);
    }
    if let Some(unsupported) = first_unsupported_claim(&sentence, digest) {
        let _ = unsupported;
        push(reasons, ReviewReason::DescriptionUnsupported);
    }
    sentence
}

fn is_abbreviation_period(value: &str, index: usize) -> bool {
    let before = value[..index]
        .split(|character: char| character.is_whitespace())
        .next_back()
        .unwrap_or_default();
    matches!(
        before.to_ascii_lowercase().as_str(),
        "inc"
            | "llc"
            | "ltd"
            | "corp"
            | "co"
            | "no"
            | "mr"
            | "mrs"
            | "ms"
            | "dr"
            | "jr"
            | "sr"
            | "st"
            | "u.s"
    ) || before.chars().filter(char::is_ascii_alphabetic).count() == 1
}

fn is_usable_sentence(description: &str) -> bool {
    let trimmed = description.trim();
    let Some(last) = trimmed.chars().last() else {
        return false;
    };
    if !matches!(last, '.' | '!' | '?') {
        return false;
    }
    let words = trimmed.split_whitespace().count();
    let starts_like_sentence = trimmed
        .chars()
        .find(|character| character.is_alphabetic())
        .is_some_and(char::is_uppercase);
    (6..=MAX_DESCRIPTION_WORDS).contains(&words) && starts_like_sentence
}

/// Finds the first specific claim in the description that the document does not
/// contain: a number, or a capitalised name.
///
/// Only specifics are checked. Ordinary prose the model wrote to glue the
/// sentence together is not a claim about the document and must not send an
/// otherwise good proposal to review.
fn first_unsupported_claim(description: &str, digest: &DocumentDigest) -> Option<String> {
    for (index, raw) in description.split_whitespace().enumerate() {
        let token = raw.trim_matches(|character: char| !character.is_alphanumeric());
        if token.chars().count() < 3 {
            continue;
        }
        let has_digit = token.bytes().any(|byte| byte.is_ascii_digit());
        let capitalised = index > 0
            && token.chars().next().is_some_and(char::is_uppercase)
            && !GENERIC_CAPITALS.contains(&token.to_ascii_lowercase().as_str());
        if !(has_digit || capitalised) {
            continue;
        }
        if !digest_contains(digest, token) {
            return Some(token.to_owned());
        }
    }
    None
}

fn push(reasons: &mut Vec<ReviewReason>, reason: ReviewReason) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distill::{DigestBudget, distill, source_from_text};
    use crate::domain::{DateRole, Evidence};

    fn digest_of(text: &str) -> DocumentDigest {
        distill(&source_from_text(text), DigestBudget::default())
    }

    fn proposal() -> ModelProposal {
        ModelProposal {
            document_type: Some("Statement of Work".into()),
            document_date: Some("2026-04-01".into()),
            date_role: Some(DateRole::Effective),
            parties: vec!["Acme Corporation".into(), "Vistage Worldwide, Inc.".into()],
            party_relation: PartyRelation::Between,
            description:
                "Statement of work between Acme Corporation and Vistage Worldwide, Inc. covering the 2026 CRM implementation and its fees."
                    .into(),
            confidence: 0.9,
            needs_review: false,
            evidence: Evidence {
                date: Some("effective as of April 1, 2026".into()),
                document_type: Some("STATEMENT OF WORK".into()),
                parties: vec![
                    "by and between Acme Corporation and Vistage Worldwide, Inc.".into(),
                ],
            },
        }
    }

    const DOCUMENT: &str = "STATEMENT OF WORK\n\nThis Statement of Work is effective as of April 1, 2026, \
by and between Acme Corporation and Vistage Worldwide, Inc.\n\nThe work covers the 2026 CRM implementation, \
its deliverables, and its fees.\n";

    #[test]
    fn a_fully_evidenced_proposal_is_ready() {
        let outcome = validate(proposal(), &digest_of(DOCUMENT));
        assert_eq!(
            outcome.status,
            ProposalStatus::Ready,
            "{:?}",
            outcome.reasons
        );
        assert_eq!(
            outcome.proposal.document_date.as_deref(),
            Some("2026-04-01")
        );
        assert_eq!(outcome.proposal.parties.len(), 2);
    }

    #[test]
    fn a_date_that_is_not_in_its_quote_is_rejected() {
        let mut candidate = proposal();
        candidate.document_date = Some("2026-05-30".into());
        let outcome = validate(candidate, &digest_of(DOCUMENT));
        assert!(outcome.proposal.document_date.is_none());
        assert!(outcome.reasons.contains(&ReviewReason::DateUnsupported));
    }

    #[test]
    fn a_reworded_quote_does_not_throw_away_a_date_the_document_really_has() {
        let mut candidate = proposal();
        candidate.evidence.date = Some(
            "This Statement of Work is effective as of April 1, 2026, and the parties agree".into(),
        );
        let outcome = validate(candidate, &digest_of(DOCUMENT));
        assert_eq!(
            outcome.proposal.document_date.as_deref(),
            Some("2026-04-01")
        );
    }

    #[test]
    fn a_date_the_document_never_states_is_rejected_however_it_is_quoted() {
        let mut candidate = proposal();
        candidate.document_date = Some("2026-04-02".into());
        candidate.evidence.date = Some("effective as of April 2, 2026".into());
        let outcome = validate(candidate, &digest_of(DOCUMENT));
        assert!(outcome.proposal.document_date.is_none());
        assert!(outcome.reasons.contains(&ReviewReason::DateUnsupported));
    }

    #[test]
    fn an_invented_party_is_dropped_and_flagged() {
        let mut candidate = proposal();
        candidate.parties = vec!["Acme Corporation".into(), "Northwind Traders LLC".into()];
        let outcome = validate(candidate, &digest_of(DOCUMENT));
        assert_eq!(
            outcome.proposal.parties,
            vec!["Acme Corporation".to_owned()]
        );
        assert!(outcome.reasons.contains(&ReviewReason::PartyUnsupported));
        assert_eq!(outcome.proposal.party_relation, PartyRelation::With);
    }

    #[test]
    fn a_document_type_worded_differently_from_its_quote_is_still_accepted() {
        let document = "FIRST AMENDMENT TO THE CONSULTING AGREEMENT\n\nThis amendment is effective as of April 1, 2026, by and between Acme Corporation and Vistage Worldwide, Inc.\n";
        let mut candidate = proposal();
        candidate.document_type = Some("First Amendment to Consulting Agreement".into());
        candidate.evidence.document_type =
            Some("FIRST AMENDMENT TO THE CONSULTING AGREEMENT".into());
        candidate.description =
            "First amendment to the consulting agreement between Acme Corporation and Vistage Worldwide, Inc. changing its fees.".into();
        let outcome = validate(candidate, &digest_of(document));
        assert_eq!(
            outcome.proposal.document_type.as_deref(),
            Some("First Amendment to Consulting Agreement")
        );
    }

    #[test]
    fn an_invented_document_type_is_rejected() {
        let mut candidate = proposal();
        candidate.document_type = Some("Settlement Agreement".into());
        candidate.evidence.document_type = Some("STATEMENT OF WORK".into());
        let outcome = validate(candidate, &digest_of(DOCUMENT));
        assert!(outcome.proposal.document_type.is_none());
        assert!(outcome.reasons.contains(&ReviewReason::TypeUnsupported));
    }

    #[test]
    fn a_description_asserting_an_absent_name_is_flagged() {
        let mut candidate = proposal();
        candidate.description =
            "Statement of work between Acme Corporation and Northwind Traders covering delivery."
                .into();
        let outcome = validate(candidate, &digest_of(DOCUMENT));
        assert!(
            outcome
                .reasons
                .contains(&ReviewReason::DescriptionUnsupported)
        );
    }

    #[test]
    fn ordinary_prose_in_a_description_does_not_trigger_review() {
        let outcome = validate(proposal(), &digest_of(DOCUMENT));
        assert!(
            !outcome
                .reasons
                .contains(&ReviewReason::DescriptionUnsupported)
        );
    }

    #[test]
    fn a_missing_date_sends_the_item_to_review() {
        let mut candidate = proposal();
        candidate.document_date = None;
        candidate.evidence.date = None;
        let outcome = validate(candidate, &digest_of(DOCUMENT));
        assert_eq!(outcome.status, ProposalStatus::NeedsReview);
        assert!(outcome.reasons.contains(&ReviewReason::DateMissing));
    }

    #[test]
    fn a_company_suffix_period_does_not_truncate_the_description() {
        let outcome = validate(proposal(), &digest_of(DOCUMENT));
        assert!(outcome.proposal.description.contains("CRM implementation"));
    }
}
