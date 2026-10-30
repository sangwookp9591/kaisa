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
    #[cfg(not(windows))]
    child: Child,
    /// Started by std, or by Kaisa itself when it is attached to a pseudo console.
    #[cfg(windows)]
    child: crate::windows_child::WindowsChild,
    /// Set once Kaisa ends the process, so the exit is reported as [`ProcessExit::Terminated`]
    /// on every OS (Windows reports a killed process as exit code 1).
    ended_by_us: bool,
    /// The Job Object that contains the process and everything it starts.
    #[cfg(windows)]
    job: crate::job::Job,
    /// A descriptor for exactly this process; `None` on kernels without `pidfd_open`.
    #[cfg(target_os = "linux")]
    pidfd: Option<std::os::fd::OwnedFd>,
}

impl OwnedProcess {
    /// Starts `spec`.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the program cannot be started.
    pub fn spawn(spec: &ProcessSpec) -> io::Result<Self> {
        let mut command: Command = command_for(spec);
        #[cfg(unix)]
        {
            // The process leads a new process group, so everything it starts can be ended
            // together by signalling the group.
            std::os::unix::process::CommandExt::process_group(&mut command, 0);
        }
        #[cfg(windows)]
        {
            // Suspended until it is inside its job, so it cannot start a child outside it.
            std::os::windows::process::CommandExt::creation_flags(
                &mut command,
                crate::job::CREATE_SUSPENDED,
            );
        }
        let child: Child = command.spawn()?;
        #[cfg(windows)]
        let child = crate::windows_child::WindowsChild::Std(child);
        Self::adopt(child)
    }

    /// Starts a command whose stdio and session were already prepared by the caller, such as
    /// an agent attached to a terminal. The caller is responsible for making the child lead
    /// its own process group, so that [`kill_tree`](Self::kill_tree) reaches its descendants.
    #[cfg(unix)]
    pub(crate) fn spawn_prepared(mut command: Command) -> io::Result<Self> {
        Self::adopt(command.spawn()?)
    }

    #[cfg(not(windows))]
    fn adopt(child: Child) -> io::Result<Self> {
        Ok(Self {
            #[cfg(target_os = "linux")]
            pidfd: crate::pidfd::open(child.id())?,
            child,
            ended_by_us: false,
        })
    }

    /// Takes over a process Kaisa created suspended with `CreateProcessW`, such as an agent
    /// attached to a pseudo console: puts it in its Job Object, then lets it run.
    #[cfg(windows)]
    pub(crate) fn adopt_created(process: crate::windows_child::CreatedProcess) -> io::Result<Self> {
        Self::adopt(crate::windows_child::WindowsChild::Created(process))
    }

