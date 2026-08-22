use std::path::{Path, PathBuf};

use intern_worker::extract::{
    CancellationToken, ExtractedDocument, ExtractionError, OcrBackend, RenderedPage,
    apply_detected_rotation, extract_anydoc, extract_pdf, extract_text, load_oriented_image,
    normalize_vision_image, snapshot_source,
};
use intern_worker::limits::ResourceLimits;
use intern_worker::ocr::TesseractOcr;
use intern_worker::pdf::PdfiumBackend;

fn runtime_directory() -> Result<PathBuf, ExtractionError> {
    if let Some(path) = std::env::var_os("INTERN_RUNTIME_DIR") {
        return Ok(path.into());
    }
    let executable = std::env::current_exe().map_err(ExtractionError::io)?;
    executable.parent().map(Path::to_path_buf).ok_or_else(|| {
        ExtractionError::native_assets_missing("worker executable has no parent directory")
    })
}

fn pdf_backend() -> Result<PdfiumBackend, ExtractionError> {
    PdfiumBackend::new(runtime_directory()?)
}

fn ocr_backend() -> Result<TesseractOcr, ExtractionError> {
    let runtime = runtime_directory()?;
    TesseractOcr::new(runtime.join("tesseract.exe"), runtime.join("tessdata"))
}

/// Builds the OCR engine the first time a page actually needs it.
///
/// Around ninety-nine per cent of documents carry usable text, and those
/// documents must not fail, wait, or load anything because an OCR engine
/// happens to be unavailable.
struct LazyOcr {
    engine: std::sync::OnceLock<Result<TesseractOcr, ExtractionError>>,
}

static LAZY_OCR: LazyOcr = LazyOcr {
    engine: std::sync::OnceLock::new(),
};

impl OcrBackend for LazyOcr {
    fn recognize(
        &self,
        page: &RenderedPage,
        cancel: &CancellationToken,
    ) -> Result<intern_worker::extract::OcrResult, ExtractionError> {
        match self.engine.get_or_init(ocr_backend) {
            Ok(engine) => engine.recognize(page, cancel),
            Err(error) => Err(error.clone()),
        }
    }
}

fn extract_path(
    path: PathBuf,
    cancel: CancellationToken,
) -> Result<ExtractedDocument, ExtractionError> {
    let limits = ResourceLimits::default();
    let snapshot = snapshot_source(&path, &limits, &cancel)?;
    let path = snapshot.path();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "docx" | "docm" => extract_anydoc(path, &limits, &cancel),
        "xlsx" => intern_worker::sheet::extract_xlsx(path, &limits, &cancel),
        "eml" => intern_worker::email::extract_eml(path, &limits, &cancel),
        "txt" | "md" | "markdown" => extract_text(path, &limits, &cancel),
        "pdf" => extract_pdf(path, &pdf_backend()?, &LAZY_OCR, &limits, &cancel),
        "png" | "jpg" | "jpeg" | "tif" | "tiff" => {
            cancel.check()?;
            let image = load_oriented_image(path, &limits)?;
            let rendered = RenderedPage::new(0, image);
            let result = LAZY_OCR.recognize(&rendered, &cancel)?;
            let low_confidence = result.mean_confidence < 75.0;
            let optional_image = Some(normalize_vision_image(
                0,
                apply_detected_rotation(rendered.image, result.rotation_degrees)?,
            )?);
            Ok(ExtractedDocument {
                pages: vec![intern_worker::extract::ExtractedPage {
                    page_number: 1,
                    text: result.text,
                    source: intern_worker::extract::PageSource::Ocr,
                    ocr_confidence: Some(result.mean_confidence),
                    vision_escalated: true,
                }],
                warnings: if low_confidence {
                    vec![intern_worker::extract::ExtractionWarning::LowOcrConfidence]
                } else {
                    vec![]
                },
                truncated: false,
                optional_image,
            })
        }
        _ => Err(ExtractionError::unsupported(
            "supported formats are PDF, DOCX, XLSX, EML, TXT, Markdown, PNG, JPEG, and TIFF",
        )),
    }
}

fn main() {
    if let Err(error) = intern_worker::protocol::run_concurrent_worker(
        std::io::stdin(),
        std::io::stdout(),
        std::io::stderr(),
        extract_path,
    ) {
        eprintln!(
            "{{\"level\":\"error\",\"code\":\"WORKER_IO_FAILED\",\"message\":{}}}",
            serde_json::to_string(&error.to_string())
                .unwrap_or_else(|_| "\"worker I/O failed\"".to_owned())
        );
        std::process::exit(1);
    }
}
