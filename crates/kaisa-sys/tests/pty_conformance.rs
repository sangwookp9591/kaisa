//! PTY conformance suite (architecture §16, ADR 0005).
//!
//! Every scenario is written once against the test-only [`Backend`] trait, so another PTY
//! library can run the same scenarios through its own API (T12.9) without changing Kaisa's
//! `PseudoTerminal`. A scenario starts an agent, optionally types or resizes, and then finishes:
//! the backend waits for the agent and returns everything it printed. Nothing reads in between,
//! so `finish` alone must recover the output, however much the agent wrote before exiting.

use std::io::Write;
use std::path::PathBuf;

use kaisa_sys::{OwnedProcess, ProcessSpec, PseudoTerminal, Terminal, TerminalSize};

/// Rows of a classic text terminal.
const CLASSIC_ROWS: u16 = 24;
/// Columns of a classic text terminal.
const CLASSIC_COLUMNS: u16 = 80;
/// Rows after the user enlarges the window; differs from the classic size on purpose.
const ENLARGED_ROWS: u16 = 40;
/// Columns after the user enlarges the window.
const ENLARGED_COLUMNS: u16 = 132;
/// A greeting the agent prints.
const GREETING: &str = "hello-from-the-agent";
/// A line a user types.
const TYPED_LINE: &str = "typed-by-the-user";
/// Put in front of the line the agent read, so its reply differs from the terminal's echo.
const REPLY_PREFIX: &str = "agent-read:";
/// Korean text and an emoji: multi-byte UTF-8, including a character outside the BMP.
const UNICODE_LINE: &str = "안녕 카이사 🚀";
/// The last thing the agent prints before it exits.
const FAREWELL: &str = "goodbye-from-the-agent";
/// Lines printed before the farewell: far more than a terminal buffers (about 4 KiB on macOS),
/// so the agent can only finish if the backend reads while it waits.
const LINES_BEFORE_FAREWELL: u32 = 2000;
/// How much of a long output a failure message shows.
const OUTPUT_TAIL_SHOWN: usize = 300;
/// The key that ends a typed line: a newline for a Unix terminal, a carriage return for a
/// Windows console.
const ENTER: &str = if cfg!(windows) { "\r" } else { "\n" };

/// One way to run an agent in a pseudo-terminal.
trait Backend {
    /// A running agent and its terminal.
    type Session: Session;

    /// Starts `spec` in a new terminal of `size`.
    fn start(spec: &ProcessSpec, size: TerminalSize) -> Self::Session;
}

/// What a scenario can do with a running agent.
trait Session {
    /// Types `text` into the agent's terminal.
    fn type_text(&mut self, text: &str);

    /// Changes the terminal's size.
    fn resize(&mut self, size: TerminalSize);

    /// Waits for the agent to exit and returns everything it printed.
    fn finish(self) -> String;
}

/// Kaisa's own terminals: `OwnedProcess::spawn_in_terminal` and `PseudoTerminal::drain`.
struct KaisaBackend;

struct KaisaSession {
    agent: OwnedProcess,
    terminal: Terminal,
}

impl Backend for KaisaBackend {
    type Session = KaisaSession;

    fn start(spec: &ProcessSpec, size: TerminalSize) -> KaisaSession {
        let (agent, terminal) = OwnedProcess::spawn_in_terminal(spec, size)
            .expect("every CI runner can start its shell in a terminal");
        KaisaSession { agent, terminal }
    }
}

impl Session for KaisaSession {
    fn type_text(&mut self, text: &str) {
        self.terminal
            .writer()
            .expect("the terminal can be written")
            .write_all(text.as_bytes())
            .expect("typing succeeds");
    }

    fn resize(&mut self, size: TerminalSize) {
        self.terminal.resize(size).expect("resizing succeeds");
    }

    fn finish(self) -> String {
        let KaisaSession {
            mut agent,
            terminal,
        } = self;
        let output: Vec<u8> = terminal
            .drain(&mut agent)
            .expect("the output can be drained");
        agent.kill_tree().expect("leftovers are cleaned up");
        String::from_utf8_lossy(&output).into_owned()
    }
}

/// `portable-pty` (wezterm), the mature wrapper ADR 0005 compares against. It has no drain of
/// its own, so this adapter reads on a thread from the start of `finish`, waits for the child,
/// and stops once no output arrived for a quiet period — the work a Kaisa caller gets from
/// `PseudoTerminal::drain`.
struct PortablePtyBackend;

