//! Atomic file primitives for the coordination directory.
//!
//! Temp files are dot-prefixed so that intake scanners on every machine skip
//! them, even when the sync client replicates one mid-write.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn temp_sibling(target: &Path, purpose: &str) -> PathBuf {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let process = std::process::id();
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    target.with_file_name(format!(
        ".{name}.{process}.{nanos}.{sequence}.{purpose}.intern-tmp"
    ))
}

/// Creates `target` with `bytes` only if no file exists there yet.
///
/// `hard_link` publishes the fully written temp file in a single step, so a
/// reader (or the sync client) can never observe a half-written target, and
/// two racing writers can never both succeed. Filesystems without hard links
/// fall back to `create_new`: exclusive creation is still atomic there, but a
/// crash between open and write can leave a partial target behind.
pub(crate) fn create_exclusive(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp = temp_sibling(target, "create");
    write_sync(&temp, bytes)?;
    match fs::hard_link(&temp, target) {
        Ok(()) => {
            let _ = fs::remove_file(&temp);
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            let _ = fs::remove_file(&temp);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)?;
            file.write_all(bytes)?;
            file.sync_all()
        }
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error)
        }
    }
}

/// Replaces `target` with `bytes` via a same-directory temp file and rename,
/// so readers only ever see the old content or the new content, never a mix.
pub(crate) fn replace_file(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp = temp_sibling(target, "replace");
    write_sync(&temp, bytes)?;
    fs::rename(&temp, target).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

fn write_sync(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}
