//! Processes Kaisa owns: started, waited for and ended by Kaisa (architecture §5.1).
//!
//! This is the portable layer on top of `std::process`. Process groups, Job Objects and pidfds
//! are added per operating system in later steps without changing this API.

use std::io;
use std::path::PathBuf;
use std::process::{Child, Command};

/// What to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    /// The executable.
    pub program: PathBuf,
    /// Arguments, passed as is (no shell).
    pub args: Vec<String>,
    /// The working directory.
    pub working_directory: PathBuf,
}

/// How an owned process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessExit {
    /// The process exited by itself with this code.
    Code(i32),
    /// The process was ended from outside: by Kaisa, or by a signal.
    Terminated,
}

/// A process started and owned by Kaisa.
#[derive(Debug)]
pub struct OwnedProcess {
    child: Child,
    /// Set once Kaisa ends the process, so the exit is reported as [`ProcessExit::Terminated`]
    /// on every OS (Windows reports a killed process as exit code 1).
    ended_by_us: bool,
}

impl OwnedProcess {
    /// Starts `spec`.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the program cannot be started.
    pub fn spawn(spec: &ProcessSpec) -> io::Result<Self> {
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&spec.working_directory);
        #[cfg(unix)]
        {
            // The process leads a new process group, so everything it starts can be ended
            // together by signalling the group.
            std::os::unix::process::CommandExt::process_group(&mut command, 0);
        }
        let child: Child = command.spawn()?;
        Ok(Self {
            child,
            ended_by_us: false,
        })
    }

    /// The operating system's process id. Only meaningful while the process is owned.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Waits for the process to exit. May be called again after it returned.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if waiting fails.
    pub fn wait(&mut self) -> io::Result<ProcessExit> {
        let status: std::process::ExitStatus = self.child.wait()?;
        if self.ended_by_us {
            return Ok(ProcessExit::Terminated);
        }
        Ok(status
            .code()
            .map_or(ProcessExit::Terminated, ProcessExit::Code))
    }

    /// Ends the process and everything it started (its process group on Unix). Ending a
    /// process that already exited is not an error; on Unix its leftover children are still
    /// ended, because an agent may leave background processes behind.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the process could not be ended.
    pub fn kill_tree(&mut self) -> io::Result<()> {
        let already_exited: bool = self.child.try_wait()?.is_some();
        #[cfg(unix)]
        {
            unix::kill_process_group(self.child.id())?;
        }
        if already_exited {
            return Ok(());
        }
        self.ended_by_us = true;
        match self.child.kill() {
            // The process exited between the check and the kill.
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
            other => other,
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::io;

    /// Sends SIGKILL to every process in the group led by `leader`. A group that no longer
    /// exists is not an error.
    pub(super) fn kill_process_group(leader: u32) -> io::Result<()> {
        let group: libc::pid_t = libc::pid_t::try_from(leader)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid out of range"))?;
        #[allow(unsafe_code)]
        // SAFETY: `kill` has no memory-safety preconditions; a negative pid addresses the
        // process group. The group id is our child's pid, which cannot be reused while the
        // child is unreaped or while any member of its group is alive.
        let result: libc::c_int = unsafe { libc::kill(-group, libc::SIGKILL) };
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
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    use super::ProcessSpec;

    /// How long a "long-running" test process would run if nobody ended it, in seconds.
    pub(crate) const LONG_RUNNING_SECONDS: u32 = 30;

    fn shell(command: &str) -> ProcessSpec {
        let (program, args): (&str, Vec<String>) = if cfg!(windows) {
            ("cmd", vec!["/C".to_owned(), command.to_owned()])
        } else {
            ("sh", vec!["-c".to_owned(), command.to_owned()])
        };
        ProcessSpec {
            program: PathBuf::from(program),
            args,
            working_directory: std::env::temp_dir(),
        }
    }

    /// A process that exits straight away with `code`.
    pub(crate) fn a_process_that_exits_with(code: i32) -> ProcessSpec {
        shell(&format!("exit {code}"))
    }

    /// A shell that starts a long-running background child, writes the child's pid to
    /// `pid_file`, and waits: an agent that left a server running.
    #[cfg(unix)]
    pub(crate) fn a_process_that_starts_a_background_child(
        pid_file: &std::path::Path,
    ) -> ProcessSpec {
        ProcessSpec {
            program: PathBuf::from("sh"),
            args: vec![
                "-c".to_owned(),
                format!("sleep {LONG_RUNNING_SECONDS} & echo $! > \"$0\"; wait"),
                pid_file.to_string_lossy().into_owned(),
            ],
            working_directory: std::env::temp_dir(),
        }
    }

    /// A process that keeps running long enough for a test to end it.
    pub(crate) fn a_long_running_process() -> ProcessSpec {
        if cfg!(windows) {
            shell(&format!("ping -n {LONG_RUNNING_SECONDS} 127.0.0.1 > nul"))
        } else {
            shell(&format!("sleep {LONG_RUNNING_SECONDS}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{a_long_running_process, a_process_that_exits_with};
    use super::{OwnedProcess, ProcessExit};

    /// The exit code a typical CLI agent uses for "I failed".
    const AGENT_FAILURE_CODE: i32 = 1;

    fn started(spec: &super::ProcessSpec) -> OwnedProcess {
        OwnedProcess::spawn(spec).expect("the shell is available on every CI runner")
    }

    #[test]
    fn a_spawned_process_reports_its_exit_code_when_waited_for() {
        let mut process: OwnedProcess = started(&a_process_that_exits_with(AGENT_FAILURE_CODE));

        assert_eq!(
            process.wait().expect("waiting succeeds"),
            ProcessExit::Code(AGENT_FAILURE_CODE),
            "a process that exits by itself reports its own code"
        );
    }

    #[test]
    fn killing_a_running_process_reports_it_as_terminated() {
        let mut process: OwnedProcess = started(&a_long_running_process());

        process
            .kill_tree()
            .expect("ending a running process succeeds");

        assert_eq!(
            process.wait().expect("waiting succeeds"),
            ProcessExit::Terminated,
            "a process Kaisa ended is never mistaken for an agent failure, on any OS"
        );
    }

    #[test]
    fn a_process_that_already_exited_can_still_be_waited_for() {
        let mut process: OwnedProcess = started(&a_process_that_exits_with(AGENT_FAILURE_CODE));
        let first: ProcessExit = process.wait().expect("the first wait succeeds");

        assert_eq!(
            process.wait().expect("waiting again succeeds"),
            first,
            "recovery code may ask twice and must get the same answer"
        );
    }

    #[test]
    fn killing_a_process_that_already_exited_is_not_an_error() {
        let mut process: OwnedProcess = started(&a_process_that_exits_with(AGENT_FAILURE_CODE));
        process.wait().expect("the process exits by itself");

        assert!(
            process.kill_tree().is_ok(),
            "cleanup after a natural exit must not fail"
        );
        assert_eq!(
            process.wait().expect("waiting succeeds"),
            ProcessExit::Code(AGENT_FAILURE_CODE),
            "a natural exit stays a natural exit even if cleanup runs afterwards"
        );
    }

    #[cfg(unix)]
    mod unix_tree {
        use std::path::PathBuf;
        use std::time::{Duration, Instant};

        use super::super::OwnedProcess;
        use super::super::test_support::a_process_that_starts_a_background_child;

        /// How long the test waits for a pid file or for a process to disappear.
        const PATIENCE: Duration = Duration::from_secs(10);
        /// How often it checks while waiting.
        const POLL_INTERVAL: Duration = Duration::from_millis(20);

        fn a_fresh_pid_file() -> PathBuf {
            std::env::temp_dir().join(format!(
                "kaisa-sys-grandchild-{}-{:?}.pid",
                std::process::id(),
                std::thread::current().id()
            ))
        }

        fn wait_for_pid_in(pid_file: &std::path::Path) -> libc::pid_t {
            let deadline: Instant = Instant::now() + PATIENCE;
            while Instant::now() < deadline {
                if let Some(pid) = std::fs::read_to_string(pid_file)
                    .ok()
                    .and_then(|text| text.trim().parse().ok())
                {
                    return pid;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            panic!("the shell never reported its background child");
        }

        fn is_alive(pid: libc::pid_t) -> bool {
            #[allow(unsafe_code)]
            // SAFETY: signal 0 only checks whether the process exists; no memory is touched.
            let result: libc::c_int = unsafe { libc::kill(pid, 0) };
            result == 0
        }

        fn disappears_within_patience(pid: libc::pid_t) -> bool {
            let deadline: Instant = Instant::now() + PATIENCE;
            while Instant::now() < deadline {
                if !is_alive(pid) {
                    return true;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            false
        }

        #[test]
        fn killing_the_tree_also_ends_a_grandchild() {
            let pid_file: PathBuf = a_fresh_pid_file();
            let mut agent =
                OwnedProcess::spawn(&a_process_that_starts_a_background_child(&pid_file))
                    .expect("sh is available");
            let grandchild: libc::pid_t = wait_for_pid_in(&pid_file);

            agent.kill_tree().expect("ending the tree succeeds");
            agent.wait().expect("the agent is reaped");
            let _ = std::fs::remove_file(&pid_file);

            assert!(
                disappears_within_patience(grandchild),
                "a background process left by the agent must not survive cleanup"
            );
        }
    }
}
