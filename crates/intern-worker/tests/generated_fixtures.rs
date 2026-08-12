use std::path::{Path, PathBuf};

use intern_worker::extract::{CancellationToken, extract_anydoc, extract_text};
use intern_worker::limits::ResourceLimits;

/// PDFium keeps process-global state and is not safe to drive from several
/// threads at once. The worker never does - it parses exactly one document at a
/// time - so the tests hold the same discipline instead of racing.
#[cfg(all(windows, feature = "native-pdfium"))]
static PDFIUM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn fixture(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/generated")
        .join(name);
    if path.is_file() {
        Some(path)
    } else if std::env::var_os("INTERN_REQUIRE_GENERATED_FIXTURES").is_some() {
        panic!("required generated fixture is missing: {}", path.display());
    } else {
        None
    }
}

#[test]
fn clean_room_docx_and_markdown_expose_literal_gold_facts() {
    let (Some(docx), Some(markdown)) = (fixture("nda.docx"), fixture("meeting-minutes.md")) else {
        return;
    };
    let limits = ResourceLimits::default();
    let cancel = CancellationToken::new();

    let nda = extract_anydoc(&docx, &limits, &cancel).unwrap();
    let nda_text = &nda.pages[0].text;
    assert!(
        nda_text.contains("Mutual Non-Disclosure Agreement"),
        "{nda_text}"
    );
    assert!(nda_text.contains("Fable Harbor Labs LLC"), "{nda_text}");
    assert!(nda_text.contains("Copper Wren Design Inc."), "{nda_text}");
    assert!(nda_text.contains("Project Marigold"), "{nda_text}");

    let minutes = extract_text(&markdown, &limits, &cancel).unwrap();
    let minutes_text = &minutes.pages[0].text;
    assert!(minutes_text.contains("Quarterly Operations Review"));
    assert!(minutes_text.contains("Fictional Meridian Committee"));
}

#[cfg(all(windows, feature = "native-pdfium"))]
#[test]
fn pinned_pdfium_extracts_literal_employment_facts() {
    use intern_worker::extract::{OcrBackend, OcrResult, RenderedPage, extract_pdf};
    use intern_worker::pdf::PdfiumBackend;

    let _serial = PDFIUM_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    struct OcrMustNotRun;
    impl OcrBackend for OcrMustNotRun {
        fn recognize(
            &self,
            _page: &RenderedPage,
            _cancel: &CancellationToken,
        ) -> Result<OcrResult, intern_worker::extract::ExtractionError> {
            panic!("the text PDF must not route to OCR")
        }
    }

    let Some(path) = fixture("employment-agreement.pdf") else {
        return;
    };
    let pdfium = std::env::var_os("INTERN_PDFIUM_DIR")
        .expect("INTERN_PDFIUM_DIR is required for the Windows fixture gate");
    let extracted = extract_pdf(
        &path,
        &PdfiumBackend::new(pdfium).unwrap(),
        &OcrMustNotRun,
        &ResourceLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let text = extracted
        .pages
        .iter()
        .map(|page| page.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Northstar Lantern Works LLC"), "{text}");
    assert!(text.contains("Mira Vale"), "{text}");
    assert!(text.contains("February 14, 2025"), "{text}");
}

/// One worker process handles a whole queue, so the PDF engine has to survive
/// past the first document. Binding PDFium per request used to fail on the
/// second file and report it as missing native assets.
#[cfg(all(windows, feature = "native-pdfium"))]
#[test]
fn one_pdf_backend_parses_every_document_in_a_queue() {
    use intern_worker::extract::{OcrBackend, OcrResult, RenderedPage, extract_pdf};
    use intern_worker::pdf::PdfiumBackend;

    let _serial = PDFIUM_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    struct OcrMustNotRun;
    impl OcrBackend for OcrMustNotRun {
        fn recognize(
            &self,
            _page: &RenderedPage,
            _cancel: &CancellationToken,
        ) -> Result<OcrResult, intern_worker::extract::ExtractionError> {
            panic!("a text PDF must not route to OCR")
        }
    }

    let names = [
        "employment-agreement.pdf",
        "statement-of-work.pdf",
        "termination-notice.pdf",
        "consulting-amendment.pdf",
        "vendor-invoice.pdf",
        "settlement-agreement.pdf",
    ];
    let Some(paths) = names
        .iter()
        .map(|name| fixture(name))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    let pdfium = std::env::var_os("INTERN_PDFIUM_DIR")
        .expect("INTERN_PDFIUM_DIR is required for the Windows fixture gate");
    let backend = PdfiumBackend::new(pdfium).unwrap();
    for (name, path) in names.iter().zip(paths) {
        let extracted = extract_pdf(
            &path,
            &backend,
            &OcrMustNotRun,
            &ResourceLimits::default(),
            &CancellationToken::new(),
        )
        .unwrap_or_else(|error| panic!("{name} failed after an earlier document: {error}"));
        assert!(
            extracted.pages.iter().any(|page| page.text.len() > 40),
            "{name} produced no usable text"
        );
    }
}

/// The long statement of work is the corpus's demanding case: its defining
/// date sits in the middle, past any head window and before any tail window.
#[cfg(all(windows, feature = "native-pdfium"))]
#[test]
fn the_long_statement_of_work_hides_its_effective_date_in_the_middle() {
    use intern_worker::extract::{OcrBackend, OcrResult, RenderedPage, extract_pdf};
    use intern_worker::pdf::PdfiumBackend;

    let _serial = PDFIUM_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    struct NoOcr;
    impl OcrBackend for NoOcr {
        fn recognize(
            &self,
            _page: &RenderedPage,
            _cancel: &CancellationToken,
        ) -> Result<OcrResult, intern_worker::extract::ExtractionError> {
            panic!("a text PDF must not route to OCR")
        }
    }

    let Some(path) = fixture("statement-of-work.pdf") else {
        return;
    };
    let pdfium = std::env::var_os("INTERN_PDFIUM_DIR")
        .expect("INTERN_PDFIUM_DIR is required for the Windows fixture gate");
    let extracted = extract_pdf(
        &path,
        &PdfiumBackend::new(pdfium).unwrap(),
        &NoOcr,
        &ResourceLimits::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let text = extracted
        .pages
        .iter()
        .map(|page| format!("[Page {}]\n{}", page.page_number, page.text))
        .collect::<Vec<_>>()
        .join("\n\n");
    let total = text.chars().count();
    let offset = text
        .find("effective as of April 1, 2026")
        .expect("the effective date must be extractable");
    let head = text.chars().take(14_000).collect::<String>();
    let tail = text
        .chars()
        .skip(total.saturating_sub(8_000))
        .collect::<String>();
    assert!(
        !head.contains("effective as of April 1, 2026")
            && !tail.contains("effective as of April 1, 2026"),
        "the effective date landed inside a head/tail window at offset {offset} of {total}"
    );
}