    #[cfg(windows)]
    fn adopt(mut child: crate::windows_child::WindowsChild) -> io::Result<Self> {
        let contained: io::Result<crate::job::Job> = crate::job::Job::new().and_then(|job| {
            job.assign(child.as_raw_handle())?;
            crate::job::resume_threads_of(child.id())?;
            Ok(job)
        });
        match contained {
            Ok(job) => Ok(Self {
                child,
                ended_by_us: false,
                job,
            }),
            Err(error) => {
                // Never leave a suspended, uncontained process behind.
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
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

    /// Ends the process and everything it started (its process group on Unix, its Job Object
    /// on Windows). Ending a process that already exited is not an error; its leftover
    /// children are still ended, because an agent may leave background processes behind.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the process could not be ended.
    pub fn kill_tree(&mut self) -> io::Result<()> {
        let already_exited: bool = self.child.try_wait()?.is_some();
        if !already_exited {
            self.ended_by_us = true;
        }
        self.end_tree()
    }

    /// Ends the process group led by the child, then the child itself if it still runs.
    #[cfg(unix)]
    fn end_tree(&mut self) -> io::Result<()> {
        unix::kill_process_group(self.child.id())?;
        #[cfg(target_os = "linux")]
        if let Some(pidfd) = &self.pidfd {
            // Addresses exactly our child, even if its PID was reused.
            return crate::pidfd::kill(pidfd);
        }
        match self.child.kill() {
            // Already exited (and possibly already reaped).
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
            other => other,
        }
    }

    /// Ends every process in the job, including the child.
    #[cfg(windows)]
    fn end_tree(&mut self) -> io::Result<()> {
        self.job.terminate()
    }
}

/// A command that runs `spec` without a shell.
pub(crate) fn command_for(spec: &ProcessSpec) -> Command {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.working_directory);
    command
}

#[cfg(all(target_os = "linux", test))]
impl OwnedProcess {
    /// True when the process is held through a pidfd.
    pub(crate) fn is_held_by_pidfd(&self) -> bool {
        self.pidfd.is_some()
    }
}

#[cfg(all(windows, test))]
impl OwnedProcess {
    /// How many processes are alive in this process's job.
    pub(crate) fn processes_in_job(&self) -> io::Result<u32> {
        self.job.active_processes()
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
    use std::time::{Duration, Instant};

    use super::ProcessSpec;

    /// How long a "long-running" test process would run if nobody ended it, in seconds.
    pub(crate) const LONG_RUNNING_SECONDS: u32 = 30;

    /// `command` run by the platform shell (`sh -c` or `cmd /C`).
    pub(crate) fn shell(command: &str) -> ProcessSpec {
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

    /// A command shell that starts a background child and keeps running itself.
    #[cfg(windows)]
    pub(crate) fn a_process_that_starts_a_background_child_on_windows() -> ProcessSpec {
        shell(&format!(
            "start /B ping -n {LONG_RUNNING_SECONDS} 127.0.0.1 > nul & ping -n {LONG_RUNNING_SECONDS} 127.0.0.1 > nul"
        ))
    }

    /// How long a test waits for a pid file, a process to disappear or a job to change size.
    pub(crate) const PATIENCE: Duration = Duration::from_secs(10);
    /// How often it checks while waiting.
    const POLL_INTERVAL: Duration = Duration::from_millis(20);

    /// A pid file path no other test uses.
    #[cfg(unix)]
    pub(crate) fn a_fresh_pid_file() -> PathBuf {
        std::env::temp_dir().join(format!(
            "kaisa-sys-grandchild-{}-{:?}.pid",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    /// The pid a shell wrote to `pid_file`, once it appears.
    #[cfg(unix)]
    pub(crate) fn wait_for_pid_in(pid_file: &std::path::Path) -> libc::pid_t {
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

    #[cfg(unix)]
    fn is_alive(pid: libc::pid_t) -> bool {
        #[allow(unsafe_code)]
        // SAFETY: signal 0 only checks whether the process exists; no memory is touched.
        let result: libc::c_int = unsafe { libc::kill(pid, 0) };
        result == 0
    }

    /// True once `pid` no longer exists.
    #[cfg(unix)]
    pub(crate) fn disappears_within_patience(pid: libc::pid_t) -> bool {
        let deadline: Instant = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if !is_alive(pid) {
                return true;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        false
    }

    /// True once the number of live processes in `agent`'s job satisfies `expected`.
    #[cfg(windows)]
    pub(crate) fn job_reaches(agent: &super::OwnedProcess, expected: impl Fn(u32) -> bool) -> bool {
        let deadline: Instant = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if expected(agent.processes_in_job().expect("the job can be queried")) {
                return true;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        false
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

        use super::super::OwnedProcess;
        use super::super::test_support::{
            a_fresh_pid_file, a_process_that_starts_a_background_child, disappears_within_patience,
            wait_for_pid_in,
        };

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

    #[cfg(windows)]
    mod windows_tree {
        use super::super::OwnedProcess;
        use super::super::test_support::{
            a_process_that_starts_a_background_child_on_windows, job_reaches,
        };

        /// The shell plus the background child it started.
        const AGENT_AND_ITS_CHILD: u32 = 2;
        /// A job whose processes have all ended.
        const NO_PROCESSES: u32 = 0;

        #[test]
        fn the_job_contains_the_agent_and_the_children_it_starts() {
            let mut agent =
                OwnedProcess::spawn(&a_process_that_starts_a_background_child_on_windows())
                    .expect("cmd is available");

            let grew: bool = job_reaches(&agent, |alive| alive >= AGENT_AND_ITS_CHILD);
            agent.kill_tree().expect("ending the tree succeeds");

            assert!(
                grew,
                "a child started by the agent must land in the agent's job"
            );
        }

        #[test]
        fn killing_the_tree_also_ends_a_grandchild() {
            let mut agent =
                OwnedProcess::spawn(&a_process_that_starts_a_background_child_on_windows())
                    .expect("cmd is available");
            assert!(
                job_reaches(&agent, |alive| alive >= AGENT_AND_ITS_CHILD),
                "the background child must start before the test can end it"
            );

            agent.kill_tree().expect("ending the tree succeeds");
            agent.wait().expect("the agent is reaped");

            assert!(
                job_reaches(&agent, |alive| alive == NO_PROCESSES),
                "a background process left by the agent must not survive cleanup"
            );
        }
    }

    #[cfg(target_os = "linux")]
    mod linux_pidfd {
        use super::super::OwnedProcess;
        use super::super::test_support::a_long_running_process;

        #[test]
        fn the_owned_process_is_identified_by_a_pidfd_on_linux() {
            let mut agent =
                OwnedProcess::spawn(&a_long_running_process()).expect("sh is available");

            let held: bool = agent.is_held_by_pidfd();
            agent.kill_tree().expect("ending the process succeeds");

            assert!(
                held,
                "on Linux 5.3+ (every supported CI runner) a reused PID can never be signalled"
            );
        }
    }
}
