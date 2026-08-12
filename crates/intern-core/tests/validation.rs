use intern_core::{
    DateKind, Evidence, ExtractedDocument, ModelProposal, ParserWarning, ProposalStatus,
    ReviewReason, ValidatedProposal, build_document_packet, compose_filename, packet_contains,
    validate_proposal,
};

fn packet(text: &str) -> intern_core::DocumentPacket {
    build_document_packet(
        ExtractedDocument {
            text: text.into(),
            parser_warnings: Vec::new(),
        },
        false,
    )
}

fn proposal() -> ModelProposal {
    ModelProposal {
        document_date: None,
        date_kind: None,
        document_type: Some("Agreement".into()),
        filename_subject: Some("Acme Corporation".into()),
        parties: vec!["Acme Corporation".into()],
        description: "An agreement with Acme Corporation.".into(),
        confidence: 0.86,
        needs_review: false,
        review_reasons: Vec::new(),
        evidence: Evidence {
            date: None,
            document_type: Some("AGREEMENT".into()),
            subject: Some("Acme Corporation".into()),
            parties: vec!["Acme Corporation".into()],
        },
    }
}

fn validated(date: Option<&str>, kind: Option<&str>, subject: Option<&str>) -> ValidatedProposal {
    ValidatedProposal {
        document_date: date.map(str::to_owned),
        date_kind: date.map(|_| DateKind::Signed),
        document_type: kind.map(str::to_owned),
        filename_subject: subject.map(str::to_owned),
        parties: Vec::new(),
        description: "Summary".into(),
        confidence: 0.99,
        evidence: Evidence::default(),
    }
}

#[test]
fn unsupported_date_is_removed_and_requires_review() {
    let mut candidate = proposal();
    candidate.document_date = Some("2024-04-12".into());
    candidate.date_kind = Some(DateKind::Effective);
    candidate.evidence.date = Some("effective April 12, 2024".into());

    let outcome = validate_proposal(candidate, &packet("Signed by Acme Corporation."));

    assert_eq!(outcome.proposal.document_date, None);
    assert_eq!(outcome.status, ProposalStatus::NeedsReview);
    assert!(outcome.reasons.contains(&ReviewReason::EvidenceMissing));
}

#[test]
fn impossible_date_is_removed() {
    let mut candidate = proposal();
    candidate.document_date = Some("2024-02-30".into());
    candidate.date_kind = Some(DateKind::Signed);
    candidate.evidence.date = Some("2024-02-30".into());
    let outcome = validate_proposal(candidate, &packet("Agreement 2024-02-30 Acme Corporation"));
    assert_eq!(outcome.proposal.document_date, None);
    assert!(outcome.reasons.contains(&ReviewReason::InvalidDate));
}

#[test]
fn date_evidence_for_a_different_date_does_not_support_the_proposal() {
    let mut candidate = proposal();
    candidate.document_date = Some("2024-04-12".into());
    candidate.date_kind = Some(DateKind::Effective);
    candidate.evidence.date = Some("effective April 13, 2024".into());
    let outcome = validate_proposal(
        candidate,
        &packet("Agreement with Acme Corporation, effective April 13, 2024"),
    );
    assert_eq!(outcome.proposal.document_date, None);
    assert!(outcome.reasons.contains(&ReviewReason::EvidenceMissing));
}

#[test]
fn exact_natural_language_date_evidence_is_ready() {
    let mut candidate = proposal();
    candidate.document_date = Some("2024-04-12".into());
    candidate.date_kind = Some(DateKind::Effective);
    candidate.evidence.date = Some("effective April 12, 2024".into());
    let outcome = validate_proposal(
        candidate,
        &packet("Agreement with Acme Corporation, effective April 12, 2024"),
    );
    assert_eq!(
        outcome.proposal.document_date.as_deref(),
        Some("2024-04-12")
    );
    assert_eq!(outcome.status, ProposalStatus::Ready);
}

