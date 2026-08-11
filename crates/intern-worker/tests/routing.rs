use std::path::Path;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

use image::{DynamicImage, GenericImageView, Rgb, RgbImage};
use intern_worker::extract::{
    CancellationToken, ExtractionError, OcrBackend, OcrResult, PageSource, PdfBackend,
    PdfPageInspection, RenderedPage, apply_detected_rotation, extract_pdf,
    normalize_vision_image, page_needs_ocr,
};
use intern_worker::limits::{MAX_PAGE_MEGAPIXELS, ResourceLimits};

#[derive(Clone)]
struct FakePdf {
    pages: Vec<PdfPageInspection>,
    renders: Arc<AtomicUsize>,
}

impl PdfBackend for FakePdf {
    fn inspect(
        &self,
        _path: &Path,
        _cancel: &CancellationToken,
    ) -> Result<Vec<PdfPageInspection>, ExtractionError> {
        Ok(self.pages.clone())
    }

    fn render(
        &self,
        _path: &Path,
        page_index: usize,
        _cancel: &CancellationToken,
    ) -> Result<RenderedPage, ExtractionError> {
        self.renders.fetch_add(1, Ordering::SeqCst);
        let page = &self.pages[page_index];
        let image = DynamicImage::ImageRgb8(RgbImage::new(page.width_pixels, page.height_pixels));
        Ok(RenderedPage::new(page_index, image))
    }
}

#[derive(Clone)]
struct FakeOcr {
    results: Vec<OcrResult>,
}

impl OcrBackend for FakeOcr {
    fn recognize(
        &self,
        page: &RenderedPage,
        _cancel: &CancellationToken,
    ) -> Result<OcrResult, ExtractionError> {
        Ok(self.results[page.page_index].clone())
    }
}

fn page(text: &str, coverage: f32) -> PdfPageInspection {
    PdfPageInspection {
        page_index: 0,
        native_text: text.to_owned(),
        image_coverage: coverage,
        width_pixels: 100,
        height_pixels: 100,
    }
}

