//! Tying the processes Intern launches to the life of the process that
//! launched them.
//!
//! `Drop` is not a stop path anything can rely on. Closing the window with
//! background mode off, the tray's Quit item, and the updater's install step
//! all leave through `std::process::exit`, which runs no destructor at all,
//! and a panic or a hard crash leaves through even less. Each of those used to
//! leave llama-server holding well over a gigabyte until the machine was
//! rebooted, made the next launch start a second one, and left the updater
//! overwriting binaries that were still running.
//!
//! On Windows the kernel can do the reaping instead. A job object carrying
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` terminates every process in it when
//! its last handle closes, and the last handle is this process's - closed by
//! the kernel however this process ends, deliberately or not. So every child
//! Intern spawns joins one process-wide job, and the ordinary stop paths
//! below become an optimisation rather than the only hope.
//!
//! Everywhere else this is a no-op: the desktop app ships on Windows, and on
//! other platforms `Drop` remains the stop path it always was.

use std::process::Child;

/// Make `child` die with this process, whatever ends this process.
///
/// Best effort on purpose. If the job cannot be created or the child cannot be
/// assigned to it, the child still stops the way it always did, and refusing
/// to launch a model because of it would help nobody.
#[cfg(windows)]
pub(crate) fn tie_to_this_process(child: &Child) {
    windows_job::assign(child);
}

#[cfg(not(windows))]
pub(crate) fn tie_to_this_process(_child: &Child) {}

// Three Win32 calls with no safe wrapper anywhere in the dependency tree. The
// rest of the crate keeps `#![deny(unsafe_code)]`.
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_job {
    use std::{os::windows::io::AsRawHandle, process::Child, ptr, sync::OnceLock};

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
    };

    /// The one job every child joins, kept for the life of the process and
    /// deliberately never closed: closing it is precisely what kills the
    /// children, so only this process ending may do it. A handle is stored as
    /// a `usize` because a raw pointer is not `Sync`; zero means the job could
    /// not be set up and there is nothing to assign anyone to.
    static JOB: OnceLock<usize> = OnceLock::new();

    pub(super) fn assign(child: &Child) {
        let job = *JOB.get_or_init(create);
        if job == 0 {
            return;
        }
        // SAFETY: `job` is a handle to a job this process owns and never
        // closes, and the child's handle is owned by `child` for the whole
        // call. A failure here is reported through the return value we ignore,
        // not through anything that could be unsound.
        unsafe {
            AssignProcessToJobObject(job as HANDLE, child.as_raw_handle() as HANDLE);
        }
    }

    fn create() -> usize {
        // SAFETY: an unnamed job object with default security, then one call
        // to set its limits with a correctly sized value of exactly the type
        // the information class names. Both are checked for failure.
        unsafe {
            let job = CreateJobObjectW(ptr::null(), ptr::null());
            if job.is_null() {
                return 0;
            }
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if configured == 0 {
                CloseHandle(job);
                return 0;
            }
            job as usize
        }
    }
}
