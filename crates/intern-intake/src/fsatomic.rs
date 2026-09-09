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
    create_exclusive_with(target, bytes, |from, to| fs::hard_link(from, to))
}

/// The linking step is a parameter so a filesystem that refuses hard links can
/// be exercised without one.
fn create_exclusive_with(
    target: &Path,
    bytes: &[u8],
    link: impl Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let temp = temp_sibling(target, "create");
    write_sync(&temp, bytes)?;
    let linked = link(&temp, target);
    let _ = fs::remove_file(&temp);
    match linked {
        Ok(()) => Ok(()),
        // `AlreadyExists` is the one answer that means something about the
        // claim: the target is there and this machine lost the race. Every
        // other error is the filesystem declining to make a link, and only
        // some of them say `Unsupported` - exFAT volumes, some SMB servers and
        // some sync-client filters report a permission error or a raw OS code
        // instead. Treating those as fatal made a claim impossible to create
        // there at all, so anything but `AlreadyExists` falls back.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
        Err(_) => {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)?;
            file.write_all(bytes)?;
            file.sync_all()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A claim must be creatable on a volume that will not make hard links,
    /// whichever way that volume chooses to say so.
    #[test]
    fn create_exclusive_falls_back_when_hard_links_fail_for_any_reason_but_exists() {
        let temp = tempfile::TempDir::new().unwrap();
        for kind in [
            io::ErrorKind::Unsupported,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidInput,
            io::ErrorKind::Other,
        ] {
            let target = temp.path().join(format!("{kind:?}.json"));
            create_exclusive_with(&target, b"claim", |_, _| {
                Err(io::Error::new(kind, "this volume makes no links"))
            })
            .unwrap_or_else(|error| panic!("{kind:?} must fall back: {error}"));
            assert_eq!(fs::read(&target).unwrap(), b"claim");
        }

        // The target already being there is a lost race, not a fallback.
        let taken = temp.path().join("taken.json");
        create_exclusive(&taken, b"first").unwrap();
        let error = create_exclusive_with(&taken, b"second", |_, _| {
            Err(io::Error::from(io::ErrorKind::AlreadyExists))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&taken).unwrap(), b"first");
    }
}
