use crate::{
    evidence_supports, packet_contains, ModelProposal, ProposalStatus, ReviewReason,
    ValidatedProposal, ValidationOutcome, DocumentPacket,
};

const READY_CONFIDENCE: f32 = 0.86;

pub fn validate_proposal(candidate: ModelProposal, packet: &DocumentPacket) -> ValidationOutcome {
    let mut reasons = Vec::new();
    let mut document_date = candidate.document_date.clone();
    let mut date_kind = document_date.as_ref().and(candidate.date_kind.clone());
    if let Some(date) = document_date.as_deref() {
        if !valid_iso_date(date) {
            document_date = None;
            date_kind = None;
            push_reason(&mut reasons, ReviewReason::InvalidDate);
        } else if !candidate.evidence.date.as_deref().is_some_and(|evidence| {
            packet_contains(packet, evidence) && date_is_in_evidence(date, evidence)
        }) {
            document_date = None;
            date_kind = None;
            push_reason(&mut reasons, ReviewReason::EvidenceMissing);
        }
    }

    let document_type = supported_optional_field(
        candidate.document_type.as_deref(),
        candidate.evidence.document_type.as_deref(),
        packet,
        &mut reasons,
    );
    let filename_subject = supported_optional_field(
        candidate.filename_subject.as_deref(),
        candidate.evidence.subject.as_deref(),
        packet,
        &mut reasons,
    );

    let parties = candidate.parties.iter().filter_map(|party| {
        let supported = candidate.evidence.parties.iter().any(|evidence| {
            evidence_supports(packet, evidence, party)
        });
        if supported {
            Some(party.clone())
        } else {
            push_reason(&mut reasons, ReviewReason::EvidenceMissing);
            None
        }
    }).collect();

    let description = first_sentence(&candidate.description, &mut reasons);
    if !candidate.confidence.is_finite() || candidate.confidence < READY_CONFIDENCE {
        push_reason(&mut reasons, ReviewReason::LowConfidence);
    }
    if candidate.needs_review || !candidate.review_reasons.is_empty() {
        push_reason(&mut reasons, ReviewReason::ModelRequestedReview);
    }
    if packet.parser_warnings.iter().any(|warning| warning.field_affecting) {
        push_reason(&mut reasons, ReviewReason::ParserWarning);
    }

    let status = if reasons.is_empty() { ProposalStatus::Ready } else { ProposalStatus::NeedsReview };
    ValidationOutcome {
        proposal: ValidatedProposal {
            document_date,
            date_kind,
            document_type,
            filename_subject,
            parties,
            description,
            confidence: candidate.confidence,
            evidence: candidate.evidence,
        },
        status,
        reasons,
    }
}

fn supported_optional_field(
    field: Option<&str>,
    evidence: Option<&str>,
    packet: &DocumentPacket,
    reasons: &mut Vec<ReviewReason>,
) -> Option<String> {
    let field = field?.trim();
    if !field.is_empty() && evidence.is_some_and(|value| evidence_supports(packet, value, field)) {
        Some(field.to_owned())
    } else {
        push_reason(reasons, ReviewReason::EvidenceMissing);
        None
    }
}

fn first_sentence(description: &str, reasons: &mut Vec<ReviewReason>) -> String {
    let trimmed = description.trim();
    for (index, character) in trimmed.char_indices() {
        if matches!(character, '.' | '!' | '?') && index + character.len_utf8() < trimmed.len() {
            push_reason(reasons, ReviewReason::DescriptionTooLong);
            return trimmed[..index + character.len_utf8()].to_owned();
        }
    }
    trimmed.to_owned()
}

fn push_reason(reasons: &mut Vec<ReviewReason>, reason: ReviewReason) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

fn valid_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    if bytes.iter().enumerate().any(|(index, byte)| {
        index != 4 && index != 7 && !byte.is_ascii_digit()
    }) {
        return false;
    }
    let parse = |range: std::ops::Range<usize>| value[range].parse::<u32>().ok();
    let (Some(year), Some(month), Some(day)) = (parse(0..4), parse(5..7), parse(8..10)) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=max_day).contains(&day)
}

fn date_is_in_evidence(date: &str, evidence: &str) -> bool {
    let year = &date[0..4];
    let month = &date[5..7];
    let day = &date[8..10];
    let month_number = month.parse::<usize>().unwrap_or(0);
    let month_unpadded = month.trim_start_matches('0');
    let month_name = [
        "", "january", "february", "march", "april", "may", "june",
        "july", "august", "september", "october", "november", "december",
    ][month_number];
    let short_month = &month_name[..3];
    let day_unpadded = day.trim_start_matches('0');
    let normalized = crate::normalize_for_evidence(evidence);
    [
        date.to_owned(),
        format!("{month}/{day}/{year}"),
        format!("{month_unpadded}/{day_unpadded}/{year}"),
        format!("{month}-{day}-{year}"),
        format!("{month_name} {day_unpadded}, {year}"),
        format!("{short_month} {day_unpadded}, {year}"),
        format!("{day_unpadded} {month_name} {year}"),
        format!("{day_unpadded} {short_month} {year}"),
    ].iter().any(|form| normalized.contains(form))
}