fn route(pages: Vec<PdfPageInspection>, ocr: Vec<OcrResult>) -> (intern_worker::extract::ExtractedDocument, usize) {
    let renders = Arc::new(AtomicUsize::new(0));
    let pdf = FakePdf {
        pages,
        renders: Arc::clone(&renders),
    };
    let result = extract_pdf(
        Path::new("fixture.pdf"),
        &pdf,
        &FakeOcr { results: ocr },
        &ResourceLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    (result, renders.load(Ordering::SeqCst))
}

#[test]
fn native_text_page_does_not_render_or_ocr() {
    let (document, renders) = route(
        vec![page("This page contains complete native document text.", 0.0)],
        vec![OcrResult::new("unused", 0.0)],
    );

    assert_eq!(renders, 0);
    assert_eq!(document.pages[0].source, PageSource::Native);
    assert_eq!(document.pages[0].text, "This page contains complete native document text.");
    assert!(document.optional_image.is_none());
}

#[test]
fn fewer_than_twenty_meaningful_characters_with_sixty_five_percent_image_coverage_uses_ocr() {
    let (document, renders) = route(
        vec![page("tiny", 0.65)],
        vec![OcrResult::new("Scanned agreement text", 91.0)],
    );

    assert_eq!(renders, 1);
    assert_eq!(document.pages[0].source, PageSource::Ocr);
    assert_eq!(document.pages[0].text, "Scanned agreement text");
}

#[test]
fn more_than_three_percent_replacement_glyphs_uses_ocr() {
    let (document, renders) = route(
        vec![page("abcdefghijklmnopqrst��", 0.1)],
        vec![OcrResult::new("Recovered text", 88.0)],
    );

    assert_eq!(renders, 1);
    assert_eq!(document.pages[0].source, PageSource::Ocr);
}

#[test]
fn selective_ocr_threshold_boundaries_are_exact() {
    assert!(!page_needs_ocr(&page(&"a".repeat(19), 0.649)));
    assert!(page_needs_ocr(&page(&"a".repeat(19), 0.65)));
    assert!(!page_needs_ocr(&page(&"a".repeat(20), 0.65)));

    let exactly_three_percent = format!("{}{}", "a".repeat(97), "�".repeat(3));
    let over_three_percent = format!("{}{}", "a".repeat(96), "�".repeat(4));
    assert!(!page_needs_ocr(&page(&exactly_three_percent, 0.0)));
    assert!(page_needs_ocr(&page(&over_three_percent, 0.0)));
}

#[test]
fn clean_mixed_page_preserves_native_text() {
    let (document, renders) = route(
        vec![page("Native text remains authoritative on a mixed page.", 0.8)],
        vec![OcrResult::new("unused", 0.0)],
    );

    assert_eq!(renders, 0);
    assert_eq!(document.pages[0].source, PageSource::Native);
}

#[test]
fn low_ocr_confidence_selects_exactly_one_lowest_confidence_image() {
    let mut first = page("", 1.0);
    first.page_index = 0;
    let mut second = page("", 1.0);
    second.page_index = 1;
    let (document, renders) = route(
        vec![first, second],
        vec![
            OcrResult::new("first scan", 72.0),
            OcrResult::new("second scan", 61.0),
        ],
    );

    assert_eq!(renders, 2);
    assert_eq!(document.optional_image.as_ref().unwrap().page_number, 2);
    assert_eq!(
        document.pages.iter().filter(|page| page.vision_escalated).count(),
        1
    );
}

#[test]
fn exactly_seventy_five_confidence_does_not_escalate_but_below_does() {
    let (exactly, _) = route(
        vec![page("", 1.0)],
        vec![OcrResult::new("readable", 75.0)],
    );
    assert!(exactly.optional_image.is_none());

    let (below, _) = route(
        vec![page("", 1.0)],
        vec![OcrResult::new("uncertain", 74.99)],
    );
    assert_eq!(below.optional_image.unwrap().page_number, 1);
}

#[test]
fn osd_rotation_is_applied_clockwise_for_all_supported_quarter_turns() {
    let mut source = RgbImage::from_pixel(2, 3, Rgb([0, 0, 0]));
    source.put_pixel(0, 0, Rgb([255, 0, 0]));

    let ninety = apply_detected_rotation(DynamicImage::ImageRgb8(source.clone()), 90).unwrap();
    assert_eq!(ninety.dimensions(), (3, 2));
    assert_eq!(ninety.to_rgb8().get_pixel(2, 0), &Rgb([255, 0, 0]));

    let one_eighty = apply_detected_rotation(DynamicImage::ImageRgb8(source.clone()), 180).unwrap();
    assert_eq!(one_eighty.dimensions(), (2, 3));
    assert_eq!(one_eighty.to_rgb8().get_pixel(1, 2), &Rgb([255, 0, 0]));

    let two_seventy = apply_detected_rotation(DynamicImage::ImageRgb8(source), 270).unwrap();
    assert_eq!(two_seventy.dimensions(), (3, 2));
    assert_eq!(two_seventy.to_rgb8().get_pixel(0, 1), &Rgb([255, 0, 0]));
}

#[test]
fn render_over_twenty_five_megapixels_is_rejected_before_allocation() {
    let mut oversized = page("", 1.0);
    oversized.width_pixels = 5_001;
    oversized.height_pixels = 5_000;
    let renders = Arc::new(AtomicUsize::new(0));
    let pdf = FakePdf {
        pages: vec![oversized],
        renders: Arc::clone(&renders),
    };

    let error = extract_pdf(
        Path::new("oversized.pdf"),
        &pdf,
        &FakeOcr { results: vec![] },
        &ResourceLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap_err();

    assert_eq!(MAX_PAGE_MEGAPIXELS, 25);
    assert_eq!(error.code(), "RESOURCE_LIMIT_EXCEEDED");
    assert_eq!(renders.load(Ordering::SeqCst), 0);
}

#[test]
fn cancellation_stops_between_pages() {
    struct CancelAfterFirst {
        token: CancellationToken,
    }

    impl OcrBackend for CancelAfterFirst {
        fn recognize(
            &self,
            _page: &RenderedPage,
            _cancel: &CancellationToken,
        ) -> Result<OcrResult, ExtractionError> {
            self.token.cancel();
            Ok(OcrResult::new("first", 90.0))
        }
    }

    let token = CancellationToken::new();
    let mut first = page("", 1.0);
    first.page_index = 0;
    let mut second = page("", 1.0);
    second.page_index = 1;
    let pdf = FakePdf {
        pages: vec![first, second],
        renders: Arc::new(AtomicUsize::new(0)),
    };

    let error = extract_pdf(
        Path::new("cancel.pdf"),
        &pdf,
        &CancelAfterFirst { token: token.clone() },
        &ResourceLimits::default(),
        &token,
    )
    .unwrap_err();

    assert_eq!(error.code(), "CANCELED");
}

#[test]
fn vision_image_is_rgb_bounded_and_padded_to_twenty_eight_pixel_grid() {
    use base64::Engine as _;
    use image::GenericImageView as _;

    let image = DynamicImage::ImageRgb8(RgbImage::new(1_200, 500));
    let normalized = normalize_vision_image(7, image).unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&normalized.data_base64)
        .unwrap();
    let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).unwrap();

    assert_eq!(normalized.page_number, 8);
    assert_eq!(normalized.mime_type, "image/png");
    assert_eq!(decoded.dimensions(), (1_204, 504));
    assert_eq!(decoded.color(), image::ColorType::Rgb8);
}

#[test]
fn vision_image_long_edge_is_reduced_to_1344_pixels() {
    use base64::Engine as _;
    use image::GenericImageView as _;

    let image = DynamicImage::ImageRgb8(RgbImage::new(2_000, 1_000));
    let normalized = normalize_vision_image(0, image).unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&normalized.data_base64)
        .unwrap();
    let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).unwrap();

    assert_eq!(decoded.dimensions(), (1_344, 672));
}
