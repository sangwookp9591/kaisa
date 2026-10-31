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

    /// Ends the process and its process group if it is still this very process; returns
    /// whether there was anything to end. Kaisa started the agent as a group leader, and a
    /// group id cannot be reused while its leader lives, so the group named here is the agent's.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the process cannot be inspected or signalled.
    #[cfg(unix)]
    pub fn end_tree(&self) -> io::Result<bool> {
        if !self.is_running()? {
            return Ok(false);
        }
        crate::process::end_process_group(self.pid)?;
        Ok(true)
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
    linux_start_from_stat(&stat)
}

#[cfg(any(target_os = "linux", test))]
fn linux_start_from_stat(stat: &str) -> io::Result<Option<u64>> {
    // The command name (field 2) may contain spaces and parentheses; fields resume after the
    // last `)`. Field 3 is then the first, so field 22 is the twentieth.
    let after_name: &str = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest)
        .ok_or_else(|| io::Error::other("unexpected /proc stat format"))?;
    let mut fields = after_name.split_whitespace();
    if fields.next() == Some("Z") {
        return Ok(None);
    }
    fields
        .nth(START_TIME_AFTER_NAME - 1)
        .and_then(|field| field.parse().ok())
        .map(Some)
        .ok_or_else(|| io::Error::other("no start time in /proc stat"))
}

/// Index of the start-time field among the fields after the command name.
#[cfg(any(target_os = "linux", test))]
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

/// Windows creation time identifies a process even if its pid has since been reused.
#[cfg(windows)]
fn start_of(pid: u32) -> io::Result<Option<u64>> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::{
        ERROR_INVALID_PARAMETER, FILETIME, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        WaitForSingleObject,
    };

    #[allow(unsafe_code)]
    // SAFETY: opening a non-inheritable query handle by pid has no memory preconditions.
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        // Access denied is not proof of death. Only an invalid pid is treated as absent.
        return if error.raw_os_error() == i32::try_from(ERROR_INVALID_PARAMETER).ok() {
            Ok(None)
        } else {
            Err(error)
        };
    }
    #[allow(unsafe_code)]
    // SAFETY: OpenProcess returned a valid handle owned only here, closed by OwnedHandle.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    #[allow(unsafe_code)]
    // SAFETY: this live process handle has SYNCHRONIZE access; zero only polls its state.
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => return Ok(None),
        WAIT_TIMEOUT => {}
        _ => return Err(io::Error::last_os_error()),
    }
    let empty = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (empty, empty, empty, empty);
    #[allow(unsafe_code)]
    // SAFETY: the query handle is live and all four output pointers are distinct FILETIMEs.
    let read = unsafe {
        GetProcessTimes(
            handle.as_raw_handle(),
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(
        (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
    ))
}

/// Not implemented on this OS yet: every process looks gone, so recovery treats it as lost
/// track and asks the user rather than guessing (T16.6b).
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn start_of(_pid: u32) -> io::Result<Option<u64>> {
    Ok(None)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod tests {
    use super::ProcessIdentity;

    /// Long enough that the agent is still running when the test ends it.
    #[cfg(unix)]
    const AGENT_LIFETIME_SECONDS: &str = "30";
    /// A pid far above any the test runner will hand out.
    const UNUSED_PID: u32 = 999_999;

    fn owner_stat(state: &str) -> String {
        let mut fields = vec!["0"; super::START_TIME_AFTER_NAME + 1];
        fields[0] = state;
        fields[super::START_TIME_AFTER_NAME] = "12345";
        format!("42 (owner with ) parentheses) {}", fields.join(" "))
    }

    #[test]
    fn a_linux_zombie_owner_is_gone_even_before_its_parent_reaps_it() {
        let zombie = owner_stat("Z");
        assert_eq!(
            super::linux_start_from_stat(&zombie).expect("valid stat"),
            None,
            "a zombie cannot own or resume Kaisa recovery"
        );
    }

    #[test]
    fn a_linux_sleeping_owner_keeps_its_start_identity() {
        let sleeping = owner_stat("S");
        let start = sleeping
            .split_whitespace()
            .last()
            .expect("start field")
            .parse()
            .expect("clock ticks");
        assert_eq!(
            super::linux_start_from_stat(&sleeping).expect("valid stat"),
            Some(start),
            "an idle live owner must still protect its attempt"
        );
    }

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

    #[cfg(unix)]
    #[test]
    fn ending_a_recorded_agent_ends_it_and_nothing_else() {
        let mut agent = crate::OwnedProcess::spawn(&crate::ProcessSpec {
            program: std::path::PathBuf::from("/bin/sleep"),
            args: vec![AGENT_LIFETIME_SECONDS.to_owned()],
            working_directory: std::env::temp_dir(),
            env: Vec::new(),
        })
        .expect("sleep starts");
        let identity = ProcessIdentity::of(agent.id())
            .expect("inspectable")
            .expect("the agent exists");
        let reused = ProcessIdentity {
            started: identity.started + 1,
            ..identity
        };

        let ended_stranger: bool = reused.end_tree().expect("inspectable");
        let ended_agent: bool = identity.end_tree().expect("signalled");
        let exit = agent.wait().expect("the agent is reaped");

        assert!(
            !ended_stranger,
            "a process that only shares the pid is never signalled"
        );
        assert!(
            ended_agent,
            "the recorded agent was still running and was ended"
        );
        assert_eq!(
            exit,
            crate::ProcessExit::Terminated,
            "the agent ended by a signal, not on its own"
        );
    }
}
