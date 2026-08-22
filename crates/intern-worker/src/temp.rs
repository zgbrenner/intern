use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::extract::{CancellationToken, ExtractionError};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct TempWorkspace {
    path: PathBuf,
    budget: u64,
    written: AtomicU64,
}

impl TempWorkspace {
    pub fn create(label: &str, budget: u64) -> Result<Self, ExtractionError> {
        let root = std::env::var_os("INTERN_TEMP_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        Self::create_in(&root, label, budget)
    }

    pub fn create_in(root: &Path, label: &str, budget: u64) -> Result<Self, ExtractionError> {
        let safe_label: String = label
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
            .take(40)
            .collect();
        let nonce = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = root.join(format!(
            "intern-worker-{}-{}-{timestamp}-{nonce}",
            std::process::id(),
            if safe_label.is_empty() {
                "request"
            } else {
                &safe_label
            }
        ));
        fs::create_dir(&path).map_err(ExtractionError::io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .map_err(ExtractionError::io)?;
        }
        Ok(Self {
            path,
            budget,
            written: AtomicU64::new(0),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(
        &self,
        relative: impl AsRef<Path>,
        bytes: &[u8],
    ) -> Result<PathBuf, ExtractionError> {
        let relative = relative.as_ref();
        if !safe_relative_path(relative) {
            return Err(ExtractionError::parse_failed("unsafe temporary path"));
        }
        let length = u64::try_from(bytes.len())
            .map_err(|_| ExtractionError::resource_limit("temporary write is too large"))?;
        let previous = self
            .written
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current
                    .checked_add(length)
                    .filter(|next| *next <= self.budget)
            })
            .map_err(|_| ExtractionError::resource_limit("temporary data exceeds 2 GiB"))?;
        let path = self.path.join(relative);
        let result = (|| {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(ExtractionError::io)?;
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(ExtractionError::io)?;
            file.write_all(bytes).map_err(ExtractionError::io)?;
            file.sync_all().map_err(ExtractionError::io)?;
            Ok(path.clone())
        })();
        if result.is_err() {
            self.written.store(previous, Ordering::SeqCst);
            let _ = fs::remove_file(&path);
        }
        result
    }

    pub fn write_from_reader(
        &self,
        relative: impl AsRef<Path>,
        reader: &mut dyn Read,
        max_bytes: u64,
        cancel: &CancellationToken,
    ) -> Result<PathBuf, ExtractionError> {
        let relative = relative.as_ref();
        if !safe_relative_path(relative) {
            return Err(ExtractionError::parse_failed("unsafe temporary path"));
        }
        let path = self.path.join(relative);
        let mut reserved = 0_u64;
        let result = (|| {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(ExtractionError::io)?;
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(ExtractionError::io)?;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                cancel.check()?;
                let read = reader.read(&mut buffer).map_err(ExtractionError::io)?;
                if read == 0 {
                    break;
                }
                let length = read as u64;
                let next_reserved = reserved
                    .checked_add(length)
                    .ok_or_else(|| ExtractionError::resource_limit("source size overflow"))?;
                if next_reserved > max_bytes {
                    return Err(ExtractionError::resource_limit("source file exceeds 1 GiB"));
                }
                self.written
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                        current
                            .checked_add(length)
                            .filter(|next| *next <= self.budget)
                    })
                    .map_err(|_| ExtractionError::resource_limit("temporary data exceeds 2 GiB"))?;
                reserved = next_reserved;
                output
                    .write_all(&buffer[..read])
                    .map_err(ExtractionError::io)?;
            }
            output.sync_all().map_err(ExtractionError::io)?;
            Ok(path.clone())
        })();
        if result.is_err() {
            self.written.fetch_sub(reserved, Ordering::SeqCst);
            let _ = fs::remove_file(&path);
        }
        result
    }

    pub fn register_existing(&self, path: &Path) -> Result<(), ExtractionError> {
        if !path.starts_with(&self.path) {
            return Err(ExtractionError::parse_failed(
                "temporary file is outside its workspace",
            ));
        }
        let length = fs::metadata(path).map_err(ExtractionError::io)?.len();
        self.written
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current
                    .checked_add(length)
                    .filter(|next| *next <= self.budget)
            })
            .map(|_| ())
            .map_err(|_| ExtractionError::resource_limit("temporary data exceeds 2 GiB"))
    }
}

fn safe_relative_path(path: &Path) -> bool {
    let raw = path.as_os_str().to_string_lossy();
    let bytes = raw.as_bytes();
    if raw.is_empty()
        || raw.starts_with('/')
        || raw.starts_with('\\')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    {
        return false;
    }
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "{{\"level\":\"warning\",\"code\":\"TEMP_CLEANUP_FAILED\",\"message\":{}}}",
                serde_json::to_string(&error.to_string())
                    .unwrap_or_else(|_| "\"temporary cleanup failed\"".to_owned())
            );
        }
    }
}
