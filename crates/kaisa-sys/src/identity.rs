//! Telling a process apart from a later one that reused its pid: the pid plus the moment the
//! process started. Kaisa records this when it starts an agent, so that after a crash it can
//! prove whether that same agent is still running (architecture §5.1, recovery).

use std::io;

/// A process as it was when Kaisa started it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// The process id.
    pub pid: u32,
    /// When the process started, in the OS's own units (opaque; only compared for equality).
    pub started: u64,
}

impl ProcessIdentity {
    /// The identity of the live process `pid`, or `None` if no such process exists.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the process exists but cannot be inspected.
    pub fn of(pid: u32) -> io::Result<Option<Self>> {
        Ok(start_of(pid)?.map(|started| Self { pid, started }))
    }

    /// True if the process this identity names is still running: same pid, same start.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the process cannot be inspected.
    pub fn is_running(&self) -> io::Result<bool> {
        Ok(start_of(self.pid)? == Some(self.started))
    }

    /// The identity as one line of text, for an attempt directory.
    #[must_use]
    pub fn to_line(&self) -> String {
        format!("{} {}", self.pid, self.started)
    }

    /// Reads an identity written by [`Self::to_line`].
    #[must_use]
    pub fn from_line(line: &str) -> Option<Self> {
        let mut parts = line.split_whitespace();
        let pid: u32 = parts.next()?.parse().ok()?;
        let started: u64 = parts.next()?.parse().ok()?;
        parts.next().is_none().then_some(Self { pid, started })
    }
}

/// Clock ticks since boot at which `pid` started (field 22 of `/proc/<pid>/stat`).
#[cfg(target_os = "linux")]
fn start_of(pid: u32) -> io::Result<Option<u64>> {
    let stat: String = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    // The command name (field 2) may contain spaces and parentheses; fields resume after the
    // last `)`. Field 3 is then the first, so field 22 is the twentieth.
    let after_name: &str = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest)
        .ok_or_else(|| io::Error::other("unexpected /proc stat format"))?;
    after_name
        .split_whitespace()
        .nth(START_TIME_AFTER_NAME)
        .and_then(|field| field.parse().ok())
        .map(Some)
        .ok_or_else(|| io::Error::other("no start time in /proc stat"))
}

/// Index of the start-time field among the fields after the command name.
#[cfg(target_os = "linux")]
const START_TIME_AFTER_NAME: usize = 19;

/// Microseconds since the epoch at which `pid` started (`proc_pidinfo`, BSD info).
#[cfg(target_os = "macos")]
fn start_of(pid: u32) -> io::Result<Option<u64>> {
    let Ok(pid) = libc::c_int::try_from(pid) else {
        return Ok(None);
    };
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size: libc::c_int = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>())
        .map_err(|_| io::Error::other("proc_bsdinfo size out of range"))?;
    #[allow(unsafe_code)]
    // SAFETY: `info` is a writable buffer of exactly `size` bytes for PROC_PIDTBSDINFO.
    let written: libc::c_int = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ESRCH) | Some(0) | None => Ok(None),
            _ => Err(error),
        };
    }
    #[allow(unsafe_code)]
    // SAFETY: the call filled the whole structure.
    let info: libc::proc_bsdinfo = unsafe { info.assume_init() };
    Ok(Some(
        info.pbi_start_tvsec * MICROS_PER_SECOND + info.pbi_start_tvusec,
    ))
}

#[cfg(target_os = "macos")]
const MICROS_PER_SECOND: u64 = 1_000_000;

/// Not implemented on this OS yet: every process looks gone, so recovery treats it as lost
/// track and asks the user rather than guessing (T16.6b, Windows with the keeper).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn start_of(_pid: u32) -> io::Result<Option<u64>> {
    Ok(None)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::ProcessIdentity;

    /// A pid far above any the test runner will hand out.
    const UNUSED_PID: u32 = 999_999;

    #[test]
    fn a_running_process_is_recognised_by_its_identity() {
        let identity = ProcessIdentity::of(std::process::id())
            .expect("this process can be inspected")
            .expect("this process exists");

        assert!(
            identity.is_running().expect("inspectable"),
            "the process that recorded its identity is still the same process"
        );
    }

    #[test]
    fn the_same_pid_with_a_different_start_is_another_process() {
        let mut identity = ProcessIdentity::of(std::process::id())
            .expect("inspectable")
            .expect("exists");
        identity.started += 1;

        assert!(
            !identity.is_running().expect("inspectable"),
            "a reused pid must never be mistaken for the agent Kaisa started"
        );
    }

    #[test]
    fn a_pid_with_no_process_has_no_identity() {
        assert_eq!(
            ProcessIdentity::of(UNUSED_PID).expect("inspectable"),
            None,
            "nothing runs under an unused pid"
        );
    }

    #[test]
    fn an_identity_survives_being_written_down() {
        let identity = ProcessIdentity::of(std::process::id())
            .expect("inspectable")
            .expect("exists");

        assert_eq!(
            ProcessIdentity::from_line(&identity.to_line()),
            Some(identity),
            "the identity read back after a crash is the one recorded"
        );
    }
}
