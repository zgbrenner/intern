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
use crate::evidence::{
    date_match_positions, digest_contains, digest_contains_date, digest_contains_loosely,
    extract_stated_dates, is_valid_iso_date, normalize,
};
use crate::infer::{infer_date_role, infer_document_type, repair_issued_relation};

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
    let original = candidate.clone();

    let (mut document_type, type_supported) = validate_document_type(&candidate, digest);
    if !type_supported {
        push(&mut reasons, ReviewReason::TypeUnsupported);
    }
    if document_type.is_none() {
        // A document with a title has a type. When the model gave none - or
        // invented one the document does not contain - the title is the
        // best-grounded answer available, and a person is asked to confirm it.
        match infer_document_type(digest) {
            Some(inferred) => {
                document_type = Some(inferred);
                push(&mut reasons, ReviewReason::TypeInferred);
            }
            None if type_supported => push(&mut reasons, ReviewReason::TypeMissing),
            None => {}
        }
    }

    let (document_date, date_role, date_supported, date_evidence_override) =
        validate_date(&candidate, digest);
    if !date_supported {
        push(&mut reasons, ReviewReason::DateUnsupported);
    }
    if document_date.is_none() && date_supported {
        push(&mut reasons, ReviewReason::DateMissing);
    }
    // The wording around the date says what kind of date it is more reliably
    // than the model's label; the model's answer stands only where the
    // document says nothing.
    let date_role = document_date
        .as_deref()
        .and_then(|date| infer_date_role(digest, date, document_type.as_deref()))
        .or(date_role);

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
    let (parties, party_relation) =
        repair_issued_relation(document_type.as_deref(), parties, party_relation, digest);

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
            evidence: {
                let mut evidence = candidate.evidence;
                if let Some(line) = date_evidence_override {
                    evidence.date = Some(line);
                }
                evidence
            },
        },
        status,
        reasons,
        candidate: original,
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
) -> (
    Option<String>,
    Option<crate::domain::DateRole>,
    bool,
    Option<String>,
) {
    let Some(date) = candidate
        .document_date
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (None, None, true, None);
    };
    if !is_valid_iso_date(date) || !digest_contains_date(digest, date) {
        return (None, None, false, None);
    }
    // A date the document states only while naming ANOTHER agreement -
    // "Issued under the Master Services Agreement dated June 2, 2023" - is
    // that other document's date, and must never become this filename's date.
    // The model is told this and usually complies, but greedy decoding is not
    // hardware-deterministic and the two candidates can sit a rounding error
    // apart, so the guarantee lives here, where nothing wobbles: if the
    // document states exactly one other date on an effective/commencement
    // line, that date is the answer; otherwise the document goes to review.
    let lines: Vec<&str> = digest
        .segments
        .iter()
        .flat_map(|segment| segment.lines())
        .collect();
    let mut stated = false;
    let mut tainted = true;
    for line in &lines {
        let normalized = normalize(line);
        for position in date_match_positions(date, &normalized) {
            stated = true;
            if !reference_introduced(&normalized, position) {
                tainted = false;
            }
        }
    }
    let tainted = stated && tainted;
    if tainted {
        let mut alternates: Vec<(String, String)> = Vec::new();
        for line in &lines {
            let normalized = normalize(line);
            if !EFFECTIVE_CUES.iter().any(|cue| normalized.contains(cue)) {
                continue;
            }
            for found in extract_stated_dates(line) {
                if found == *date || alternates.iter().any(|(existing, _)| *existing == found) {
                    continue;
                }
                let clean = date_match_positions(&found, &normalized)
                    .iter()
                    .any(|&position| !reference_introduced(&normalized, position));
                if clean {
                    alternates.push((found, line.trim().to_owned()));
                }
            }
        }
        if let [(alternate, line)] = alternates.as_slice() {
            return (
                Some(alternate.clone()),
                Some(crate::domain::DateRole::Effective),
                true,
                Some(line.clone()),
            );
        }
        return (None, None, false, None);
    }
    (Some(date.to_owned()), candidate.date_role, true, None)
}

