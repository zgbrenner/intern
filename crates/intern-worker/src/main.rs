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
    let runtime = runtime_directory()?;
    PdfiumBackend::new(&runtime)
}

fn ocr_backend() -> Result<TesseractOcr, ExtractionError> {
    let runtime = runtime_directory()?;
    TesseractOcr::new(runtime.join("tesseract.exe"), runtime.join("tessdata"))
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
        "txt" | "md" | "markdown" => extract_text(path, &limits, &cancel),
        "pdf" => {
            let pdf = pdf_backend()?;
            let ocr = ocr_backend()?;
            extract_pdf(path, &pdf, &ocr, &limits, &cancel)
        }
        "png" | "jpg" | "jpeg" | "tif" | "tiff" => {
            cancel.check()?;
            let image = load_oriented_image(path, &limits)?;
            let ocr = ocr_backend()?;
            let rendered = RenderedPage::new(0, image);
            let result = ocr.recognize(&rendered, &cancel)?;
            let low_confidence = result.mean_confidence < 75.0;
            let optional_image = if low_confidence {
                Some(normalize_vision_image(
                    0,
                    apply_detected_rotation(rendered.image, result.rotation_degrees)?,
                )?)
            } else {
                None
            };
            Ok(ExtractedDocument {
                pages: vec![intern_worker::extract::ExtractedPage {
                    page_number: 1,
                    text: result.text,
                    source: intern_worker::extract::PageSource::Ocr,
                    ocr_confidence: Some(result.mean_confidence),
                    vision_escalated: low_confidence,
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
            "supported formats are PDF, DOCX, TXT, Markdown, PNG, JPEG, and TIFF",
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