struct PortablePtySession {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Box<dyn portable_pty::MasterPty + Send>,
}

/// How long the portable-pty adapter waits for more output after the child exited.
const PORTABLE_QUIET_PERIOD: std::time::Duration = std::time::Duration::from_millis(500);

fn portable_size(size: TerminalSize) -> portable_pty::PtySize {
    portable_pty::PtySize {
        rows: size.rows(),
        cols: size.columns(),
        pixel_width: 0,
        pixel_height: 0,
    }
}

impl Backend for PortablePtyBackend {
    type Session = PortablePtySession;

    fn start(spec: &ProcessSpec, size: TerminalSize) -> PortablePtySession {
        let pair = portable_pty::native_pty_system()
            .openpty(portable_size(size))
            .expect("portable-pty opens a terminal");
        let mut command = portable_pty::CommandBuilder::new(&spec.program);
        command.args(&spec.args);
        command.cwd(&spec.working_directory);
        let child = pair
            .slave
            .spawn_command(command)
            .expect("portable-pty starts the shell");
        drop(pair.slave);
        PortablePtySession {
            child,
            master: pair.master,
        }
    }
}

impl Session for PortablePtySession {
    fn type_text(&mut self, text: &str) {
        self.master
            .take_writer()
            .expect("the terminal can be written")
            .write_all(text.as_bytes())
            .expect("typing succeeds");
    }

    fn resize(&mut self, size: TerminalSize) {
        self.master
            .resize(portable_size(size))
            .expect("resizing succeeds");
    }