/// Whether the wording immediately before a date's occurrence marks it as
/// ANOTHER document's date. Judged per occurrence, not per line, because one
/// line can state two dates in two roles: "...Amendment to the Consulting
/// Agreement dated September 1, 2020, is entered into as of September 14,
/// 2025" references the first date and owns the second.
///
/// "dated" directly before the occurrence is the referencing construction -
/// "the Master Services Agreement dated June 2, 2023" - unless the naming
/// phrase begins with "this", which is how a document dates itself.
pub(crate) fn reference_introduced(normalized: &str, position: usize) -> bool {
    const NEAR: usize = 12;
    const WIDE: usize = 48;
    fn window(normalized: &str, position: usize, span: usize) -> &str {
        let mut start = position.saturating_sub(span);
        while !normalized.is_char_boundary(start) {
            start -= 1;
        }
        &normalized[start..position]
    }
    if window(normalized, position, NEAR).contains("dated") {
        // "dated" only references another document when a document noun
        // introduces it - "the Master Services Agreement dated June 2, 2023".
        // A bare "Dated January 8, 2025" on a title block is the document
        // dating itself, and "This Agreement dated ..." is too.
        let wide = window(normalized, position, WIDE);
        let names_a_document = ["agreement", "contract", "order", "amendment", "memorandum"]
            .iter()
            .any(|noun| wide.contains(noun));
        return names_a_document && !wide.contains("this ");
    }
    ["issued under", "pursuant to", "as amended", "amending "]
        .iter()
        .any(|cue| window(normalized, position, WIDE).contains(cue))
}

const EFFECTIVE_CUES: &[&str] = &[
    "effective",
    "commencement",
    "commencing",
    "start date",
    "in force",
    "entered into",
];

