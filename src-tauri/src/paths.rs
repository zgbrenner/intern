use std::{fs, path::{Path, PathBuf}};

use crate::pipeline::{PipelineError, PipelineResult};

const SUPPORTED_EXTENSIONS: &[&str] = &["pdf", "docx", "txt", "md", "markdown", "png", "jpg", "jpeg", "tif", "tiff"];

pub fn parse_item_id(value: &str) -> PipelineResult<i64> {
    if value.is_empty() || value.starts_with('0') || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(PipelineError::new("ITEM_ID_INVALID", "item id must be a positive decimal integer"));
    }
    let id = value.parse::<i64>().map_err(|_| PipelineError::new(
        "ITEM_ID_INVALID", "item id is outside the supported range",
    ))?;
    if id <= 0 { return Err(PipelineError::new("ITEM_ID_INVALID", "item id must be positive")); }
    Ok(id)
}

pub fn canonical_file(path: &Path) -> PipelineResult<PathBuf> {
    reject_reparse_point(path)?;
    let canonical = path.canonicalize().map_err(|_| PipelineError::new("FILE_MISSING", "selected file does not exist"))?;
    let metadata = fs::metadata(&canonical).map_err(|_| PipelineError::new("FILE_MISSING", "selected file is unavailable"))?;
    if !metadata.is_file() || metadata.len() == 0 || skipped_name(&canonical) || !supported(&canonical) {
        return Err(PipelineError::new("UNSUPPORTED_FORMAT", "selected path is not a supported nonempty document"));
    }
    Ok(canonical)
}

pub fn canonical_folder(path: &Path) -> PipelineResult<PathBuf> {
    reject_reparse_point(path)?;
    let canonical = path.canonicalize().map_err(|_| PipelineError::new("FOLDER_MISSING", "selected folder does not exist"))?;
    if !fs::metadata(&canonical).map(|metadata| metadata.is_dir()).unwrap_or(false) {
        return Err(PipelineError::new("FOLDER_MISSING", "selected path is not a folder"));
    }
    Ok(canonical)
}

pub fn collect_supported_files(root: &Path) -> PipelineResult<Vec<PathBuf>> {
    let root = canonical_folder(root)?;
    let mut pending = vec![root];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(directory).map_err(|_| PipelineError::new(
            "FOLDER_UNAVAILABLE", "selected folder could not be read",
        ))?;
        for entry in entries {
            let entry = entry.map_err(|_| PipelineError::new("FOLDER_UNAVAILABLE", "folder entry could not be read"))?;
            let path = entry.path();
            if skipped_name(&path) || reject_reparse_point(&path).is_err() { continue; }
            let metadata = entry.metadata().map_err(|_| PipelineError::new("FOLDER_UNAVAILABLE", "folder entry is unavailable"))?;
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() && metadata.len() > 0 && supported(&path) {
                files.push(path.canonicalize().map_err(|_| PipelineError::new("FILE_MISSING", "folder file disappeared"))?);
            }
        }
    }
    files.sort_by(|left, right| left.to_string_lossy().to_lowercase().cmp(&right.to_string_lossy().to_lowercase()));
    Ok(files)
}

fn supported(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str())
        .is_some_and(|extension| SUPPORTED_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

fn skipped_name(path: &Path) -> bool {
    path.file_name().and_then(|value| value.to_str())
        .is_none_or(|name| name.starts_with('.') || name.starts_with("~$"))
}

fn reject_reparse_point(path: &Path) -> PipelineResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| PipelineError::new("FILE_MISSING", "selected path does not exist"))?;
    if metadata.file_type().is_symlink() || windows_reparse_point(&metadata) {
        return Err(PipelineError::new("PATH_UNTRUSTED", "symbolic links and junctions are not followed"));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn windows_reparse_point(_: &fs::Metadata) -> bool { false }
