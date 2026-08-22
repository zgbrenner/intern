//! Local diagnostic: render a fixture PDF page through the production
//! PdfiumBackend and write the PNG for visual inspection.
#[cfg(feature = "native-pdfium")]
fn main() {
    use intern_worker::extract::{CancellationToken, PdfBackend};
    use intern_worker::pdf::PdfiumBackend;
    use std::path::Path;

    let library_dir = std::env::args()
        .nth(1)
        .expect("usage: render_fixture <pdfium-dir> <pdf> <out.png>");
    let pdf_path = std::env::args().nth(2).expect("pdf path");
    let out_path = std::env::args().nth(3).expect("out path");
    let backend = PdfiumBackend::new(&library_dir).expect("bind pdfium");
    let cancel = CancellationToken::new();
    let inspections = backend
        .inspect(Path::new(&pdf_path), &cancel)
        .expect("inspect");
    for page in &inspections {
        println!(
            "page {}: text_len={} coverage={:.3} {}x{}",
            page.page_index,
            page.native_text.len(),
            page.image_coverage,
            page.width_pixels,
            page.height_pixels
        );
    }
    let rendered = backend
        .render(Path::new(&pdf_path), 0, &cancel)
        .expect("render");
    rendered.image.save(&out_path).expect("save png");
    println!("saved {out_path}");
}

#[cfg(not(feature = "native-pdfium"))]
fn main() {
    eprintln!("build with --features native-pdfium");
}