/// A party is accepted when its name appears in the document.
///
/// Verbatim first; failing that, with punctuation disregarded, because
/// "Vistage Worldwide Inc" for a document that writes "Vistage Worldwide,
/// Inc." is the same company and the corpus showed the model dropping the
/// comma often enough that a real name was reaching review over it. Words are
/// never loosened: a name the document does not contain is still rejected.
fn validate_parties(candidate: &ModelProposal, digest: &DocumentDigest) -> (Vec<String>, bool) {
    let mut kept = Vec::new();
    let mut all_supported = true;
    for party in &candidate.parties {
        let party = party.trim();
        if party.is_empty() {
            all_supported = false;
            continue;
        }
        if digest_contains(digest, party) || digest_contains_loosely(digest, party) {
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
        .unwrap_or_default()
        .trim_start_matches(|character: char| !character.is_alphanumeric());
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
            | "e.g"
            | "i.e"
            | "etc"
            | "vs"
            | "approx"
            | "dept"
            | "assoc"
            | "bros"
            | "ave"
            | "blvd"
            | "ste"
            | "esq"
            | "ph.d"
            | "m.d"
            | "j.d"
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
        if !claim_is_supported(digest, token) {
            return Some(token.to_owned());
        }
    }
    None
}

/// Whether one specific claim is in the document, allowing for the ways a
/// sentence reshapes a fact it is quoting.
///
/// A possessive ("Acme's" for a document that writes "Acme"), a thousands
/// separator the document omits ("248,000" for "248000"), or a hyphen the
/// model added between two words that the document keeps apart are all the
/// same fact. Words themselves are never changed; a name that is not in the
/// document is still an unsupported claim.
fn claim_is_supported(digest: &DocumentDigest, token: &str) -> bool {
    if digest_contains(digest, token) {
        return true;
    }
    let mut variants: Vec<String> = Vec::new();
    for possessive in ["'s", "\u{2019}s"] {
        if let Some(stem) = token.strip_suffix(possessive) {
            variants.push(stem.to_owned());
        }
    }
    if token.bytes().any(|byte| byte.is_ascii_digit()) && token.contains(',') {
        variants.push(token.replace(',', ""));
    }
    if token.contains('-') {
        variants.push(token.replace('-', " "));
    }
    variants
        .iter()
        .any(|variant| variant.chars().count() >= 3 && digest_contains(digest, variant))
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

    const REFERENCING_DOCUMENT: &str = "STATEMENT OF WORK

Issued under the Master Services Agreement dated June 2, 2023

Capitalized terms have the meanings given to them in the Master Services
Agreement dated June 2, 2023 between the same parties.

This Statement of Work is effective as of April 1, 2026 and continues
by and between Acme Corporation and Vistage Worldwide, Inc.
";

    /// Greedy decoding is not hardware-deterministic, and the corpus showed a
    /// run filing this exact shape under the referenced agreement's date. The
    /// guard is code so it cannot wobble.
    #[test]
    fn a_date_stated_only_beside_another_agreements_name_is_replaced_by_the_effective_date() {
        let mut candidate = proposal();
        candidate.document_date = Some("2023-06-02".into());
        candidate.evidence.date =
            Some("Issued under the Master Services Agreement dated June 2, 2023".into());
        let outcome = validate(candidate, &digest_of(REFERENCING_DOCUMENT));
        assert_eq!(
            outcome.proposal.document_date.as_deref(),
            Some("2026-04-01"),
            "{:?}",
            outcome.reasons
        );
        assert_eq!(outcome.proposal.date_role, Some(DateRole::Effective));
        assert_eq!(
            outcome.proposal.evidence.date.as_deref(),
            Some("This Statement of Work is effective as of April 1, 2026 and continues"),
            "the evidence must be the line the substituted date actually stands on"
        );
        assert!(!outcome.reasons.contains(&ReviewReason::DateUnsupported));
    }

    #[test]
    fn a_referenced_date_with_two_effective_candidates_goes_to_review_not_to_a_guess() {
        let document = REFERENCING_DOCUMENT.replace(
            "and continues",
            "and continues
with services commencing on October 1, 2026",
        );
        let mut candidate = proposal();
        candidate.document_date = Some("2023-06-02".into());
        let outcome = validate(candidate, &digest_of(&document));
        assert!(outcome.proposal.document_date.is_none());
        assert!(outcome.reasons.contains(&ReviewReason::DateUnsupported));
    }

    /// One line, two dates, two roles: the referenced contract's and the
    /// amendment's own. The first guard version tainted whole lines and threw
    /// away the amendment's real date; this pins the per-occurrence judgment.
    #[test]
    fn an_amendments_own_date_survives_sharing_a_line_with_the_referenced_contracts() {
        let document = "FIRST AMENDMENT

This First Amendment to the Consulting Agreement dated September 1, 2020,
is entered into as of September 14, 2025 by Acme Corporation and
Vistage Worldwide, Inc. The work covers the 2026 CRM implementation.
";
        let mut candidate = proposal();
        candidate.document_date = Some("2025-09-14".into());
        candidate.date_role = Some(DateRole::Amendment);
        candidate.evidence.date = Some("is entered into as of September 14, 2025".into());
        let outcome = validate(candidate, &digest_of(document));
        assert_eq!(
            outcome.proposal.document_date.as_deref(),
            Some("2025-09-14"),
            "{:?}",
            outcome.reasons
        );
        assert_eq!(outcome.proposal.date_role, Some(DateRole::Amendment));

        // And the mirror image: choosing the referenced contract's date is
        // redirected to the amendment's own.
        let mut candidate = proposal();
        candidate.document_date = Some("2020-09-01".into());
        let outcome = validate(candidate, &digest_of(document));
        assert_eq!(
            outcome.proposal.document_date.as_deref(),
            Some("2025-09-14"),
            "{:?}",
            outcome.reasons
        );
    }

    /// mixed-signature.pdf dates itself with a bare title-block line, and the
    /// first per-occurrence guard read its "Dated" as a reference and threw
    /// the date away.
    #[test]
    fn a_bare_title_block_dated_line_is_the_documents_own_date() {
        let document = "SERVICES AGREEMENT
Dated January 8, 2025

Acme Corporation and Vistage Worldwide, Inc.

The work covers the 2026 CRM implementation, its deliverables, and fees.
";
        let mut candidate = proposal();
        candidate.document_date = Some("2025-01-08".into());
        candidate.date_role = Some(DateRole::Execution);
        candidate.evidence.date = Some("Dated January 8, 2025".into());
        let outcome = validate(candidate, &digest_of(document));
        assert_eq!(
            outcome.proposal.document_date.as_deref(),
            Some("2025-01-08"),
            "{:?}",
            outcome.reasons
        );
    }

    #[test]
    fn a_date_the_document_also_states_on_its_own_line_is_left_alone() {
        // The chosen date appears both beside the reference and on a plain
        // line of its own, so nothing here says it belongs to another document.
        let document = format!(
            "{REFERENCING_DOCUMENT}
Countersigned June 2, 2023.
"
        );
        let mut candidate = proposal();
        candidate.document_date = Some("2023-06-02".into());
        candidate.date_role = Some(DateRole::Execution);
        let outcome = validate(candidate, &digest_of(&document));
        assert_eq!(
            outcome.proposal.document_date.as_deref(),
            Some("2023-06-02")
        );
        assert_eq!(outcome.proposal.date_role, Some(DateRole::Execution));
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

    /// An invented type is rejected, and the document's own title stands in
    /// for it so the reviewer is shown the right name rather than "Document".
    #[test]
    fn an_invented_document_type_is_rejected_and_the_title_offered_instead() {
        let mut candidate = proposal();
        candidate.document_type = Some("Settlement Agreement".into());
        candidate.evidence.document_type = Some("STATEMENT OF WORK".into());
        let outcome = validate(candidate, &digest_of(DOCUMENT));
        assert_eq!(
            outcome.proposal.document_type.as_deref(),
            Some("Statement of Work")
        );
        assert!(outcome.reasons.contains(&ReviewReason::TypeUnsupported));
        assert!(outcome.reasons.contains(&ReviewReason::TypeInferred));
        assert_eq!(outcome.status, ProposalStatus::NeedsReview);

        // With no usable title either, the type stays empty.
        let mut candidate = proposal();
        candidate.document_type = Some("Settlement Agreement".into());
        let outcome = validate(
            candidate,
            &digest_of(
                "Some notes.\n\nThis Statement of Work is effective as of April 1, 2026, by and between Acme Corporation and Vistage Worldwide, Inc.\n",
            ),
        );
        assert!(outcome.proposal.document_type.is_none());
        assert!(outcome.reasons.contains(&ReviewReason::TypeUnsupported));
        assert!(!outcome.reasons.contains(&ReviewReason::TypeInferred));
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

    /// The corpus minutes and journal fixtures got no type from the model and
    /// were named "<date> Document". Their titles name what they are.
    #[test]
    fn a_missing_type_is_taken_from_the_title_and_flagged_for_a_person() {
        let document = "# Quarterly Operations Review\n\n**Date:** May 7, 2025\n\nThe Fictional Meridian Committee reviewed inventory, safety, and the next quarterly plan.\n";
        let candidate = ModelProposal {
            document_type: None,
            document_date: Some("2025-05-07".into()),
            date_role: Some(DateRole::Effective),
            parties: Vec::new(),
            party_relation: PartyRelation::None,
            description: "Quarterly operations review minutes covering inventory, safety, and the next quarterly plan for the committee.".into(),
            confidence: 0.85,
            needs_review: false,
            evidence: Evidence {
                date: Some("**Date:** May 7, 2025".into()),
                document_type: None,
                parties: Vec::new(),
            },
        };
        let outcome = validate(candidate, &digest_of(document));
        assert_eq!(
            outcome.proposal.document_type.as_deref(),
            Some("Quarterly Operations Review")
        );
        assert_eq!(outcome.status, ProposalStatus::NeedsReview);
        assert!(outcome.reasons.contains(&ReviewReason::TypeInferred));
        assert!(!outcome.reasons.contains(&ReviewReason::TypeMissing));
        // The bare "Date:" label says nothing; a review is something issued.
        assert_eq!(outcome.proposal.date_role, Some(DateRole::Issuance));
    }

    #[test]
    fn the_date_role_follows_the_documents_wording_not_the_models_habit() {
        let document = "INVOICE\n\nAcme Corporation\nInvoice Number: INV-7741\nInvoice Date: January 5, 2026\nPayment Due Date: February 4, 2026\nBill To: Vistage Worldwide, Inc.\nAnnual platform subscription for the 2026 term, $42,000.\n";
        let candidate = ModelProposal {
            document_type: Some("Invoice".into()),
            document_date: Some("2026-01-05".into()),
            // The corpus habit: everything is "effective".
            date_role: Some(DateRole::Effective),
            parties: vec![
                "Vistage Worldwide, Inc.".into(),
                "Acme Corporation".into(),
            ],
            party_relation: PartyRelation::Between,
            description: "Invoice INV-7741 from Acme Corporation to Vistage Worldwide, Inc. for the 2026 annual platform subscription of $42,000.".into(),
            confidence: 0.9,
            needs_review: false,
            evidence: Evidence {
                date: Some("Invoice Date: January 5, 2026".into()),
                document_type: Some("INVOICE".into()),
                parties: vec!["Bill To: Vistage Worldwide, Inc.".into()],
            },
        };
        let outcome = validate(candidate, &digest_of(document));
        assert_eq!(outcome.proposal.date_role, Some(DateRole::Invoice));
        // Two parties and "between" on an invoice: the bill-to line names the
        // customer, so the issuer stands alone as "from".
        assert_eq!(
            outcome.proposal.parties,
            vec!["Acme Corporation".to_owned()]
        );
        assert_eq!(outcome.proposal.party_relation, PartyRelation::From);
        assert_eq!(
            outcome.status,
            ProposalStatus::Ready,
            "{:?}",
            outcome.reasons
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

    /// The model drops the comma from "Vistage Worldwide, Inc." often enough
    /// that a correct party was reaching review, and the filename lost the
    /// name. Punctuation is typography; the words are still checked.
    #[test]
    fn a_party_that_differs_from_the_document_only_in_punctuation_is_kept() {
        let mut candidate = proposal();
        candidate.parties = vec!["Acme Corporation".into(), "Vistage Worldwide Inc".into()];
        let outcome = validate(candidate, &digest_of(DOCUMENT));
        assert_eq!(
            outcome.proposal.parties,
            vec![
                "Acme Corporation".to_owned(),
                "Vistage Worldwide Inc".to_owned()
            ]
        );
        assert!(!outcome.reasons.contains(&ReviewReason::PartyUnsupported));
        assert_eq!(outcome.proposal.party_relation, PartyRelation::Between);

        let mut candidate = proposal();
        candidate.parties = vec!["Vistage Worldwide LLC".into()];
        let outcome = validate(candidate, &digest_of(DOCUMENT));
        assert!(outcome.proposal.parties.is_empty());
        assert!(outcome.reasons.contains(&ReviewReason::PartyUnsupported));
    }

    #[test]
    fn a_description_may_reshape_a_fact_it_quotes_but_not_invent_one() {
        let document = format!(
            "{DOCUMENT}\nThe total fee is $248000 payable to Acme.\nWork begins in Ridgeline Cartography's office.\n"
        );
        let mut candidate = proposal();
        candidate.description = "Statement of work between Acme Corporation and Vistage Worldwide, Inc. for Ridgeline-Cartography's 2026 CRM implementation at a fee of $248,000.".into();
        let outcome = validate(candidate, &digest_of(&document));
        assert!(
            !outcome
                .reasons
                .contains(&ReviewReason::DescriptionUnsupported),
            "{:?}",
            outcome.reasons
        );

        let mut candidate = proposal();
        candidate.description = "Statement of work between Acme Corporation and Vistage Worldwide, Inc. for Northwind's 2026 CRM implementation.".into();
        let outcome = validate(candidate, &digest_of(&document));
        assert!(
            outcome
                .reasons
                .contains(&ReviewReason::DescriptionUnsupported)
        );
    }

    #[test]
    fn an_abbreviation_inside_the_sentence_does_not_end_it() {
        let mut candidate = proposal();
        candidate.description =
            "Statement of work between Acme Corporation and Vistage Worldwide, Inc. covering deliverables (e.g. the 2026 CRM implementation) and fees."
                .into();
        let outcome = validate(candidate, &digest_of(DOCUMENT));
        assert!(
            outcome.proposal.description.ends_with("and fees."),
            "{}",
            outcome.proposal.description
        );
    }
}
