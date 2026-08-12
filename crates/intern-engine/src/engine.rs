//! The document-understanding engine: one call in, one structured result out.
//!
//! ```text
//! DocumentSource ─▶ distill ─▶ prompt ─▶ one local inference ─▶ validate ─▶ name
//! ```
//!
//! Everything above this line (extraction, OCR) and everything below it (the
//! queue, the file operations, the UI) is somebody else's problem. That is what
//! makes a CLI, a watched folder, or a future connector able to reuse this
//! without touching how documents are understood.

use std::time::Instant;

use crate::client::{ModelClient, ModelRequest};
use crate::distill::{DigestBudget, DocumentDigest, distill};
use crate::domain::{
    AnalysisTelemetry, DocumentAnalysis, DocumentSource, ProposalStatus, ReviewReason,
    ValidationOutcome,
};
use crate::error::EngineResult;
use crate::naming::compose_filename;
use crate::validate::validate;

/// Below this much extracted text, a document is treated as unreadable rather
/// than analysed on the strength of a few stray characters.
///
/// Intern has no vision fallback: the model is text-only and the local server
/// runs without a projector, so a page nothing could read is a page for a human
/// to look at, not one to guess about.
pub const MIN_READABLE_CHARACTERS: usize = 200;

pub struct Engine {
    client: ModelClient,
    budget: DigestBudget,
}

impl Engine {
    pub fn new(client: ModelClient) -> Self {
        Self {
            client,
            budget: DigestBudget::default(),
        }
    }

    pub fn with_budget(mut self, budget: DigestBudget) -> Self {
        self.budget = budget;
        self
    }

    pub fn budget(&self) -> DigestBudget {
        self.budget
    }

    /// Reads one document and proposes a name, a description, and the evidence
    /// behind both.
    pub fn analyze(
        &self,
        source: &DocumentSource,
        extension: &str,
        existing_names: &[&str],
    ) -> EngineResult<DocumentAnalysis> {
        let distill_started = Instant::now();
        let digest = distill(source, self.budget);
        let distill_micros =
            u64::try_from(distill_started.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.analyze_digest(source, &digest, distill_micros, extension, existing_names)
    }

    /// Runs inference, validation, and naming over an already-built digest.
    pub fn analyze_digest(
        &self,
        source: &DocumentSource,
        digest: &DocumentDigest,
        distill_micros: u64,
        extension: &str,
        existing_names: &[&str],
    ) -> EngineResult<DocumentAnalysis> {
        let request = ModelRequest::from_digest(digest);
        let inference_started = Instant::now();
        let proposal = self.client.propose(&request)?;
        let inference_millis =
            u64::try_from(inference_started.elapsed().as_millis()).unwrap_or(u64::MAX);

        let mut outcome = validate(proposal, digest);
        if barely_readable(source) {
            // A page that yielded almost no text cannot support a confident
            // name, whatever the model returned about it.
            if !outcome.reasons.contains(&ReviewReason::ParserWarning) {
                outcome.reasons.push(ReviewReason::ParserWarning);
            }
            outcome.status = ProposalStatus::NeedsReview;
        }
        Ok(finish(
            outcome,
            digest,
            extension,
            existing_names,
            AnalysisTelemetry {
                source_characters: digest.source_characters,
                digest_characters: digest.digest_characters,
                compression_ratio: digest.compression_ratio(),
                distill_micros,
                inference_millis,
            },
        ))
    }

    pub fn distill(&self, source: &DocumentSource) -> DocumentDigest {
        distill(source, self.budget)
    }
}

/// Builds the final analysis from an already-validated proposal.
///
/// Split out so evaluation harnesses can drive validation and naming without a
/// live model.
pub fn finish(
    outcome: ValidationOutcome,
    digest: &DocumentDigest,
    extension: &str,
    existing_names: &[&str],
    telemetry: AnalysisTelemetry,
) -> DocumentAnalysis {
    let filename = compose_filename(&outcome.proposal, extension, existing_names).value;
    let _ = digest;
    DocumentAnalysis {
        filename,
        description: outcome.proposal.description.clone(),
        status: outcome.status,
        review_reasons: outcome.reasons,
        proposal: outcome.proposal,
        telemetry,
    }
}

/// True when extraction produced too little text to name a document from.
///
/// The parser hands back a rendered page image when it could not read a page as
/// text. Intern cannot show that image to the model, so the image is a signal
/// rather than an input: it means a human should look at this one.
pub fn barely_readable(source: &DocumentSource) -> bool {
    source.page_image.is_some() && source.character_count() < MIN_READABLE_CHARACTERS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distill::source_from_text;
    use crate::domain::{PageImage, PageOrigin, SourcePage};

    fn source_with_image(text: &str) -> DocumentSource {
        DocumentSource {
            pages: vec![SourcePage::new(1, text, PageOrigin::Ocr)],
            parser_warnings: Vec::new(),
            page_image: Some(PageImage {
                page_number: 1,
                media_type: "image/png".into(),
                bytes: vec![1, 2, 3],
            }),
        }
    }

    #[test]
    fn a_text_bearing_page_is_readable_even_when_an_image_came_with_it() {
        let source = source_with_image(
            "STATEMENT OF WORK\n\nThis Statement of Work is effective as of April 1, 2026 by and \
             between Acme Corporation and Vistage Worldwide, Inc. and covers the 2026 CRM \
             implementation, its deliverables, its fees, and its project term.",
        );
        assert!(!barely_readable(&source));
    }

    #[test]
    fn a_scan_that_yielded_nothing_is_flagged_rather_than_guessed_at() {
        assert!(barely_readable(&source_with_image("l1 ll  I")));
    }

    #[test]
    fn a_short_document_with_no_image_is_not_treated_as_unreadable() {
        // A one-line note is short but perfectly legible; only a page the parser
        // gave up on arrives with an image attached.
        assert!(!barely_readable(&source_from_text("Paid in full.")));
    }
}
