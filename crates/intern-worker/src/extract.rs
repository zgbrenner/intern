use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use base64::Engine as _;
use image::{
    DynamicImage, GenericImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader, Rgb,
    RgbImage, imageops::FilterType,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::limits::{MAX_EXTRACTION_DURATION, MAX_VISION_LONG_EDGE, ResourceLimits, VISION_GRID};
use crate::temp::TempWorkspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExtractionErrorKind {
    Canceled,
    ResourceLimit,
    Unsupported,
    NativeAssetsMissing,
    ParseFailed,
    Io,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct ExtractionError {
    kind: ExtractionErrorKind,
    message: String,
}

impl ExtractionError {
    pub fn canceled() -> Self {
        Self {
            kind: ExtractionErrorKind::Canceled,
            message: "request canceled".to_owned(),
        }
    }

    pub fn resource_limit(message: impl Into<String>) -> Self {
        Self {
            kind: ExtractionErrorKind::ResourceLimit,
            message: message.into(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            kind: ExtractionErrorKind::Unsupported,
            message: message.into(),
        }
    }

    pub fn native_assets_missing(message: impl Into<String>) -> Self {
        Self {
            kind: ExtractionErrorKind::NativeAssetsMissing,
            message: message.into(),
        }
    }

    pub fn parse_failed(message: impl Into<String>) -> Self {
        Self {
            kind: ExtractionErrorKind::ParseFailed,
            message: message.into(),
        }
    }

    pub fn io(error: std::io::Error) -> Self {
        Self {
            kind: ExtractionErrorKind::Io,
            message: error.to_string(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self.kind {
            ExtractionErrorKind::Canceled => "CANCELED",
            ExtractionErrorKind::ResourceLimit => "RESOURCE_LIMIT_EXCEEDED",
            ExtractionErrorKind::Unsupported => "UNSUPPORTED_FORMAT",
            ExtractionErrorKind::NativeAssetsMissing => "NATIVE_ASSETS_MISSING",
            ExtractionErrorKind::ParseFailed | ExtractionErrorKind::Io => "PARSE_FAILED",
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(self.kind, ExtractionErrorKind::Io)
    }
}

#[derive(Debug)]
struct CancellationState {
    canceled: AtomicBool,
    deadline: Instant,
}

#[derive(Clone, Debug)]
pub struct CancellationToken(Arc<CancellationState>);

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        Self(Arc::new(CancellationState {
            canceled: AtomicBool::new(false),
            deadline: Instant::now() + MAX_EXTRACTION_DURATION,
        }))
    }
    pub fn cancel(&self) {
        self.0.canceled.store(true, Ordering::SeqCst);
    }
    pub fn is_canceled(&self) -> bool {
        self.0.canceled.load(Ordering::SeqCst)
    }
    pub fn check(&self) -> Result<(), ExtractionError> {
        if self.is_canceled() {
            Err(ExtractionError::canceled())
        } else if Instant::now() > self.0.deadline {
            Err(ExtractionError::resource_limit(
                "extraction exceeded 30 minutes",
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PdfPageInspection {
    pub page_index: usize,
    pub native_text: String,
    pub image_coverage: f32,
    pub width_pixels: u32,
    pub height_pixels: u32,
}

#[derive(Clone, Debug)]
pub struct RenderedPage {
    pub page_index: usize,
    pub image: DynamicImage,
}

impl RenderedPage {
    pub fn new(page_index: usize, image: DynamicImage) -> Self {
        Self { page_index, image }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OcrResult {
    pub text: String,
    pub mean_confidence: f32,
    /// Clockwise non-EXIF rotation applied before OCR.
    pub rotation_degrees: u16,
}

impl OcrResult {
    pub fn new(text: impl Into<String>, mean_confidence: f32) -> Self {
        Self {
            text: text.into(),
            mean_confidence,
            rotation_degrees: 0,
        }
    }

    pub fn with_rotation(mut self, rotation_degrees: u16) -> Self {
        self.rotation_degrees = rotation_degrees;
        self
    }
}

/// Mean word confidence at or above which a page is considered read. Below it a
/// page earns a `LowOcrConfidence` warning, may escalate to vision, and is worth
/// re-reading in another orientation before any of that.
pub const CONFIDENT_READING: f32 = 75.0;

/// Of two readings of the same page, the one to keep. A tie keeps the
/// incumbent, so the orientation OSD chose wins by default and behaviour on a
/// blank page stays predictable.
///
/// Orientation detection is trained on prose with ascenders and descenders.
/// On a dense all-caps form it can report a rotation that is 180 degrees
/// wrong, and OCR then returns a full page of gibberish rather than obviously
/// empty output - same word count, plausible shape, useless text. Volume
/// cannot tell those apart; word confidence can. Measured on one corpus page,
/// the four orientations scored 23, 14, 14, and 76.
///
/// Confidence alone cannot arbitrate either, because it is a mean over
/// whatever was read: three tokens picked out of a rotated page at 80 outrank
/// three hundred words of the real document at 74.9, and the page then comes
/// back as three tokens. A reading has to be about as dense as the one it
/// displaces before its confidence counts.
pub fn better_reading(incumbent: OcrResult, challenger: OcrResult) -> OcrResult {
    let words = |reading: &OcrResult| reading.text.split_whitespace().count();
    let comparably_dense = words(&challenger) * 2 >= words(&incumbent);
    if challenger.mean_confidence > incumbent.mean_confidence && comparably_dense {
        challenger
    } else {
        incumbent
    }
}

/// Whether reading this page again in another orientation could tell us
/// anything.
///
/// A confident reading is done. So is a reading that found no words at all:
/// that page is blank, and a blank page is blank in four orientations. It
/// scores zero confidence, though, which read as "not confident, keep
/// looking" and bought three more recognition passes and three more
/// full-page PNG encodes - on the back of every sheet of a three-hundred-page
/// double-sided scan.
pub fn orientation_search_is_worthwhile(reading: &OcrResult) -> bool {
    reading.mean_confidence < CONFIDENT_READING && !reading.text.trim().is_empty()
}

pub fn apply_detected_rotation(
    image: DynamicImage,
    rotation_degrees: u16,
) -> Result<DynamicImage, ExtractionError> {
    match rotation_degrees % 360 {
        0 => Ok(image),
        90 => Ok(image.rotate90()),
        180 => Ok(image.rotate180()),
        270 => Ok(image.rotate270()),
        degrees => Err(ExtractionError::parse_failed(format!(
            "unsupported OSD rotation {degrees}"
        ))),
    }
}

pub trait PdfBackend {
    fn inspect(
        &self,
        path: &Path,
        cancel: &CancellationToken,
    ) -> Result<Vec<PdfPageInspection>, ExtractionError>;
    fn render(
        &self,
        path: &Path,
        page_index: usize,
        cancel: &CancellationToken,
    ) -> Result<RenderedPage, ExtractionError>;
}

pub trait OcrBackend {
    fn recognize(
        &self,
        page: &RenderedPage,
        cancel: &CancellationToken,
    ) -> Result<OcrResult, ExtractionError>;
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PageSource {
    Native,
    Ocr,
    AnyDoc,
    Text,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ExtractedPage {
    pub page_number: usize,
    pub text: String,
    pub source: PageSource,
    pub ocr_confidence: Option<f32>,
    pub vision_escalated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExtractionWarning {
    LowOcrConfidence,
    NativeTextCorrupt,
    TextTruncated,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VisionImage {
    pub page_number: usize,
    pub mime_type: String,
    pub data_base64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ExtractedDocument {
    pub pages: Vec<ExtractedPage>,
    pub warnings: Vec<ExtractionWarning>,
    pub truncated: bool,
    pub optional_image: Option<VisionImage>,
}

fn timed_check(
    cancel: &CancellationToken,
    started: Instant,
    limits: &ResourceLimits,
) -> Result<(), ExtractionError> {
    cancel.check()?;
    if started.elapsed() > limits.max_duration {
        return Err(ExtractionError::resource_limit(
            "extraction exceeded 30 minutes",
        ));
    }
    Ok(())
}

pub fn page_needs_ocr(page: &PdfPageInspection) -> bool {
    let meaningful = page
        .native_text
        .chars()
        .filter(|character| {
            !character.is_whitespace() && !character.is_control() && *character != '\u{fffd}'
        })
        .count();
    let considered = page
        .native_text
        .chars()
        .filter(|character| !character.is_whitespace())
        .count();
    let replacements = page
        .native_text
        .chars()
        .filter(|character| *character == '\u{fffd}')
        .count();
    let replacement_ratio = if considered == 0 {
        0.0
    } else {
        replacements as f32 / considered as f32
    };
    // A page that is essentially all image is a scan, and a scan's text layer
    // is whatever the scanner or the review platform stamped on it: a Bates
    // number, a confidentiality legend, an exhibit label. Those clear the
    // twenty-character veto while carrying none of the document, so a
    // stamped scan has to reach OCR on the strength of its coverage.
    let stamped_scan = meaningful < STAMP_CHARACTERS && page.image_coverage >= FULL_PAGE_IMAGE;
    (meaningful < 20 && page.image_coverage >= 0.65) || stamped_scan || replacement_ratio > 0.03
}

/// Native text this short on a page that is all image is a stamp, not the
/// document.
const STAMP_CHARACTERS: usize = 200;

/// Image coverage at or above which a page is a picture of a page rather
/// than a page with a picture on it. A scanner covers the sheet; a chart or a
/// letterhead on a page of prose does not come close.
const FULL_PAGE_IMAGE: f32 = 0.9;

fn page_needs_vision(page: &PdfPageInspection) -> bool {
    let meaningful = page
        .native_text
        .chars()
        .filter(|character| !character.is_whitespace())
        .count();
    // Word-structured native text is trustworthy and stays text-only; a large
    // non-text region only routes to vision when extraction leaves less than
    // 100 meaningful characters without word structure.
    let word_structured = page.native_text.split_whitespace().count() > 1;
    meaningful < 100 && page.image_coverage >= 0.65 && !word_structured
}

pub fn extract_pdf(
    path: &Path,
    pdf: &dyn PdfBackend,
    ocr: &dyn OcrBackend,
    limits: &ResourceLimits,
    cancel: &CancellationToken,
) -> Result<ExtractedDocument, ExtractionError> {
    let started = Instant::now();
    timed_check(cancel, started, limits)?;
    let inspections = pdf.inspect(path, cancel)?;
    limits.validate_page_count(inspections.len())?;
    let mut pages = Vec::with_capacity(inspections.len());
    let mut warnings = Vec::new();
    let mut vision_candidate: Option<VisionImage> = None;

    for inspection in inspections {
        timed_check(cancel, started, limits)?;
        // The render cap belongs to rendering. A large-format sheet - an A1
        // drawing, a plan set - is over it at 300 DPI while carrying a
        // perfectly good text layer, and failing the whole document over a
        // page nobody was going to rasterise loses the document. A page that
        // is too large to render also cannot be escalated to vision, but it
        // keeps its text: the page image is the optional part.
        let renderable = limits
            .validate_page_pixels(inspection.width_pixels, inspection.height_pixels)
            .is_ok();
        if !page_needs_ocr(&inspection) {
            let vision_escalated =
                renderable && page_needs_vision(&inspection) && vision_candidate.is_none();
            if vision_escalated {
                let rendered = pdf.render(path, inspection.page_index, cancel)?;
                let (render_width, render_height) = rendered.image.dimensions();
                limits.validate_page_pixels(render_width, render_height)?;
                vision_candidate = Some(normalize_vision_image(
                    inspection.page_index,
                    rendered.image,
                )?);
            }
            pages.push(ExtractedPage {
                page_number: inspection.page_index + 1,
                text: inspection.native_text,
                source: PageSource::Native,
                ocr_confidence: None,
                vision_escalated,
            });
            continue;
        }

        if inspection.native_text.contains('\u{fffd}')
            && !warnings.contains(&ExtractionWarning::NativeTextCorrupt)
        {
            warnings.push(ExtractionWarning::NativeTextCorrupt);
        }
        // This page has no text worth keeping, so it has to be rendered to be
        // read at all, and being too large to render is a resource limit.
        limits.validate_page_pixels(inspection.width_pixels, inspection.height_pixels)?;
        let rendered = pdf.render(path, inspection.page_index, cancel)?;
        let (render_width, render_height) = rendered.image.dimensions();
        limits.validate_page_pixels(render_width, render_height)?;
        timed_check(cancel, started, limits)?;
        let result = ocr.recognize(&rendered, cancel)?;
        let vision_escalated =
            vision_candidate.is_none() && result.mean_confidence < CONFIDENT_READING;
        if result.mean_confidence < CONFIDENT_READING
            && !warnings.contains(&ExtractionWarning::LowOcrConfidence)
        {
            warnings.push(ExtractionWarning::LowOcrConfidence);
        }
        if vision_escalated {
            vision_candidate = Some(normalize_vision_image(
                inspection.page_index,
                apply_detected_rotation(rendered.image, result.rotation_degrees)?,
            )?);
        }
        pages.push(ExtractedPage {
            page_number: inspection.page_index + 1,
            text: result.text,
            source: PageSource::Ocr,
            ocr_confidence: Some(result.mean_confidence),
            vision_escalated,
        });
    }

    Ok(ExtractedDocument {
        pages,
        warnings,
        truncated: false,
        optional_image: vision_candidate,
    })
}

pub fn normalize_vision_image(
    page_index: usize,
    image: DynamicImage,
) -> Result<VisionImage, ExtractionError> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err(ExtractionError::parse_failed("image has zero dimensions"));
    }
    let scale = (MAX_VISION_LONG_EDGE as f64 / f64::from(width.max(height))).min(1.0);
    let resized_width = (f64::from(width) * scale).round().max(1.0) as u32;
    let resized_height = (f64::from(height) * scale).round().max(1.0) as u32;
    let rgb = image
        .resize_exact(resized_width, resized_height, FilterType::Lanczos3)
        .into_rgb8();
    let padded_width = resized_width.div_ceil(VISION_GRID) * VISION_GRID;
    let padded_height = resized_height.div_ceil(VISION_GRID) * VISION_GRID;
    let mut padded = RgbImage::from_pixel(padded_width, padded_height, Rgb([255, 255, 255]));
    padded
        .copy_from(&rgb, 0, 0)
        .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
    let mut bytes = Vec::new();
    DynamicImage::ImageRgb8(padded)
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
    Ok(VisionImage {
        page_number: page_index + 1,
        mime_type: "image/png".to_owned(),
        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

pub fn extract_anydoc(
    path: &Path,
    limits: &ResourceLimits,
    cancel: &CancellationToken,
) -> Result<ExtractedDocument, ExtractionError> {
    cancel.check()?;
    let metadata = std::fs::metadata(path).map_err(ExtractionError::io)?;
    limits.validate_source_size(metadata.len())?;
    let bytes = std::fs::read(path).map_err(ExtractionError::io)?;
    let format = expected_anydoc_format(path, &bytes)?;
    enforce_office_decompressed_limit(path, limits, cancel)?;
    cancel.check()?;
    let markdown = anydoc::to_markdown_bytes(&bytes, format)
        .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
    cancel.check()?;
    Ok(ExtractedDocument {
        pages: vec![ExtractedPage {
            page_number: 1,
            text: markdown,
            source: PageSource::AnyDoc,
            ocr_confidence: None,
            vision_escalated: false,
        }],
        warnings: vec![],
        truncated: false,
        optional_image: None,
    })
}

/// The parser an extension names, refusing content that disagrees with it.
///
/// Left to itself anydoc picks its parser from the file's content and treats
/// the extension as a fallback, so a workbook renamed `.docx` is rendered by
/// its Excel path with none of the row and column caps spreadsheets are
/// routed through here, and a PDF renamed `.pptx` reaches a PDF reader with
/// no page cap, no OCR, and no page image. Routing in this crate is by
/// extension, so content that is not what the extension names is a routing
/// failure that belongs in review, not a document to parse anyway.
fn expected_anydoc_format(path: &Path, bytes: &[u8]) -> Result<anydoc::Format, ExtractionError> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let named = anydoc::Format::from_extension(&extension).ok_or_else(|| {
        ExtractionError::unsupported(format!("no Office reader handles a .{extension} file"))
    })?;
    if anydoc::Format::from_bytes(bytes) != Some(named) {
        return Err(ExtractionError::unsupported(format!(
            "file content is not what its .{extension} extension names"
        )));
    }
    Ok(named)
}

pub(crate) fn enforce_office_decompressed_limit(
    path: &Path,
    limits: &ResourceLimits,
    cancel: &CancellationToken,
) -> Result<(), ExtractionError> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "docx" | "docm" | "xlsx") {
        return Ok(());
    }
    let file = File::open(path).map_err(ExtractionError::io)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    for index in 0..archive.len() {
        cancel.check()?;
        let mut entry = archive
            .by_index(index)
            .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
        loop {
            cancel.check()?;
            let read = entry.read(&mut buffer).map_err(ExtractionError::io)?;
            if read == 0 {
                break;
            }
            total = total.checked_add(read as u64).ok_or_else(|| {
                ExtractionError::resource_limit("Office decompression size overflow")
            })?;
            if total > limits.max_decompressed_office_bytes {
                return Err(ExtractionError::resource_limit(
                    "decompressed Office content exceeds 1 GiB",
                ));
            }
        }
    }
    Ok(())
}

pub fn extract_text(
    path: &Path,
    limits: &ResourceLimits,
    cancel: &CancellationToken,
) -> Result<ExtractedDocument, ExtractionError> {
    cancel.check()?;
    let metadata = std::fs::metadata(path).map_err(ExtractionError::io)?;
    limits.validate_source_size(metadata.len())?;
    let file = File::open(path).map_err(ExtractionError::io)?;
    let mut reader = BufReader::new(file).take(limits.max_source_bytes + 1);
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        cancel.check()?;
        let read = reader.read(&mut buffer).map_err(ExtractionError::io)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    limits.validate_source_size(bytes.len() as u64)?;
    let (text, lossy) = decode_text(&bytes);
    cancel.check()?;
    Ok(ExtractedDocument {
        pages: vec![ExtractedPage {
            page_number: 1,
            text,
            source: PageSource::Text,
            ocr_confidence: None,
            vision_escalated: false,
        }],
        warnings: if lossy {
            vec![ExtractionWarning::NativeTextCorrupt]
        } else {
            vec![]
        },
        truncated: false,
        optional_image: None,
    })
}

/// Decodes a text file by its byte-order mark, and says whether anything was
/// replaced on the way.
///
/// Notepad and PowerShell's redirection still write UTF-16, and a mark on a
/// UTF-8 file is ordinary; neither is a document to refuse, and the mark
/// itself is not a character of the document. Bytes that decode as nothing
/// known are read lossily rather than lost: half a document with a warning
/// beats a file the queue cannot open at all.
fn decode_text(bytes: &[u8]) -> (String, bool) {
    fn from_utf16(units: impl Iterator<Item = u16>) -> (String, bool) {
        let units = units.collect::<Vec<_>>();
        match String::from_utf16(&units) {
            Ok(text) => (text, false),
            Err(_) => (String::from_utf16_lossy(&units), true),
        }
    }
    match bytes {
        [0xFF, 0xFE, rest @ ..] => from_utf16(
            rest.chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]])),
        ),
        [0xFE, 0xFF, rest @ ..] => from_utf16(
            rest.chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]])),
        ),
        [0xEF, 0xBB, 0xBF, rest @ ..] => from_utf8(rest),
        _ => from_utf8(bytes),
    }
}

fn from_utf8(bytes: &[u8]) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_owned(), false),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), true),
    }
}

/// Reads a standalone image file as a one-page document. There is no text
/// layer to prefer, so the page is OCR'd and kept as the page image.
///
/// A TIFF can hold a page per frame - a fax or a batch scan usually does -
/// and only the first frame is read here, because neither this decoder nor
/// the CCITT-compressed files these arrive in support the rest. What the
/// caller must not do is believe it received the whole document, so unread
/// frames are reported as truncation.
pub fn extract_image(
    path: &Path,
    ocr: &dyn OcrBackend,
    limits: &ResourceLimits,
    cancel: &CancellationToken,
) -> Result<ExtractedDocument, ExtractionError> {
    cancel.check()?;
    let image = load_oriented_image(path, limits)?;
    let rendered = RenderedPage::new(0, image);
    let result = ocr.recognize(&rendered, cancel)?;
    let mut warnings = Vec::new();
    if result.mean_confidence < CONFIDENT_READING {
        warnings.push(ExtractionWarning::LowOcrConfidence);
    }
    let truncated = has_unread_frames(path);
    if truncated {
        warnings.push(ExtractionWarning::TextTruncated);
    }
    let optional_image = Some(normalize_vision_image(
        0,
        apply_detected_rotation(rendered.image, result.rotation_degrees)?,
    )?);
    Ok(ExtractedDocument {
        pages: vec![ExtractedPage {
            page_number: 1,
            text: result.text,
            source: PageSource::Ocr,
            ocr_confidence: Some(result.mean_confidence),
            vision_escalated: true,
        }],
        warnings,
        truncated,
        optional_image,
    })
}

/// Whether an image file holds frames after the one that was read.
///
/// A TIFF is a chain of image file directories; the first one's link to the
/// next is all this needs, and it is read rather than decoded so a
/// twelve-frame fax costs one seek. Anything that does not read as a TIFF
/// holds one image by construction, and a file whose chain cannot be
/// followed is reported as single-framed rather than as an error: the frame
/// that was read is still the document's first page.
fn has_unread_frames(path: &Path) -> bool {
    fn next_directory(path: &Path) -> Option<u64> {
        let mut file = File::open(path).ok()?;
        let mut header = [0_u8; 8];
        file.read_exact(&mut header).ok()?;
        let big_endian = match &header[..2] {
            b"II" => false,
            b"MM" => true,
            _ => return None,
        };
        let word = |bytes: [u8; 2]| {
            if big_endian {
                u16::from_be_bytes(bytes)
            } else {
                u16::from_le_bytes(bytes)
            }
        };
        let long = |bytes: [u8; 4]| {
            if big_endian {
                u32::from_be_bytes(bytes)
            } else {
                u32::from_le_bytes(bytes)
            }
        };
        let quad = |bytes: [u8; 8]| {
            if big_endian {
                u64::from_be_bytes(bytes)
            } else {
                u64::from_le_bytes(bytes)
            }
        };
        // Classic TIFF marks itself 42 and counts in 32-bit words; BigTIFF
        // marks itself 43 and counts in 64-bit ones, with wider entries.
        let (first, entry_bytes, big) = match word([header[2], header[3]]) {
            42 => (
                u64::from(long([header[4], header[5], header[6], header[7]])),
                12_u64,
                false,
            ),
            43 => {
                let mut offset = [0_u8; 8];
                file.read_exact(&mut offset).ok()?;
                (quad(offset), 20_u64, true)
            }
            _ => return None,
        };
        file.seek(SeekFrom::Start(first)).ok()?;
        let entries = if big {
            let mut count = [0_u8; 8];
            file.read_exact(&mut count).ok()?;
            quad(count)
        } else {
            let mut count = [0_u8; 2];
            file.read_exact(&mut count).ok()?;
            u64::from(word(count))
        };
        file.seek(SeekFrom::Current(
            i64::try_from(entries.checked_mul(entry_bytes)?).ok()?,
        ))
        .ok()?;
        if big {
            let mut next = [0_u8; 8];
            file.read_exact(&mut next).ok()?;
            Some(quad(next))
        } else {
            let mut next = [0_u8; 4];
            file.read_exact(&mut next).ok()?;
            Some(u64::from(long(next)))
        }
    }
    next_directory(path).is_some_and(|next| next != 0)
}

pub fn load_oriented_image(
    path: &Path,
    limits: &ResourceLimits,
) -> Result<DynamicImage, ExtractionError> {
    let metadata = std::fs::metadata(path).map_err(ExtractionError::io)?;
    limits.validate_source_size(metadata.len())?;
    let mut decoder = ImageReader::open(path)
        .map_err(ExtractionError::io)?
        .with_guessed_format()
        .map_err(|error| ExtractionError::parse_failed(error.to_string()))?
        .into_decoder()
        .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
    let (encoded_width, encoded_height) = decoder.dimensions();
    limits.validate_page_pixels(encoded_width, encoded_height)?;
    let orientation = decoder
        .orientation()
        .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
    let mut image = DynamicImage::from_decoder(decoder)
        .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
    image.apply_orientation(orientation);
    limits.validate_page_pixels(image.width(), image.height())?;
    Ok(image.into_rgb8().into())
}

#[derive(Debug)]
pub struct SourceSnapshot {
    _workspace: TempWorkspace,
    path: PathBuf,
}

impl SourceSnapshot {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn snapshot_source(
    source: &Path,
    limits: &ResourceLimits,
    cancel: &CancellationToken,
) -> Result<SourceSnapshot, ExtractionError> {
    snapshot_source_after_open(source, limits, cancel, || {})
}

fn snapshot_source_after_open<F>(
    source: &Path,
    limits: &ResourceLimits,
    cancel: &CancellationToken,
    after_open: F,
) -> Result<SourceSnapshot, ExtractionError>
where
    F: FnOnce(),
{
    cancel.check()?;
    let mut input = File::open(source).map_err(ExtractionError::io)?;
    let before = input.metadata().map_err(ExtractionError::io)?;
    limits.validate_source_size(before.len())?;
    after_open();
    let workspace = TempWorkspace::create("source", limits.max_temp_bytes)?;
    let relative = match source.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if !extension.is_empty() => format!("source.{extension}"),
        _ => "source".to_owned(),
    };
    let path =
        workspace.write_from_reader(relative, &mut input, limits.max_source_bytes, cancel)?;
    let after = input.metadata().map_err(ExtractionError::io)?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err(ExtractionError::parse_failed(
            "source file changed while it was being snapshotted",
        ));
    }
    cancel.check()?;
    Ok(SourceSnapshot {
        _workspace: workspace,
        path,
    })
}

#[cfg(all(test, unix))]
mod snapshot_tests {
    use super::*;

    #[test]
    fn snapshot_stays_bound_to_open_file_when_source_path_is_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        std::fs::write(&source, b"original").unwrap();
        let replacement = directory.path().join("replacement.txt");
        std::fs::write(&replacement, b"replacement").unwrap();

        let snapshot = snapshot_source_after_open(
            &source,
            &ResourceLimits::default(),
            &CancellationToken::new(),
            || std::fs::rename(&replacement, &source).unwrap(),
        )
        .unwrap();

        assert_eq!(std::fs::read(snapshot.path()).unwrap(), b"original");
        assert_eq!(std::fs::read(source).unwrap(), b"replacement");
    }
}
