use std::path::{Path, PathBuf};

use intern_worker::extract::{CancellationToken, extract_anydoc, extract_text};
use intern_worker::limits::ResourceLimits;

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
    let (Some(docx), Some(markdown)) = (fixture("nda.docx"), fixture("meeting-minutes.md"))
    else {
        return;
    };
    let limits = ResourceLimits::default();
    let cancel = CancellationToken::new();

    let nda = extract_anydoc(&docx, &limits, &cancel).unwrap();
    let nda_text = &nda.pages[0].text;
    assert!(nda_text.contains("Mutual Non-Disclosure Agreement"), "{nda_text}");
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
