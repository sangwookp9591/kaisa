//! Linux process file descriptors (architecture §5.1: "never trust a PID alone").
//!
//! A pidfd refers to one specific process. Signals sent through it can never reach a different
//! process that happened to reuse the PID after the original was reaped.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// Opens a pidfd for `pid`. Returns `Ok(None)` on kernels older than 5.3, which lack
/// `pidfd_open`; callers must surface that as reduced protection, never ignore it.
pub(crate) fn open(pid: u32) -> io::Result<Option<OwnedFd>> {
    let pid: libc::pid_t = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid out of range"))?;
    let flags: libc::c_uint = 0;
    #[allow(unsafe_code)]
    // SAFETY: `pidfd_open` takes a pid and flags by value and returns a new descriptor or -1;
    // it does not touch caller memory.
    let result: libc::c_long = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, flags) };
    if result < 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOSYS) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let raw: libc::c_int = libc::c_int::try_from(result)
        .map_err(|_| io::Error::other("pidfd_open returned an out-of-range descriptor"))?;
    #[allow(unsafe_code)]
    // SAFETY: `raw` is a freshly created descriptor that nothing else owns.
    let owned: OwnedFd = unsafe { OwnedFd::from_raw_fd(raw) };
    Ok(Some(owned))
}

/// Sends SIGKILL to the exact process `pidfd` refers to. A process that already exited is
/// not an error.
pub(crate) fn kill(pidfd: &OwnedFd) -> io::Result<()> {
    let no_info: *const libc::siginfo_t = std::ptr::null();
    let flags: libc::c_uint = 0;
    #[allow(unsafe_code)]
    // SAFETY: the descriptor is valid for the call, the siginfo pointer may be null as
    // documented for `pidfd_send_signal`, and no caller memory is written.
    let result: libc::c_long = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            libc::SIGKILL,
            no_info,
            flags,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}