#[test]
fn evidence_matching_normalizes_unicode_case_quotes_and_whitespace() {
    let mut candidate = proposal();
    candidate.filename_subject = Some("ＡＣＭＥ “North”".into());
    candidate.parties = vec!["ＡＣＭＥ “North”".into()];
    candidate.description = "An agreement for ＡＣＭＥ “North”.".into();
    candidate.evidence.subject = Some("acme \"north\"".into());
    candidate.evidence.parties = vec!["acme \"north\"".into()];
    let outcome = validate_proposal(
        candidate,
        &packet("AGREEMENT\n\nAcme   “North” signed this document."),
    );
    assert_eq!(outcome.status, ProposalStatus::Ready);
}

#[test]
fn evidence_matching_uses_full_unicode_case_folding() {
    let mut candidate = proposal();
    candidate.filename_subject = Some("STRASSE".into());
    candidate.parties = vec!["STRASSE".into()];
    candidate.description = "An agreement for STRASSE.".into();
    candidate.evidence.subject = Some("Straße".into());
    candidate.evidence.parties = vec!["Straße".into()];
    let outcome = validate_proposal(candidate, &packet("Agreement for Straße"));
    assert_eq!(outcome.status, ProposalStatus::Ready);
}

#[test]
fn unsupported_party_is_removed_without_fuzzy_matching() {
    let mut candidate = proposal();
    candidate.parties.push("Globex Corporation".into());
    candidate.evidence.parties.push("Globex Corp".into());
    let outcome = validate_proposal(candidate, &packet("Agreement with Acme Corporation"));
    assert_eq!(outcome.proposal.parties, vec!["Acme Corporation"]);
    assert_eq!(outcome.status, ProposalStatus::NeedsReview);
    assert!(outcome.reasons.contains(&ReviewReason::EvidenceMissing));
}

#[test]
fn unsupported_subject_is_removed_without_fuzzy_matching() {
    let mut candidate = proposal();
    candidate.filename_subject = Some("Acme Corporation North".into());
    candidate.evidence.subject = Some("Acme Corporation".into());
    let outcome = validate_proposal(candidate, &packet("Agreement with Acme Corporation"));
    assert_eq!(outcome.proposal.filename_subject, None);
    assert!(outcome.reasons.contains(&ReviewReason::EvidenceMissing));
}

#[test]
fn multiple_sentence_description_is_reduced_and_reviewed() {
    let mut candidate = proposal();
    candidate.description = "First sentence. Unsupported second sentence.".into();
    let outcome = validate_proposal(
        candidate,
        &packet("Agreement with Acme Corporation. First sentence."),
    );
    assert_eq!(outcome.proposal.description, "First sentence.");
    assert_eq!(outcome.status, ProposalStatus::NeedsReview);
    assert!(outcome.reasons.contains(&ReviewReason::DescriptionTooLong));
}

#[test]
fn description_over_thirty_words_is_bounded_and_reviewed() {
    let mut candidate = proposal();
    candidate.description = format!("{}.", vec!["agreement"; 31].join(" "));
    let outcome = validate_proposal(candidate, &packet("Agreement with Acme Corporation."));

    assert!(outcome.proposal.description.split_whitespace().count() <= 30);
    assert_eq!(outcome.status, ProposalStatus::NeedsReview);
    assert!(outcome.reasons.contains(&ReviewReason::DescriptionTooLong));
}

#[test]
fn description_without_a_complete_sentence_is_never_ready() {
    let mut candidate = proposal();
    candidate.description = "Agreement with Acme Corporation".into();
    let outcome = validate_proposal(candidate, &packet("Agreement with Acme Corporation."));

    assert_eq!(outcome.status, ProposalStatus::NeedsReview);
    assert!(outcome.reasons.contains(&ReviewReason::DescriptionInvalid));
}