    fn finish(self) -> String {
        let mut reader = self
            .master
            .try_clone_reader()
            .expect("the terminal can be read");
        let (chunks, received) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            while let Ok(read) = std::io::Read::read(&mut reader, &mut buffer) {
                if read == 0 || chunks.send(buffer[..read].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut output: Vec<u8> = Vec::new();
        let mut child = self.child;
        let waiter = std::thread::spawn(move || child.wait());
        while !waiter.is_finished() {
            if let Ok(chunk) = received.recv_timeout(PORTABLE_QUIET_PERIOD) {
                output.extend(chunk);
            }
        }
        while let Ok(chunk) = received.recv_timeout(PORTABLE_QUIET_PERIOD) {
            output.extend(chunk);
        }
        drop(self.master);
        String::from_utf8_lossy(&output).into_owned()
    }
}

fn size(rows: u16, columns: u16) -> TerminalSize {
    TerminalSize::new(rows, columns).expect("test sizes are valid")
}

/// `script` run by the platform shell.
fn shell(script: &str) -> ProcessSpec {
    let (program, args): (&str, Vec<String>) = if cfg!(windows) {
        (
            "cmd",
            vec!["/V:ON".to_owned(), "/C".to_owned(), script.to_owned()],
        )
    } else {
        ("sh", vec!["-c".to_owned(), script.to_owned()])
    };
    ProcessSpec {
        program: PathBuf::from(program),
        args,
        working_directory: std::env::temp_dir(),
    }
}

fn an_agent_that_greets() -> ProcessSpec {
    shell(&format!("echo {GREETING}"))
}

fn an_agent_that_answers_one_typed_line() -> ProcessSpec {
    if cfg!(windows) {
        // `chcp 65001` switches the console to UTF-8, so non-ASCII input is not mangled.
        shell(&format!(
            "chcp 65001 >nul && set /p line=&& echo {REPLY_PREFIX}!line!"
        ))
    } else {
        shell(&format!("read line; echo \"{REPLY_PREFIX}$line\""))
    }
}

/// Waits for one typed line, then prints its terminal size as "rows columns".
fn an_agent_that_reports_its_size_after_a_line() -> ProcessSpec {
    if cfg!(windows) {
        ProcessSpec {
            program: PathBuf::from("powershell"),
            args: vec![
                "-NoProfile".to_owned(),
                "-Command".to_owned(),
                "$null = Read-Host; '{0} {1}' -f [Console]::WindowHeight, [Console]::WindowWidth"
                    .to_owned(),
            ],
            working_directory: std::env::temp_dir(),
        }
    } else {
        shell("read resized; stty size")
    }
}

fn an_agent_that_prints_a_lot_and_exits_at_once() -> ProcessSpec {
    if cfg!(windows) {
        // A newline would end `cmd /C`'s command, and `&` inside `do` would join the loop
        // body, so the loop is parenthesised and chained with `&`.
        shell(&format!(
            "(for /L %i in (1,1,{LINES_BEFORE_FAREWELL}) do @echo line %i) & echo {FAREWELL}"
        ))
    } else {
        shell(&format!(
            "i=0; while [ $i -lt {LINES_BEFORE_FAREWELL} ]; do echo line $i; i=$((i+1)); done; echo {FAREWELL}"
        ))
    }
}

fn scenario_prints<B: Backend>() {
    let session = B::start(&an_agent_that_greets(), size(CLASSIC_ROWS, CLASSIC_COLUMNS));

    let output: String = session.finish();

    assert!(
        output.contains(GREETING),
        "the keeper records what the agent shows, got {output:?}"
    );
}

fn scenario_typed_input<B: Backend>() {
    let reply: String = format!("{REPLY_PREFIX}{TYPED_LINE}");
    let mut session = B::start(
        &an_agent_that_answers_one_typed_line(),
        size(CLASSIC_ROWS, CLASSIC_COLUMNS),
    );

    session.type_text(&format!("{TYPED_LINE}{ENTER}"));
    let output: String = session.finish();

    assert!(
        output.contains(&reply),
        "steering an agent means typing into its terminal, got {output:?}"
    );
}

fn scenario_unicode<B: Backend>() {
    let reply: String = format!("{REPLY_PREFIX}{UNICODE_LINE}");
    let mut session = B::start(
        &an_agent_that_answers_one_typed_line(),
        size(CLASSIC_ROWS, CLASSIC_COLUMNS),
    );

    session.type_text(&format!("{UNICODE_LINE}{ENTER}"));
    let output: String = session.finish();

    assert!(
        output.contains(&reply),
        "users steer agents in Korean too; text must survive the terminal intact, got {output:?}"
    );
}

fn scenario_resize<B: Backend>() {
    let size_as_the_agent_reports_it: String = format!("{ENLARGED_ROWS} {ENLARGED_COLUMNS}");
    let mut session = B::start(
        &an_agent_that_reports_its_size_after_a_line(),
        size(CLASSIC_ROWS, CLASSIC_COLUMNS),
    );

    session.resize(size(ENLARGED_ROWS, ENLARGED_COLUMNS));
    session.type_text(ENTER);
    let output: String = session.finish();

    assert!(
        output.contains(&size_as_the_agent_reports_it),
        "a TUI agent must redraw for the window the user actually has, got {output:?}"
    );
}

fn scenario_output_before_exit<B: Backend>() {
    let session = B::start(
        &an_agent_that_prints_a_lot_and_exits_at_once(),
        size(CLASSIC_ROWS, CLASSIC_COLUMNS),
    );

    let output: String = session.finish();

    let ending: String = output
        .chars()
        .rev()
        .take(OUTPUT_TAIL_SHOWN)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    assert!(
        output.contains(FAREWELL),
        "an agent's last words must survive however much it printed before them; output ended with {ending:?}"
    );
}

/// Instantiates every scenario for one backend.
macro_rules! conformance_suite {
    ($module:ident, $backend:ty) => {
        mod $module {
            use super::{
                scenario_output_before_exit, scenario_prints, scenario_resize,
                scenario_typed_input, scenario_unicode,
            };

            #[test]
            fn a_backend_shows_what_the_agent_prints() {
                scenario_prints::<$backend>();
            }

            #[test]
            fn a_backend_delivers_typed_input() {
                scenario_typed_input::<$backend>();
            }

            #[test]
            fn unicode_text_survives_the_round_trip_through_the_terminal() {
                scenario_unicode::<$backend>();
            }

            #[test]
            fn a_backend_reports_a_resize_to_the_agent() {
                scenario_resize::<$backend>();
            }

            #[test]
            fn a_backend_keeps_output_written_just_before_exit() {
                scenario_output_before_exit::<$backend>();
            }
        }
    };
}

conformance_suite!(kaisa, super::KaisaBackend);
conformance_suite!(portable_pty_wezterm, super::PortablePtyBackend);
