//! Stable input and output types for the document-understanding engine.
//!
//! Everything a caller needs to drive Intern headlessly lives here: a
//! [`DocumentSource`] goes in, a [`DocumentAnalysis`] comes out. The desktop
//! app, the CLI, and any future watched-folder or connector host share this
//! boundary, so the engine can change without changing its callers.

use serde::{Deserialize, Serialize};

/// Where a page's text came from.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PageOrigin {
    /// Text drawn directly from the PDF content stream.
    Native,
    /// Text recovered by optical character recognition.
    Ocr,
    /// Markdown produced from an Office container.
    Office,
    /// A plain text or Markdown source file.
    PlainText,
}

/// One extracted page of a source document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourcePage {
    pub page_number: usize,
    pub text: String,
    pub origin: PageOrigin,
    pub ocr_confidence: Option<u32>,
}

impl SourcePage {
    pub fn new(page_number: usize, text: impl Into<String>, origin: PageOrigin) -> Self {
        Self {
            page_number,
            text: text.into(),
            origin,
            ocr_confidence: None,
        }
    }
}

/// A rendered page image, supplied only when text extraction was inadequate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PageImage {
    pub page_number: usize,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// A non-fatal problem the parser reported about the extracted text.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParserWarning {
    pub code: String,
    /// Whether the warning can plausibly corrupt the facts the model reads.
    pub field_affecting: bool,
}

impl ParserWarning {
    pub fn new(code: impl Into<String>, field_affecting: bool) -> Self {
        Self {
            code: code.into(),
            field_affecting,
        }
    }
}

/// Everything the extraction stage produced for one document.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentSource {
    pub pages: Vec<SourcePage>,
    pub parser_warnings: Vec<ParserWarning>,
    /// Present only when a page could not be read as text at all.
    pub page_image: Option<PageImage>,
}

impl DocumentSource {
    pub fn from_pages(pages: Vec<SourcePage>) -> Self {
        Self {
            pages,
            parser_warnings: Vec::new(),
            page_image: None,
        }
    }

    pub fn character_count(&self) -> usize {
        self.pages
            .iter()
            .map(|page| page.text.chars().count())
            .sum()
    }
}

/// What a document-defining date actually means.
///
/// The grammar deliberately has no "due", "deadline", or "renewal" member: a
/// future obligation date must never become the filename date, so the model is
/// not given the vocabulary to propose one.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DateRole {
    Effective,
    Execution,
    Notice,
    Termination,
    Amendment,
    Invoice,
    Filing,
    Issuance,
    Other,
}

impl DateRole {
    pub const ALL: [Self; 9] = [
        Self::Effective,
        Self::Execution,
        Self::Notice,
        Self::Termination,
        Self::Amendment,
        Self::Invoice,
        Self::Filing,
        Self::Issuance,
        Self::Other,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Effective => "effective",
            Self::Execution => "execution",
            Self::Notice => "notice",
            Self::Termination => "termination",
            Self::Amendment => "amendment",
            Self::Invoice => "invoice",
            Self::Filing => "filing",
            Self::Issuance => "issuance",
            Self::Other => "other",
        }
    }
}

/// How the defining parties attach to the document type in a filename.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PartyRelation {
    Between,
    For,
    With,
    From,
    To,
    None,
}

impl PartyRelation {
    pub const ALL: [Self; 6] = [
        Self::Between,
        Self::For,
        Self::With,
        Self::From,
        Self::To,
        Self::None,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Between => "between",
            Self::For => "for",
            Self::With => "with",
            Self::From => "from",
            Self::To => "to",
            Self::None => "none",
        }
    }
}

/// Verbatim excerpts that must be found in the distilled document before the
/// corresponding fact is allowed to reach a filename.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Evidence {
    pub date: Option<String>,
    pub document_type: Option<String>,
    pub parties: Vec<String>,
}

/// Raw, unvalidated model output.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ModelProposal {
    pub document_type: Option<String>,
    pub document_date: Option<String>,
    pub date_role: Option<DateRole>,
    pub parties: Vec<String>,
    pub party_relation: PartyRelation,
    pub description: String,
    pub confidence: f32,
    pub needs_review: bool,
    pub evidence: Evidence,
}

/// Model output after evidence, format, and calibration checks.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ValidatedProposal {
    pub document_type: Option<String>,
    pub document_date: Option<String>,
    pub date_role: Option<DateRole>,
    pub parties: Vec<String>,
    pub party_relation: PartyRelation,
    pub description: String,
    pub confidence: f32,
    pub evidence: Evidence,
}

/// Whether a proposal can be applied without a human looking at it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Ready,
    NeedsReview,
}

/// Why a proposal was routed to review.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewReason {
    /// No usable document-defining date was found.
    DateMissing,
    /// The date was not literally present in the document.
    DateUnsupported,
    /// No specific document type was identified.
    TypeMissing,
    /// The document type was not literally present in the document.
    TypeUnsupported,
    /// A named party could not be found in the document.
    PartyUnsupported,
    /// The description asserted something the document does not contain.
    DescriptionUnsupported,
    /// The description was not a single usable sentence.
    DescriptionInvalid,
    /// The model reported low confidence.
    LowConfidence,
    /// The model asked for review itself.
    ModelRequestedReview,
    /// Extraction reported a problem that can corrupt the read facts.
    ParserWarning,
}

impl ReviewReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DateMissing => "DATE_MISSING",
            Self::DateUnsupported => "DATE_UNSUPPORTED",
            Self::TypeMissing => "TYPE_MISSING",
            Self::TypeUnsupported => "TYPE_UNSUPPORTED",
            Self::PartyUnsupported => "PARTY_UNSUPPORTED",
            Self::DescriptionUnsupported => "DESCRIPTION_UNSUPPORTED",
            Self::DescriptionInvalid => "DESCRIPTION_INVALID",
            Self::LowConfidence => "LOW_CONFIDENCE",
            Self::ModelRequestedReview => "MODEL_REQUESTED_REVIEW",
            Self::ParserWarning => "PARSER_WARNING",
        }
    }
}

/// The result of validating one model proposal.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ValidationOutcome {
    pub proposal: ValidatedProposal,
    pub status: ProposalStatus,
    pub reasons: Vec<ReviewReason>,
}

/// A composed filename and the collision suffix it needed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ComposedName {
    pub value: String,
    pub collision_index: u32,
}

/// Local-only measurements for one analysis. Never leaves the machine.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisTelemetry {
    pub source_characters: usize,
    pub digest_characters: usize,
    pub compression_ratio: f32,
    pub distill_micros: u64,
    pub inference_millis: u64,
    pub vision_used: bool,
}

/// Everything Intern knows about one document.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentAnalysis {
    pub filename: String,
    pub description: String,
    pub status: ProposalStatus,
    pub review_reasons: Vec<ReviewReason>,
    pub proposal: ValidatedProposal,
    pub telemetry: AnalysisTelemetry,
}