#[test]
fn unsupported_party_or_date_in_description_forces_review() {
    let mut party = proposal();
    party.description = "An agreement between Acme Corporation and Globex Corporation.".into();
    let party_outcome = validate_proposal(party, &packet("Agreement with Acme Corporation."));
    assert_eq!(party_outcome.status, ProposalStatus::NeedsReview);
    assert!(
        party_outcome
            .reasons
            .contains(&ReviewReason::DescriptionUnsupported)
    );

    let mut date = proposal();
    date.description = "An agreement effective in 2037 for Acme Corporation.".into();
    let date_outcome = validate_proposal(date, &packet("Agreement with Acme Corporation."));
    assert_eq!(date_outcome.status, ProposalStatus::NeedsReview);
    assert!(
        date_outcome
            .reasons
            .contains(&ReviewReason::DescriptionUnsupported)
    );
}

#[test]
fn due_date_is_removed_and_never_ready_even_with_literal_evidence() {
    let mut candidate = proposal();
    candidate.document_date = Some("2024-04-12".into());
    candidate.date_kind = Some(DateKind::Due);
    candidate.evidence.date = Some("due April 12, 2024".into());
    let outcome = validate_proposal(
        candidate,
        &packet("Agreement with Acme Corporation, due April 12, 2024."),
    );

    assert_eq!(outcome.proposal.document_date, None);
    assert_eq!(outcome.proposal.date_kind, None);
    assert_eq!(outcome.status, ProposalStatus::NeedsReview);
    assert!(outcome.reasons.contains(&ReviewReason::InvalidDate));

    let mut kind_only = proposal();
    kind_only.date_kind = Some(DateKind::Due);
    let kind_only_outcome =
        validate_proposal(kind_only, &packet("Agreement with Acme Corporation."));
    assert_eq!(kind_only_outcome.status, ProposalStatus::NeedsReview);
    assert!(
        kind_only_outcome
            .reasons
            .contains(&ReviewReason::InvalidDate)
    );
}

#[test]
fn confidence_boundary_is_exact() {
    let mut below = proposal();
    below.confidence = 0.859;
    let low = validate_proposal(below, &packet("Agreement with Acme Corporation"));
    assert_eq!(low.status, ProposalStatus::NeedsReview);
    assert!(low.reasons.contains(&ReviewReason::LowConfidence));

    let ready = validate_proposal(proposal(), &packet("Agreement with Acme Corporation"));
    assert_eq!(ready.status, ProposalStatus::Ready);
}

#[test]
fn model_flag_and_field_affecting_parser_warning_prevent_ready() {
    let mut flagged = proposal();
    flagged.needs_review = true;
    assert_eq!(
        validate_proposal(flagged, &packet("Agreement with Acme Corporation")).status,
        ProposalStatus::NeedsReview
    );

    let warned = build_document_packet(
        ExtractedDocument {
            text: "Agreement with Acme Corporation".into(),
            parser_warnings: vec![ParserWarning {
                code: "ocr_text_incomplete".into(),
                field_affecting: true,
            }],
        },
        false,
    );
    let outcome = validate_proposal(proposal(), &warned);
    assert!(outcome.reasons.contains(&ReviewReason::ParserWarning));

    let informational = build_document_packet(
        ExtractedDocument {
            text: "Agreement with Acme Corporation".into(),
            parser_warnings: vec![ParserWarning {
                code: "image_omitted".into(),
                field_affecting: false,
            }],
        },
        false,
    );
    assert_eq!(
        validate_proposal(proposal(), &informational).status,
        ProposalStatus::Ready
    );
}

