//! Agents started inside a pseudo-terminal (architecture §5.1): the keeper holds Kaisa's side,
//! the agent sees an ordinary terminal. Unix uses `openpty` and makes the terminal the agent's
//! controlling terminal; Windows uses a ConPTY pseudo console.

use std::fs::File;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::OwnedHandle;
#[cfg(unix)]
use std::process::{Command, Stdio};

#[cfg(windows)]
use crate::conpty::{self, OpenedConsole, PseudoConsole};
#[cfg(unix)]
use crate::openpty::{self, TerminalPair};
#[cfg(unix)]
use crate::process::command_for;
use crate::process::{OwnedProcess, ProcessSpec};
use crate::pty::{PseudoTerminal, TerminalSize};

/// Kaisa's side of the pseudo-terminal an agent runs in.
#[derive(Debug)]
pub struct Terminal {
    /// The master side: the agent's output is read from it and its input written to it.
    #[cfg(unix)]
    master: OwnedFd,
    /// The pseudo console. Declared first so it closes before the pipes on drop.
    #[cfg(windows)]
    console: PseudoConsole,
    /// Writing here types into the agent's console.
    #[cfg(windows)]
    input: OwnedHandle,
    /// Reading here yields what the agent draws.
    #[cfg(windows)]
    output: OwnedHandle,
    size: TerminalSize,
}

impl PseudoTerminal for Terminal {
    fn reader(&self) -> io::Result<Box<dyn Read + Send>> {
        #[cfg(unix)]
        let handle = self.master.try_clone()?;
        #[cfg(windows)]
        let handle = self.output.try_clone()?;
        Ok(Box::new(File::from(handle)))
    }

    fn writer(&self) -> io::Result<Box<dyn Write + Send>> {
        #[cfg(unix)]
        let handle = self.master.try_clone()?;
        #[cfg(windows)]
        let handle = self.input.try_clone()?;
        Ok(Box::new(File::from(handle)))
    }

    fn resize(&mut self, size: TerminalSize) -> io::Result<()> {
        #[cfg(unix)]
        openpty::set_size(&self.master, size)?;
        #[cfg(windows)]
        self.console.resize(size)?;
        self.size = size;
        Ok(())
    }

    fn size(&self) -> TerminalSize {
        self.size
    }
}

impl OwnedProcess {
    /// Starts `spec` with a new pseudo-terminal of `size` as its standard input, output and
    /// error, and as its controlling terminal. The process starts a new session, so it also
    /// leads its own process group and [`kill_tree`](Self::kill_tree) still reaches everything
    /// it starts.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the terminal cannot be opened or the program
    /// cannot be started.
    #[cfg(unix)]
    pub fn spawn_in_terminal(
        spec: &ProcessSpec,
        size: TerminalSize,
    ) -> io::Result<(Self, Terminal)> {
        let pair: TerminalPair = openpty::open(size)?;
        let mut command: Command = command_for(spec);
        command
            .stdin(Stdio::from(pair.slave.try_clone()?))
            .stdout(Stdio::from(pair.slave.try_clone()?))
            .stderr(Stdio::from(pair.slave));
        #[allow(unsafe_code)]
        // SAFETY: the closure runs in the forked child before `exec` and only calls `setsid`
        // and `ioctl`, which are async-signal-safe, and reads `errno` through std.
        unsafe {
            command.pre_exec(become_session_leader_of_its_terminal);
        }
        // `process_group(0)` is not used: `setsid` fails for a process that already leads a
        // group, and a new session makes the child lead a new group anyway.
        let process: OwnedProcess = OwnedProcess::spawn_prepared(command)?;
        Ok((
            process,
            Terminal {
                master: pair.master,
                size,
            },
        ))
    }
}

impl OwnedProcess {
    /// Starts `spec` attached to a new pseudo console of `size`, inside its own Job Object,
    /// so [`kill_tree`](Self::kill_tree) still reaches everything it starts.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the console cannot be opened or the program
    /// cannot be started.
    #[cfg(windows)]
    pub fn spawn_in_terminal(
        spec: &ProcessSpec,
        size: TerminalSize,
    ) -> io::Result<(Self, Terminal)> {
        let OpenedConsole {
            console,
            input,
            output,
        } = conpty::open(size)?;
        let created = crate::console_process::create_suspended_in(&console, spec)?;
        let process: OwnedProcess = OwnedProcess::adopt_created(created)?;
        Ok((
            process,
            Terminal {
                console,
                input,
                output,
                size,
            },
        ))
    }
}

