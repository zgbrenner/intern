//! Atomic file primitives for the coordination directory.
//!
//! Temp files are dot-prefixed so that intake scanners on every machine skip
//! them, even when the sync client replicates one mid-write.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
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
///
/// The rename is retried briefly, because this directory lives inside the
/// watched folder: the sync client that replicates it, the search indexer and
/// any antivirus filter all open these files, and while one of them holds the
/// target Windows refuses the replace with a sharing violation or a bare
/// "access is denied" rather than anything that names the real reason. Those
/// holds are momentary, so waiting one out is the difference between a claim
/// that renews and a document that strands. Every other error is returned as
/// it arrives.
pub(crate) fn replace_file(target: &Path, bytes: &[u8]) -> io::Result<()> {
    through_transient_holds(|| {
        // A fresh temp each time: a name whose file is pending deletion is
        // refused with the same "access is denied" as a held target, and
        // reusing it would retry into the same refusal.
        let temp = temp_sibling(target, "replace");
        write_sync(&temp, bytes)?;
        fs::rename(&temp, target).inspect_err(|_| {
            let _ = fs::remove_file(&temp);
        })
    })
}

/// How long a momentary hold on a coordination file is waited out. These are
/// the numbers `intern_core::LockRetry` already uses for the same problem on
/// the documents themselves: an antivirus filter or a sync client can hold a
/// file it has just been handed for the better part of a second, so anything
/// shorter reports a failure the next attempt would not have seen.
const HOLD_RETRY_ATTEMPTS: usize = 5;
const HOLD_RETRY_BACKOFF: Duration = Duration::from_millis(200);

/// Runs `attempt` until it stops being refused by another program's handle.
///
/// The closure is retried whole rather than the syscall inside it, so a step
/// that has to be redone from the beginning - creating a fresh temp file after
/// one was left in a delete-pending state - is redone from the beginning.
fn through_transient_holds<T>(mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut waited = 0;
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error) if is_transient_hold(&error) && waited < HOLD_RETRY_ATTEMPTS => {
                std::thread::sleep(HOLD_RETRY_BACKOFF);
                waited += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Whether an error is another program holding the file open for a moment,
/// rather than something about the file or the volume that waiting cannot fix.
fn is_transient_hold(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::PermissionDenied {
        return true;
    }
    // ERROR_SHARING_VIOLATION and ERROR_LOCK_VIOLATION have no `ErrorKind` of
    // their own, so they are recognised by number.
    matches!(error.raw_os_error(), Some(32) | Some(33))
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

    /// A claim renewal must survive the moment another program has the claim
    /// file open. Driven through the rename seam rather than a real handle, so
    /// the retry is proved without racing a thread for it.
    #[test]
    fn a_replace_waits_out_a_hold_and_then_succeeds() {
        use std::cell::Cell;

        let temp = tempfile::TempDir::new().unwrap();
        let from = temp.path().join("source");
        let to = temp.path().join("claim.json");
        fs::write(&from, b"new").unwrap();

        let attempts = Cell::new(0_usize);
        through_transient_holds(|| {
            let seen = attempts.get();
            attempts.set(seen + 1);
            if seen < 2 {
                // What Windows says while the sync client, the indexer or an
                // antivirus filter has the target open.
                return Err(io::Error::from_raw_os_error(5));
            }
            fs::rename(&from, &to)
        })
        .expect("a momentary hold must be waited out, not reported");
        assert_eq!(
            attempts.get(),
            3,
            "the first two holds must have been retried"
        );
        assert_eq!(fs::read(&to).unwrap(), b"new");
    }

    /// The waiting is bounded: a hold that never lets go is reported rather
    /// than retried for ever.
    #[test]
    fn a_hold_that_never_lets_go_is_reported() {
        let temp = tempfile::TempDir::new().unwrap();
        let attempts = std::cell::Cell::new(0_usize);
        let error = through_transient_holds(|| -> io::Result<()> {
            attempts.set(attempts.get() + 1);
            Err(io::Error::from_raw_os_error(32))
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(32));
        assert_eq!(
            attempts.get(),
            HOLD_RETRY_ATTEMPTS + 1,
            "the waiting must be bounded"
        );
        let _ = temp;
    }

    /// Windows really does refuse the replace with one of these while another
    /// program holds the file, so the classification is checked against a real
    /// handle rather than only against the numbers.
    #[cfg(windows)]
    #[test]
    fn windows_names_a_real_hold_as_something_worth_waiting_out() {
        use std::os::windows::fs::OpenOptionsExt;

        let temp = tempfile::TempDir::new().unwrap();
        let target = temp.path().join("claim.json");
        let source = temp.path().join("source");
        fs::write(&target, b"old").unwrap();
        fs::write(&source, b"new").unwrap();

        let held = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&target)
            .unwrap();
        let error = fs::rename(&source, &target).unwrap_err();
        drop(held);
        assert!(
            is_transient_hold(&error),
            "a held file must read as a momentary hold, not a fatal error: {error:?}"
        );
    }

    /// An error that waiting cannot fix is still reported, rather than costing
    /// a scan the whole backoff before failing anyway.
    #[test]
    fn a_replace_into_a_directory_that_is_not_there_fails_at_once() {
        let temp = tempfile::TempDir::new().unwrap();
        let missing = temp.path().join("gone").join("claim.json");
        let error = replace_file(&missing, b"new").unwrap_err();
        assert!(
            !is_transient_hold(&error),
            "{error:?} is not a momentary hold"
        );
    }
}