#[test]
fn packet_budgets_text_and_image_requests_from_both_ends() {
    let text = format!("{}{}", "A".repeat(16_000), "Z".repeat(10_000));
    let text_only = build_document_packet(
        ExtractedDocument {
            text: text.clone(),
            parser_warnings: vec![],
        },
        false,
    );
    assert_eq!(
        text_only
            .text_segments
            .iter()
            .map(|value| value.chars().count())
            .sum::<usize>(),
        22_000
    );
    assert_eq!(
        text_only.text_segments[0]
            .chars()
            .filter(|c| *c == 'A')
            .count(),
        14_000
    );
    assert_eq!(
        text_only.text_segments[1]
            .chars()
            .filter(|c| *c == 'Z')
            .count(),
        8_000
    );

    let imaged = build_document_packet(
        ExtractedDocument {
            text,
            parser_warnings: vec![],
        },
        true,
    );
    assert_eq!(
        imaged
            .text_segments
            .iter()
            .map(|value| value.chars().count())
            .sum::<usize>(),
        12_000
    );
    assert_eq!(
        imaged.text_segments[0]
            .chars()
            .filter(|c| *c == 'A')
            .count(),
        8_000
    );
    assert_eq!(
        imaged.text_segments[1]
            .chars()
            .filter(|c| *c == 'Z')
            .count(),
        4_000
    );
}

#[test]
fn packet_evidence_cannot_match_across_the_head_tail_gap() {
    let text = format!(
        "{}Acme{} Corporation{}",
        "A".repeat(13_996),
        "M".repeat(5_000),
        "Z".repeat(7_988)
    );
    let packet = build_document_packet(
        ExtractedDocument {
            text,
            parser_warnings: vec![],
        },
        false,
    );
    assert!(
        packet
            .text
            .contains("Acme\n\n[... DOCUMENT GAP ...]\n\n Corporation")
    );
    assert!(!packet_contains(&packet, "Acme Corporation"));
}

#[test]
fn composer_uses_iso_date_type_subject_and_collision_suffix() {
    let name = compose_filename(
        &validated(
            Some("2024-04-12"),
            Some("Employment Agreement"),
            Some("John Smith"),
        ),
        "PDF",
        &["2024-04-12 - Employment Agreement - John Smith.pdf"],
    );
    assert_eq!(
        name.value,
        "2024-04-12 - Employment Agreement - John Smith (2).pdf"
    );
}

#[test]
fn composer_is_windows_safe_and_deterministic() {
    let name = compose_filename(
        &validated(
            None,
            Some("CON."),
            Some("A\u{202e}B\u{0001}<>:\"/\\|?*..pdf"),
        ),
        ".PDF",
        &[],
    );
    assert_eq!(name.value, "_CON - AB.pdf");
}

#[test]
fn composer_escapes_every_windows_device_family() {
    for reserved in [
        "PRN", "AUX.txt", "NUL", "COM1", "COM9.log", "LPT1", "LPT9.txt",
    ] {
        let name = compose_filename(&validated(None, Some(reserved), None), "pdf", &[]);
        assert!(name.value.starts_with('_'), "{reserved} was not escaped");
    }
}

#[test]
fn composer_omits_absent_segments_and_duplicate_extension() {
    assert_eq!(
        compose_filename(
            &validated(None, None, Some("Quarterly Report.pdf")),
            "pdf",
            &[]
        )
        .value,
        "Quarterly Report.pdf"
    );
}

#[test]
fn composer_strips_repeated_extensions_without_splitting_utf8() {
    assert_eq!(
        compose_filename(&validated(None, None, Some("Résumé.PDF.pdf")), "pdf", &[]).value,
        "Résumé.pdf"
    );
}

#[test]
fn composer_filters_directional_marks_and_bounds_a_hostile_extension() {
    let subject = "A\u{061c}B\u{200e}C\u{200f}D";
    let extension = "x".repeat(500);
    let value = compose_filename(&validated(None, None, Some(subject)), &extension, &[]).value;
    assert!(value.starts_with("ABCD."));
    assert_eq!(value.chars().count(), 140);
}

#[test]
fn composer_limits_every_collision_candidate_to_140_characters() {
    let subject = "x".repeat(200);
    let first = compose_filename(&validated(None, None, Some(&subject)), "tiff", &[]);
    assert_eq!(first.value.chars().count(), 140);
    let second = compose_filename(
        &validated(None, None, Some(&subject)),
        "tiff",
        &[first.value.as_str()],
    );
    assert_eq!(second.value.chars().count(), 140);
    assert!(second.value.ends_with(" (2).tiff"));
}
