use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::pipeline::{PipelineError, PipelineResult};

pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "pdf", "docx", "xlsx", "eml", "txt", "md", "markdown", "png", "jpg", "jpeg", "tif", "tiff",
];

pub fn parse_item_id(value: &str) -> PipelineResult<i64> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(PipelineError::new(
            "ITEM_ID_INVALID",
            "item id must be a positive decimal integer",
        ));
    }
    let id = value.parse::<i64>().map_err(|_| {
        PipelineError::new("ITEM_ID_INVALID", "item id is outside the supported range")
    })?;
    if id <= 0 {
        return Err(PipelineError::new(
            "ITEM_ID_INVALID",
            "item id must be positive",
        ));
    }
    Ok(id)
}

pub fn canonical_file(path: &Path) -> PipelineResult<PathBuf> {
    reject_reparse_point(path)?;
    let canonical = path
        .canonicalize()
        .map_err(|_| PipelineError::new("FILE_MISSING", "selected file does not exist"))?;
    let metadata = fs::metadata(&canonical)
        .map_err(|_| PipelineError::new("FILE_MISSING", "selected file is unavailable"))?;
    if !metadata.is_file()
        || metadata.len() == 0
        || skipped_name(&canonical)
        || !supported(&canonical)
    {
        return Err(PipelineError::new(
            "UNSUPPORTED_FORMAT",
            "selected path is not a supported nonempty document",
        ));
    }
    Ok(canonical)
}

pub fn canonical_model_file(path: &Path) -> PipelineResult<PathBuf> {
    reject_reparse_point(path)?;
    let canonical = path
        .canonicalize()
        .map_err(|_| PipelineError::new("MODEL_FILE_INVALID", "selected model file is missing"))?;
    let metadata = fs::metadata(&canonical).map_err(|_| {
        PipelineError::new("MODEL_FILE_INVALID", "selected model file is unavailable")
    })?;
    let gguf = canonical
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"));
    if !metadata.is_file() || metadata.len() == 0 || !gguf {
        return Err(PipelineError::new(
            "MODEL_FILE_INVALID",
            "selected path is not a nonempty GGUF model file",
        ));
    }
    Ok(canonical)
}

pub fn canonical_folder(path: &Path) -> PipelineResult<PathBuf> {
    reject_reparse_point(path)?;
    let canonical = path
        .canonicalize()
        .map_err(|_| PipelineError::new("FOLDER_MISSING", "selected folder does not exist"))?;
    if !fs::metadata(&canonical)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
    {
        return Err(PipelineError::new(
            "FOLDER_MISSING",
            "selected path is not a folder",
        ));
    }
    Ok(canonical)
}

pub fn collect_supported_files(root: &Path) -> PipelineResult<Vec<PathBuf>> {
    let root = canonical_folder(root)?;
    let mut pending = vec![root];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(directory).map_err(|_| {
            PipelineError::new("FOLDER_UNAVAILABLE", "selected folder could not be read")
        })?;
        for entry in entries {
            let entry = entry.map_err(|_| {
                PipelineError::new("FOLDER_UNAVAILABLE", "folder entry could not be read")
            })?;
            let path = entry.path();
            if skipped_name(&path) || reject_reparse_point(&path).is_err() {
                continue;
            }
            let metadata = entry.metadata().map_err(|_| {
                PipelineError::new("FOLDER_UNAVAILABLE", "folder entry is unavailable")
            })?;
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() && metadata.len() > 0 && supported(&path) {
                files.push(
                    path.canonicalize().map_err(|_| {
                        PipelineError::new("FILE_MISSING", "folder file disappeared")
                    })?,
                );
            }
        }
    }
    files.sort_by(|left, right| {
        left.to_string_lossy()
            .to_lowercase()
            .cmp(&right.to_string_lossy().to_lowercase())
    });
    Ok(files)
}

/// The largest path Windows accepts without the `\\?\` prefix.
const WINDOWS_MAX_PATH: usize = 260;

/// A path as a person should read it.
///
/// `canonicalize` on Windows answers in the verbatim form - `\\?\C:\Users\...`
/// and `\\?\UNC\server\share\...` - which every file API accepts and every
/// person squints at. Settings and history showed it as stored. The prefix
/// is dropped only when the plain spelling is safe: short enough to work
/// without it, and free of the trailing dots and spaces the prefix exists to
/// protect. Storage keeps the verbatim form; this is for display alone.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let plain = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        return text.into_owned();
    };
    let simple = plain.chars().count() < WINDOWS_MAX_PATH
        && plain
            .split(['\\', '/'])
            .all(|component| !component.ends_with([' ', '.']));
    if simple { plain } else { text.into_owned() }
}

fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            SUPPORTED_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        })
}

fn skipped_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_none_or(|name| name.starts_with('.') || name.starts_with("~$"))
}

/// Whether a Windows reparse tag belongs to the cloud files family
/// (IO_REPARSE_TAG_CLOUD through IO_REPARSE_TAG_CLOUD_F).
///
/// OneDrive and SharePoint Files On-Demand mark online-only files as reparse
/// points with one of these tags; reading such a file hydrates it through the
/// sync client. Rejecting them like other reparse points would exclude every
/// synced-but-not-downloaded document, so they are the one exempt family.
pub fn is_cloud_reparse_tag(tag: u32) -> bool {
    (tag & 0xFFFF_0FFF) == 0x9000_001A
}

fn reject_reparse_point(path: &Path) -> PipelineResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| PipelineError::new("FILE_MISSING", "selected path does not exist"))?;
    if metadata.file_type().is_symlink() || rejected_windows_reparse_point(path, &metadata) {
        return Err(PipelineError::new(
            "PATH_UNTRUSTED",
            "symbolic links and junctions are not followed",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn rejected_windows_reparse_point(path: &Path, metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    if metadata.file_attributes() & 0x400 == 0 {
        return false;
    }
    match windows_reparse::reparse_tag(path) {
        Some(tag) => !is_cloud_reparse_tag(tag),
        None => true,
    }
}

#[cfg(not(windows))]
fn rejected_windows_reparse_point(_: &Path, _: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
mod windows_reparse {
    #![allow(unsafe_code)]

    use std::{mem::MaybeUninit, os::windows::ffi::OsStrExt, path::Path};

    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FindClose, FindFirstFileW, WIN32_FIND_DATAW,
        },
    };

    /// Reads the reparse tag of `path`, or `None` when the path carries no
    /// reparse point or cannot be inspected. `dwReserved0` holds the tag only
    /// while `FILE_ATTRIBUTE_REPARSE_POINT` is set in `dwFileAttributes`.
    pub(super) fn reparse_tag(path: &Path) -> Option<u32> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut data = MaybeUninit::<WIN32_FIND_DATAW>::uninit();
        // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the
        // call, and `data` points to writable storage of the exact structure
        // FindFirstFileW fills.
        let handle = unsafe { FindFirstFileW(wide.as_ptr(), data.as_mut_ptr()) };
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        // SAFETY: a valid handle guarantees the API initialized the structure.
        let data = unsafe { data.assume_init() };
        // SAFETY: the handle came from a successful FindFirstFileW call and is
        // closed exactly once.
        unsafe { FindClose(handle) };
        if data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            None
        } else {
            Some(data.dwReserved0)
        }
    }
}
