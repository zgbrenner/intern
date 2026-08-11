use image::{DynamicImage, RgbImage};
use intern_worker::extract::{CancellationToken, OcrBackend, PdfBackend, RenderedPage};
use intern_worker::ocr::TesseractOcr;
use intern_worker::pdf::PdfiumBackend;
use tempfile::tempdir;

#[cfg(all(feature = "native-tesseract", unix))]
fn fake_tesseract(
    directory: &std::path::Path,
    osd_exit_code: i32,
    osd_diagnostic: &str,
) -> TesseractOcr {
    use std::os::unix::fs::PermissionsExt as _;

    let executable = directory.join("tesseract");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$8\" = \"0\" ]; then\n\
           if [ {osd_exit_code} -eq 0 ]; then\n\
             printf 'Orientation in degrees: 270\\nRotate: 90\\n' > \"$2.osd\"\n\
           fi\n\
           printf '{osd_diagnostic}' >&2\n\
           exit {osd_exit_code}\n\
         fi\n\
         {{\n\
           printf 'level\\tpage_num\\tblock_num\\tpar_num\\tline_num\\tword_num\\t'\n\
           printf 'left\\ttop\\twidth\\theight\\tconf\\ttext\\n'\n\
           printf '5\\t1\\t1\\t1\\t1\\t1\\t0\\t0\\t1\\t1\\t88\\tfixture\\n'\n\
         }} > \"$2.tsv\"\n"
    );
    std::fs::write(&executable, script).unwrap();
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&executable, permissions).unwrap();
    let tessdata = directory.join("tessdata");
    std::fs::create_dir(&tessdata).unwrap();
    std::fs::write(tessdata.join("eng.traineddata"), b"fixture").unwrap();
    std::fs::write(tessdata.join("osd.traineddata"), b"fixture").unwrap();
    TesseractOcr::new(executable, tessdata).unwrap()
}

#[test]
fn missing_pdfium_never_reports_successful_extraction() {
    let directory = tempdir().unwrap();
    let error = match PdfiumBackend::new(directory.path()) {
        Ok(backend) => backend
            .inspect(directory.path().join("missing.pdf").as_path(), &CancellationToken::new())
            .unwrap_err(),
        Err(error) => error,
    };

    assert_eq!(error.code(), "NATIVE_ASSETS_MISSING");
}

#[test]
fn missing_tesseract_never_reports_successful_ocr() {
    let directory = tempdir().unwrap();
    let executable = directory.path().join("tesseract.exe");
    let tessdata = directory.path().join("tessdata");
    let page = RenderedPage::new(0, DynamicImage::ImageRgb8(RgbImage::new(10, 10)));
    let error = match TesseractOcr::new(executable, tessdata) {
        Ok(backend) => backend.recognize(&page, &CancellationToken::new()).unwrap_err(),
        Err(error) => error,
    };

    assert_eq!(error.code(), "NATIVE_ASSETS_MISSING");
}

#[cfg(all(feature = "native-tesseract", unix))]
#[test]
fn tesseract_adapter_reads_osd_extension_and_applies_rotation() {
    let directory = tempdir().unwrap();
    let backend = fake_tesseract(directory.path(), 0, "");
    let page = RenderedPage::new(0, DynamicImage::ImageRgb8(RgbImage::new(10, 20)));

    let result = backend.recognize(&page, &CancellationToken::new()).unwrap();

    assert_eq!(result.text, "fixture");
    assert_eq!(result.mean_confidence, 88.0);
    assert_eq!(result.rotation_degrees, 90);
}

#[cfg(all(feature = "native-tesseract", unix))]
#[test]
fn tesseract_osd_exit_one_falls_back_to_zero_degree_ocr() {
    let directory = tempdir().unwrap();
    let backend = fake_tesseract(
        directory.path(),
        1,
        "Too few characters. Skipping this page\\n",
    );
    let page = RenderedPage::new(0, DynamicImage::ImageRgb8(RgbImage::new(10, 20)));

    let result = backend.recognize(&page, &CancellationToken::new()).unwrap();

    assert_eq!(result.text, "fixture");
    assert_eq!(result.rotation_degrees, 0);
}

#[cfg(all(feature = "native-tesseract", unix))]
#[test]
fn tesseract_osd_does_not_turn_cancellation_into_fallback() {
    let directory = tempdir().unwrap();
    let backend = fake_tesseract(
        directory.path(),
        1,
        "Too few characters. Skipping this page\\n",
    );
    let page = RenderedPage::new(0, DynamicImage::ImageRgb8(RgbImage::new(10, 20)));
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = backend.recognize(&page, &cancel).unwrap_err();

    assert_eq!(error.code(), "CANCELED");
}

#[cfg(all(feature = "native-tesseract", unix))]
#[test]
fn tesseract_spawn_failure_is_propagated_instead_of_falling_back() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempdir().unwrap();
    let backend = fake_tesseract(
        directory.path(),
        1,
        "Too few characters. Skipping this page\\n",
    );
    let executable = directory.path().join("tesseract");
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o644);
    std::fs::set_permissions(executable, permissions).unwrap();
    let page = RenderedPage::new(0, DynamicImage::ImageRgb8(RgbImage::new(10, 20)));

    let error = backend.recognize(&page, &CancellationToken::new()).unwrap_err();

    assert_eq!(error.code(), "PARSE_FAILED");
    assert!(error.retryable());
}

#[cfg(all(feature = "native-tesseract", unix))]
#[test]
fn tesseract_osd_exit_one_with_initialization_diagnostic_is_not_fallback() {
    let directory = tempdir().unwrap();
    let backend = fake_tesseract(
        directory.path(),
        1,
        concat!(
            "Error opening data file osd.traineddata\\n",
            "Failed loading language osd\\n",
            "Could not initialize tesseract\\n"
        ),
    );
    let page = RenderedPage::new(0, DynamicImage::ImageRgb8(RgbImage::new(10, 20)));

    let error = backend.recognize(&page, &CancellationToken::new()).unwrap_err();

    assert_eq!(error.code(), "NATIVE_ASSETS_MISSING");
    assert!(!error.retryable());
}

#[cfg(feature = "native-pdfium")]
fn nested_image_pdf() -> Vec<u8> {
    fn stream(dictionary: &str, contents: &[u8]) -> Vec<u8> {
        let mut object =
            format!("<< {dictionary} /Length {} >>\nstream\n", contents.len()).into_bytes();
        object.extend_from_slice(contents);
        object.extend_from_slice(b"\nendstream");
        object
    }

    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        concat!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] ",
            "/Resources << /XObject << /Fm0 4 0 R >> >> /Contents 5 0 R >>"
        )
        .as_bytes()
        .to_vec(),
        stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /Im0 6 0 R >> >>",
            b"q 100 0 0 100 0 0 cm /Im0 Do Q",
        ),
        stream("", b"q /Fm0 Do Q"),
        stream(
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8",
            &[255, 0, 0],
        ),
    ];
    let mut pdf = b"%PDF-1.7\n%\x80\x80\x80\x80\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

#[cfg(feature = "native-pdfium")]
#[test]
fn form_xobject_nested_image_contributes_rendered_coverage() {
    let Some(library_directory) = std::env::var_os("INTERN_PDFIUM_DIR") else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("nested-image.pdf");
    std::fs::write(&path, nested_image_pdf()).unwrap();
    let backend = PdfiumBackend::new(library_directory).unwrap();

    let pages = backend.inspect(&path, &CancellationToken::new()).unwrap();

    assert_eq!(pages.len(), 1);
    assert!(pages[0].image_coverage >= 0.65, "{}", pages[0].image_coverage);
}
