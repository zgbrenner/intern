use std::time::Duration;

use crate::extract::ExtractionError;

pub const MAX_SOURCE_BYTES: u64 = 1_073_741_824;
pub const MAX_PAGE_COUNT: usize = 500;
pub const MAX_DECOMPRESSED_OFFICE_BYTES: u64 = 1_073_741_824;
pub const MAX_TEMP_BYTES: u64 = 2_147_483_648;
pub const MAX_PAGE_MEGAPIXELS: u64 = 25;
pub const MAX_PAGE_PIXELS: u64 = MAX_PAGE_MEGAPIXELS * 1_000_000;
pub const MAX_EXTRACTION_DURATION: Duration = Duration::from_secs(30 * 60);
pub const MAX_RESIDENT_RENDERED_PAGES: usize = 1;
pub const MAX_VISION_LONG_EDGE: u32 = 1_344;
pub const VISION_GRID: u32 = 28;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceLimits {
    pub max_source_bytes: u64,
    pub max_page_count: usize,
    pub max_decompressed_office_bytes: u64,
    pub max_temp_bytes: u64,
    pub max_page_pixels: u64,
    pub max_duration: Duration,
    pub max_resident_rendered_pages: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: MAX_SOURCE_BYTES,
            max_page_count: MAX_PAGE_COUNT,
            max_decompressed_office_bytes: MAX_DECOMPRESSED_OFFICE_BYTES,
            max_temp_bytes: MAX_TEMP_BYTES,
            max_page_pixels: MAX_PAGE_PIXELS,
            max_duration: MAX_EXTRACTION_DURATION,
            max_resident_rendered_pages: MAX_RESIDENT_RENDERED_PAGES,
        }
    }
}

impl ResourceLimits {
    pub fn validate_source_size(&self, bytes: u64) -> Result<(), ExtractionError> {
        if bytes > self.max_source_bytes {
            return Err(ExtractionError::resource_limit("source file exceeds 1 GiB"));
        }
        Ok(())
    }

    pub fn validate_page_count(&self, pages: usize) -> Result<(), ExtractionError> {
        if pages > self.max_page_count {
            return Err(ExtractionError::resource_limit(
                "document exceeds 500 pages",
            ));
        }
        Ok(())
    }

    pub fn validate_page_pixels(&self, width: u32, height: u32) -> Result<(), ExtractionError> {
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| ExtractionError::resource_limit("page pixel count overflow"))?;
        if pixels > self.max_page_pixels {
            return Err(ExtractionError::resource_limit(
                "rendered page exceeds 25 megapixels",
            ));
        }
        Ok(())
    }
}