/// Runs in the child: starts a new session and makes standard input (the slave) its
/// controlling terminal, so the agent behaves exactly as in a user's terminal window.
#[cfg(unix)]
fn become_session_leader_of_its_terminal() -> io::Result<()> {
    #[allow(unsafe_code)]
    // SAFETY: `setsid` takes no arguments and touches no caller memory.
    let session: libc::pid_t = unsafe { libc::setsid() };
    if session == -1 {
        return Err(io::Error::last_os_error());
    }
    let no_steal: libc::c_int = 0;
    // The request constant is `u32` on macOS but `ioctl` takes a `c_ulong` there; on Linux
    // both already agree.
    #[allow(clippy::cast_lossless, clippy::unnecessary_cast)]
    let set_controlling_terminal = libc::TIOCSCTTY as _;
    #[allow(unsafe_code)]
    // SAFETY: TIOCSCTTY takes an integer argument by value; no memory is passed.
    let result: libc::c_int =
        unsafe { libc::ioctl(libc::STDIN_FILENO, set_controlling_terminal, no_steal) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    #[cfg(unix)]
    use std::path::PathBuf;
    use std::sync::mpsc::{Receiver, channel};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::Terminal;
    use crate::process::test_support::shell;
    #[cfg(unix)]
    use crate::process::test_support::{
        a_fresh_pid_file, a_process_that_starts_a_background_child, disappears_within_patience,
        wait_for_pid_in,
    };
    #[cfg(windows)]
    use crate::process::test_support::{
        a_process_that_starts_a_background_child_on_windows, job_reaches,
    };
    use crate::process::{OwnedProcess, ProcessSpec};
    use crate::pty::{PseudoTerminal, TerminalSize};

    /// Rows after the user enlarges the window; differs from the classic size on purpose.
    #[cfg(unix)]
    const ENLARGED_ROWS: u16 = 40;
    /// Columns after the user enlarges the window.
    #[cfg(unix)]
    const ENLARGED_COLUMNS: u16 = 132;
    /// Rows of a classic text terminal.
    const CLASSIC_ROWS: u16 = 24;
    /// Columns of a classic text terminal.
    const CLASSIC_COLUMNS: u16 = 80;
    /// How long a test waits for expected output.
    const PATIENCE: Duration = Duration::from_secs(10);
    /// What the agent prints when its standard input is a terminal.
    #[cfg(unix)]
    const SEES_A_TERMINAL: &str = "stdin-is-a-terminal";
    /// A greeting the agent prints.
    const GREETING: &str = "hello-from-the-agent";
    /// A line a user types into the agent's terminal.
    const TYPED_LINE: &str = "typed-by-the-user";
    /// The prefix the agent puts in front of the line it read, so its reply can be told apart
    /// from the terminal's echo of the typed line.
    const REPLY_PREFIX: &str = "agent-read:";
    /// The key a user presses to finish a line: a terminal's line discipline expects a
    /// newline, a Windows console expects a carriage return.
    const ENTER: &str = if cfg!(windows) { "\r" } else { "\n" };
    /// The agent and the background child it started, both inside its job.
    #[cfg(windows)]
    const AGENT_AND_ITS_CHILD: u32 = 2;
    /// A job whose processes have all ended.
    #[cfg(windows)]
    const NO_PROCESSES: u32 = 0;

    fn a_classic_size() -> TerminalSize {
        TerminalSize::new(CLASSIC_ROWS, CLASSIC_COLUMNS).expect("a classic size is valid")
    }

    fn started_in_a_terminal(spec: &ProcessSpec) -> (OwnedProcess, Terminal) {
        OwnedProcess::spawn_in_terminal(spec, a_classic_size())
            .expect("every CI runner can start its shell in a terminal")
    }

    /// An agent that reads one typed line and answers with it behind [`REPLY_PREFIX`].
    fn an_agent_that_answers_one_typed_line() -> ProcessSpec {
        if cfg!(windows) {
            // Delayed expansion (`/V:ON`, `!line!`) reads the variable after `set /p` ran.
            let mut agent: ProcessSpec =
                shell(&format!("set /p line=&& echo {REPLY_PREFIX}!line!"));
            agent.args.insert(0, "/V:ON".to_owned());
            agent
        } else {
            shell(&format!("read line; echo \"{REPLY_PREFIX}$line\""))
        }
    }

    /// Everything the agent printed, collected until `expected` appears or patience runs out.
    fn output_until(terminal: &Terminal, expected: &str) -> String {
        let mut reader = terminal.reader().expect("the terminal can be read");
        let (chunks, received): (_, Receiver<Vec<u8>>) = channel();
        thread::spawn(move || {
            let mut buffer = [0_u8; 1024];
            // Ends at end of file, or with EIO on Linux once the agent's side is closed.
            while let Ok(read) = reader.read(&mut buffer) {
                if read == 0 || chunks.send(buffer[..read].to_vec()).is_err() {
                    break;
                }
            }
        });
        let deadline: Instant = Instant::now() + PATIENCE;
        let mut output: Vec<u8> = Vec::new();
        while !String::from_utf8_lossy(&output).contains(expected) {
            let left: Duration = deadline.saturating_duration_since(Instant::now());
            match received.recv_timeout(left) {
                Ok(chunk) => output.extend(chunk),
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&output).into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn an_agent_in_a_terminal_sees_a_terminal_on_its_standard_input() {
        let (mut agent, terminal) = started_in_a_terminal(&shell(&format!(
            "if [ -t 0 ]; then echo {SEES_A_TERMINAL}; else echo stdin-is-not-a-terminal; fi"
        )));

        let output: String = output_until(&terminal, SEES_A_TERMINAL);
        agent.wait().expect("the agent exits by itself");

        assert!(
            output.contains(SEES_A_TERMINAL),
            "interactive agent CLIs only draw their UI on a terminal, got {output:?}"
        );
    }

    #[test]
    fn what_an_agent_prints_can_be_read_from_the_terminal() {
        let (mut agent, terminal) = started_in_a_terminal(&shell(&format!("echo {GREETING}")));

        let output: String = output_until(&terminal, GREETING);
        agent.wait().expect("the agent exits by itself");

        assert!(
            output.contains(GREETING),
            "the keeper records what the agent shows, got {output:?}"
        );
    }

    #[test]
    fn what_is_typed_into_the_terminal_reaches_the_agent() {
        let reply: String = format!("{REPLY_PREFIX}{TYPED_LINE}");
        let (mut agent, terminal) = started_in_a_terminal(&an_agent_that_answers_one_typed_line());

        let mut keyboard = terminal.writer().expect("the terminal can be written");
        keyboard
            .write_all(format!("{TYPED_LINE}{ENTER}").as_bytes())
            .expect("typing succeeds");
        let output: String = output_until(&terminal, &reply);
        agent
            .wait()
            .expect("the agent exits after reading one line");

        assert!(
            output.contains(&reply),
            "steering an agent means typing into its terminal, got {output:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn killing_the_tree_of_an_agent_in_a_terminal_also_ends_a_grandchild() {
        let pid_file: PathBuf = a_fresh_pid_file();
        let (mut agent, _terminal) =
            started_in_a_terminal(&a_process_that_starts_a_background_child(&pid_file));
        let grandchild: libc::pid_t = wait_for_pid_in(&pid_file);

        agent.kill_tree().expect("ending the tree succeeds");
        agent.wait().expect("the agent is reaped");
        let _ = std::fs::remove_file(&pid_file);

        assert!(
            disappears_within_patience(grandchild),
            "a terminal session must not let background processes escape cleanup"
        );
    }

    #[cfg(windows)]
    #[test]
    fn killing_the_tree_of_an_agent_in_a_terminal_also_ends_a_grandchild() {
        let (mut agent, _terminal) =
            started_in_a_terminal(&a_process_that_starts_a_background_child_on_windows());
        assert!(
            job_reaches(&agent, |alive| alive >= AGENT_AND_ITS_CHILD),
            "the background child must start before the test can end it"
        );

        agent.kill_tree().expect("ending the tree succeeds");
        agent.wait().expect("the agent is reaped");

        assert!(
            job_reaches(&agent, |alive| alive == NO_PROCESSES),
            "a console session must not let background processes escape cleanup"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_agent_sees_the_new_size_after_the_terminal_is_resized() {
        let enlarged: TerminalSize =
            TerminalSize::new(ENLARGED_ROWS, ENLARGED_COLUMNS).expect("an enlarged size is valid");
        // `stty size` prints "rows columns" as the agent's terminal reports them.
        let size_as_the_agent_reports_it: String = format!("{ENLARGED_ROWS} {ENLARGED_COLUMNS}");
        let (mut agent, mut terminal) = started_in_a_terminal(&shell("read resized; stty size"));

        terminal.resize(enlarged).expect("resizing succeeds");
        terminal
            .writer()
            .expect("the terminal can be written")
            .write_all(b"\n")
            .expect("typing succeeds");
        let output: String = output_until(&terminal, &size_as_the_agent_reports_it);
        agent
            .wait()
            .expect("the agent exits after reporting its size");

        assert!(
            output.contains(&size_as_the_agent_reports_it),
            "a TUI agent must redraw for the window the user actually has, got {output:?}"
        );
        assert_eq!(terminal.size(), enlarged, "Kaisa remembers the size it set");
    }
}
