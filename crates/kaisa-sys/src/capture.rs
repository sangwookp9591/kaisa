//! Running a command to completion with its output captured, a deadline and an output cap
//! (architecture §5.6 broker `exec` contract).

use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::process::{OwnedProcess, ProcessExit, ProcessSpec, command_for};

/// The OS confinement a captured run gets.
#[derive(Debug)]
#[non_exhaustive]
pub enum Confinement<'policy> {
    /// No OS confinement; the caller must label the run as reduced.
    Unconfined,
    /// A Seatbelt profile (macOS).
    #[cfg(target_os = "macos")]
    Seatbelt(&'policy crate::seatbelt::SeatbeltPolicy),
    /// A Landlock ruleset with its UDP filter (Linux).
    #[cfg(target_os = "linux")]
    Landlock(&'policy crate::landlock::LandlockRuleset),
    /// Keeps the lifetime on platforms whose confinement (Windows AppContainer, T15.5) is not
    /// implemented yet; never constructed.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[doc(hidden)]
    Pending(std::marker::PhantomData<&'policy ()>),
}

/// How a captured run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRun {
    /// The exit, or `None` if the deadline passed and the tree was ended.
    pub exit: Option<ProcessExit>,
    /// Standard output and error, interleaved, up to the cap.
    pub output: Vec<u8>,
    /// True if output beyond the cap was discarded.
    pub truncated: bool,
}

/// Runs `spec` under `confinement`, waiting at most `deadline` and keeping at most `cap` bytes of
/// output. Output beyond the cap is read and dropped, so a chatty process cannot stall on a full
/// pipe. A run past its deadline is ended with its whole process tree.
///
/// # Errors
///
/// Returns the operating system's error if the program cannot be started or waited for.
pub fn run_captured(
    spec: &ProcessSpec,
    confinement: &Confinement<'_>,
    deadline: Duration,
    cap: usize,
) -> io::Result<CapturedRun> {
    let (reader, writer) = io::pipe()?;
    let mut command: Command = confined_command(spec, confinement);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(writer.try_clone()?))
        .stderr(Stdio::from(writer));
    let mut process: OwnedProcess = OwnedProcess::spawn_owned(command)?;
    let collector = std::thread::spawn(move || collect(reader, cap));
    let exit: Option<ProcessExit> = process.wait_timeout(deadline)?;
    // Ends the tree on timeout, and any leftovers that would keep the pipe open otherwise.
    process.kill_tree()?;
    if exit.is_none() {
        process.wait()?;
    }
    let (output, truncated) = collector
        .join()
        .map_err(|_| io::Error::other("the output collector panicked"))??;
    Ok(CapturedRun {
        exit,
        output,
        truncated,
    })
}

fn collect(mut reader: io::PipeReader, cap: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut output: Vec<u8> = Vec::new();
    let mut truncated: bool = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let read: usize = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok((output, truncated));
        }
        let room: usize = cap.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(room)]);
        truncated |= read > room;
    }
}

fn confined_command(spec: &ProcessSpec, confinement: &Confinement<'_>) -> Command {
    match confinement {
        Confinement::Unconfined => command_for(spec),
        #[cfg(target_os = "macos")]
        Confinement::Seatbelt(policy) => {
            command_for(&crate::seatbelt::sandboxed_spec(spec, policy))
        }
        #[cfg(target_os = "linux")]
        Confinement::Landlock(ruleset) => {
            use std::os::unix::process::CommandExt;

            let mut command: Command = command_for(spec);
            let fd = ruleset.raw_fd();
            let udp_filter: Option<(usize, u16)> = ruleset.udp_filter_parts();
            #[allow(unsafe_code)]
            // SAFETY: the closure runs in the forked child before `exec` and only calls `prctl`
            // and the `landlock_restrict_self` syscall, which are async-signal-safe; the filter
            // lives in `ruleset`, which outlives the spawn.
            unsafe {
                command.pre_exec(move || crate::landlock::restrict_self(fd, udp_filter));
            }
            command
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        Confinement::Pending(_) => command_for(spec),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{CapturedRun, Confinement, run_captured};
    use crate::process::ProcessExit;
    use crate::process::test_support::{a_long_running_process, shell};

    /// Plenty for a shell that prints one line.
    const DEADLINE: Duration = Duration::from_secs(30);
    /// Far shorter than a long-running test process lives.
    const A_MOMENT: Duration = Duration::from_millis(200);
    /// A small cap, so a modest output overflows it.
    const SMALL_CAP: usize = 16;
    /// What the command prints.
    const GREETING: &str = "hello-from-exec";

    #[test]
    fn an_exec_reports_its_exit_and_output() {
        let run: CapturedRun = run_captured(
            &shell(&format!("echo {GREETING}")),
            &Confinement::Unconfined,
            DEADLINE,
            usize::MAX,
        )
        .expect("the shell runs");

        assert_eq!(
            (run.exit, String::from_utf8_lossy(&run.output).trim()),
            (Some(ProcessExit::Code(0)), GREETING),
            "the agent sees what its command printed and how it ended"
        );
    }

    #[test]
    fn an_exec_past_its_deadline_is_ended() {
        let run: CapturedRun = run_captured(
            &a_long_running_process(),
            &Confinement::Unconfined,
            A_MOMENT,
            usize::MAX,
        )
        .expect("the process starts");

        assert_eq!(
            run.exit, None,
            "a command that hangs must not hang the agent's turn"
        );
    }

    #[test]
    fn exec_output_is_cut_at_its_cap() {
        let run: CapturedRun = run_captured(
            &shell(&format!("echo {GREETING}{GREETING}{GREETING}")),
            &Confinement::Unconfined,
            DEADLINE,
            SMALL_CAP,
        )
        .expect("the shell runs");

        assert_eq!(
            (run.output.len(), run.truncated),
            (SMALL_CAP, true),
            "a huge log must not flood the agent's context, and the cut is visible"
        );
    }
}
