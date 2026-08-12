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
use crate::domain::{AnalysisTelemetry, DocumentAnalysis, DocumentSource, ValidationOutcome};
use crate::error::EngineResult;
use crate::naming::compose_filename;
use crate::validate::validate;

/// Whether a document should be shown to the model as an image as well as text.
///
/// Vision is a fallback, not the normal path: loading a projector costs
/// hundreds of megabytes and image tokens cost far more CPU than text tokens.
/// It is used only when text extraction produced almost nothing.
pub const MIN_TEXT_CHARACTERS_FOR_TEXT_ONLY: usize = 200;

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
        let image = vision_input(source, digest);
        let request = ModelRequest::from_digest(digest, image.clone());
        let inference_started = Instant::now();
        let proposal = self.client.propose(&request)?;
        let inference_millis =
            u64::try_from(inference_started.elapsed().as_millis()).unwrap_or(u64::MAX);

        let outcome = validate(proposal, digest);
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
                vision_used: image.is_some(),
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

/// True when a document is unreadable enough as text that the page image is
/// worth its cost.
///
/// Callers use this to decide whether to load a vision projector at all, which
/// is why it depends only on the source and not on the digest.
pub fn wants_vision(source: &DocumentSource) -> bool {
    source.page_image.is_some() && source.character_count() < MIN_TEXT_CHARACTERS_FOR_TEXT_ONLY
}

fn vision_input(
    source: &DocumentSource,
    digest: &DocumentDigest,
) -> Option<crate::domain::PageImage> {
    let _ = digest;
    wants_vision(source)
        .then(|| source.page_image.clone())
        .flatten()
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
    fn a_text_bearing_page_never_pays_for_vision() {
        let source = source_with_image(
            "STATEMENT OF WORK\n\nThis Statement of Work is effective as of April 1, 2026 by and \
             between Acme Corporation and Vistage Worldwide, Inc. and covers the 2026 CRM \
             implementation, its deliverables, its fees, and its project term.",
        );
        let digest = distill(&source, DigestBudget::default());
        assert!(vision_input(&source, &digest).is_none());
    }

    #[test]
    fn an_unreadable_scan_falls_back_to_vision() {
        let source = source_with_image("l1 ll  I");
        let digest = distill(&source, DigestBudget::default());
        assert!(vision_input(&source, &digest).is_some());
    }

    #[test]
    fn a_document_with_no_image_never_uses_vision() {
        let source = source_from_text("x");
        let digest = distill(&source, DigestBudget::default());
        assert!(vision_input(&source, &digest).is_none());
    }
}
