use std::path::Path;
#[cfg(feature = "native-pdfium")]
use std::path::PathBuf;

use crate::extract::{
    CancellationToken, ExtractionError, PdfBackend, PdfPageInspection, RenderedPage,
};
#[cfg(feature = "native-pdfium")]
use crate::limits::MAX_PAGE_COUNT;

#[cfg(feature = "native-pdfium")]
use pdfium_render::prelude::*;

#[cfg(feature = "native-pdfium")]
pub struct PdfiumBackend {
    library_path: PathBuf,
    render_dpi: f32,
}

#[cfg(feature = "native-pdfium")]
impl PdfiumBackend {
    pub fn new(library_directory: impl AsRef<Path>) -> Result<Self, ExtractionError> {
        let library_path = Pdfium::pdfium_platform_library_name_at_path(&library_directory);
        if !library_path.exists() {
            return Err(ExtractionError::native_assets_missing(format!(
                "PDFium library is absent at {}",
                library_path.display()
            )));
        }
        Pdfium::bind_to_library(&library_path)
            .map_err(|error| ExtractionError::native_assets_missing(error.to_string()))?;
        Ok(Self {
            library_path,
            render_dpi: 300.0,
        })
    }

    fn pdfium(&self) -> Result<Pdfium, ExtractionError> {
        let bindings = Pdfium::bind_to_library(&self.library_path)
            .map_err(|error| ExtractionError::native_assets_missing(error.to_string()))?;
        Ok(Pdfium::new(bindings))
    }

    fn page_dimensions(&self, width_points: f32, height_points: f32) -> (u32, u32) {
        let scale = self.render_dpi / 72.0;
        let width = (width_points * scale).ceil().max(1.0) as u32;
        let height = (height_points * scale).ceil().max(1.0) as u32;
        (width, height)
    }
}

#[cfg(feature = "native-pdfium")]
fn contains_rendered_image(object: &PdfPageObject<'_>) -> bool {
    if object.as_image_object().is_some() {
        return true;
    }
    if let Some(form) = object.as_x_object_form_object() {
        for index in form.as_range() {
            if form
                .get(index)
                .map(|child| contains_rendered_image(&child))
                .unwrap_or(false)
            {
                return true;
            }
        }
    }
    false
}

#[cfg(feature = "native-pdfium")]
fn rendered_image_area(object: &PdfPageObject<'_>) -> f32 {
    if !contains_rendered_image(object) {
        return 0.0;
    }
    // For a Form XObject, its own transformed bounds describe the stamped area
    // on the containing page. This is conservative for mixed-content forms and
    // avoids incorrectly treating nested child coordinates as page coordinates.
    object
        .bounds()
        .map(|bounds| bounds.width().value.abs() * bounds.height().value.abs())
        .unwrap_or(0.0)
}

#[cfg(feature = "native-pdfium")]
impl PdfBackend for PdfiumBackend {
    fn inspect(
        &self,
        path: &Path,
        cancel: &CancellationToken,
    ) -> Result<Vec<PdfPageInspection>, ExtractionError> {
        cancel.check()?;
        let pdfium = self.pdfium()?;
        let document = pdfium
            .load_pdf_from_file(path, None)
            .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
        if document.pages().len() as usize > MAX_PAGE_COUNT {
            return Err(ExtractionError::resource_limit(
                "document exceeds 500 pages",
            ));
        }
        let mut inspections = Vec::with_capacity(document.pages().len() as usize);
        for (page_index, page) in document.pages().iter().enumerate() {
            cancel.check()?;
            let native_text = page
                .text()
                .map_err(|error| ExtractionError::parse_failed(error.to_string()))?
                .all();
            let (width_pixels, height_pixels) =
                self.page_dimensions(page.width().value, page.height().value);
            let page_area = page.width().value.abs() * page.height().value.abs();
            let image_area = page
                .objects()
                .iter()
                .map(|object| rendered_image_area(&object))
                .sum::<f32>();
            let image_coverage = if page_area <= f32::EPSILON {
                0.0
            } else {
                (image_area / page_area).clamp(0.0, 1.0)
            };
            inspections.push(PdfPageInspection {
                page_index,
                native_text,
                image_coverage,
                width_pixels,
                height_pixels,
            });
        }
        Ok(inspections)
    }

    fn render(
        &self,
        path: &Path,
        page_index: usize,
        cancel: &CancellationToken,
    ) -> Result<RenderedPage, ExtractionError> {
        cancel.check()?;
        let pdfium = self.pdfium()?;
        let document = pdfium
            .load_pdf_from_file(path, None)
            .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
        let page = document
            .pages()
            .get(page_index as i32)
            .map_err(|error| ExtractionError::parse_failed(error.to_string()))?;
        let (width, height) = self.page_dimensions(page.width().value, page.height().value);
        let config = PdfRenderConfig::new()
            .set_target_width(width as i32)
            .set_maximum_height(height as i32);
        let image = page
            .render_with_config(&config)
            .map_err(|error| ExtractionError::parse_failed(error.to_string()))?
            .as_image()
            .map_err(|error| ExtractionError::parse_failed(error.to_string()))?
            .into_rgb8();
        cancel.check()?;
        Ok(RenderedPage::new(page_index, image.into()))
    }
}

#[cfg(not(feature = "native-pdfium"))]
#[derive(Clone, Debug, Default)]
pub struct PdfiumBackend;

#[cfg(not(feature = "native-pdfium"))]
impl PdfiumBackend {
    pub fn new(_library_directory: impl AsRef<Path>) -> Result<Self, ExtractionError> {
        Err(ExtractionError::native_assets_missing(
            "intern-worker was built without the native-pdfium feature",
        ))
    }
}

#[cfg(not(feature = "native-pdfium"))]
impl PdfBackend for PdfiumBackend {
    fn inspect(
        &self,
        _path: &Path,
        _cancel: &CancellationToken,
    ) -> Result<Vec<PdfPageInspection>, ExtractionError> {
        Err(ExtractionError::native_assets_missing(
            "PDFium support is unavailable",
        ))
    }

    fn render(
        &self,
        _path: &Path,
        _page_index: usize,
        _cancel: &CancellationToken,
    ) -> Result<RenderedPage, ExtractionError> {
        Err(ExtractionError::native_assets_missing(
            "PDFium support is unavailable",
        ))
    }
}
