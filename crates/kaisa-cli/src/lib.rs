//! Kaisa command-line front end.
//!
//! Design: `docs/user-flows.md` §7. Only `--version` and `--help` exist so far; every
//! other command reports that it is not implemented yet instead of pretending to work.

use std::io;
use std::io::Write;

mod delivery;
mod goal;
mod watch;

pub use delivery::{EXIT_WAITING_FOR_HUMAN, report_delivery};

/// Exit status for a command that completed successfully.
pub const EXIT_SUCCESS: u8 = 0;

/// Exit status for a command that has not been implemented yet.
pub const EXIT_NOT_IMPLEMENTED: u8 = 2;

/// Exit status for a command that ran and failed.
pub const EXIT_FAILURE: u8 = 1;

/// Exit status for a command used with the wrong arguments.
pub const EXIT_USAGE: u8 = 64;

/// Exit status of a goal whose agent failed or whose work was kept back (CI contract, F26).
pub const EXIT_RUN_FAILED: u8 = 4;

/// Command names reserved for later milestones; they answer "not implemented" instead of being
/// mistaken for a goal.
const PLANNED_COMMANDS: [&str; 17] = [
    "review", "ask", "attach", "steer", "pause", "resume", "cancel", "undo", "restore", "init",
    "doctor", "status", "console", "memory", "agent", "team", "admin",
];

/// The version of this build, taken from the workspace manifest (single source of truth).
const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
Kaisa — run coding agents safely (M0: one brokered agent per goal; more is planned)

Usage:
  kaisa \"<goal>\"     run one agent on the goal in this repository and preserve its work
  kaisa watch        follow progress       (planned)
  kaisa review       approve results       (planned)

Options:
  -h, --help         show this help
  -V, --version      show the version
";

/// Runs the CLI with `args` (excluding the program name) and returns the exit status.
///
/// Output goes to `out` and diagnostics to `err`, so callers and tests choose the sinks.
///
/// # Errors
///
/// Returns an error only if writing to `out` or `err` fails.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => {
            out.write_all(HELP.as_bytes())?;
            Ok(EXIT_SUCCESS)
        }
        Some("-V" | "--version" | "version") => {
            writeln!(out, "kaisa {VERSION}")?;
            Ok(EXIT_SUCCESS)
        }
        Some("watch") => watch::run(out, err),
        Some("mcp") => match (args.get(1).map(String::as_str), args.get(2)) {
            (Some("--attempt"), Some(directory)) => {
                match kaisa_mcp::bridge(std::path::Path::new(directory), out) {
                    Ok(()) => Ok(EXIT_SUCCESS),
                    Err(error) => {
                        writeln!(err, "kaisa mcp: {error}")?;
                        Ok(EXIT_FAILURE)
                    }
                }
            }
            _ => {
                writeln!(err, "usage: kaisa mcp --attempt <attempt directory>")?;
                Ok(EXIT_USAGE)
            }
        },
        Some(command) if PLANNED_COMMANDS.contains(&command) => {
            writeln!(
                err,
                "kaisa: `{command}` is not implemented yet. See docs/user-flows.md for the plan."
            )?;
            Ok(EXIT_NOT_IMPLEMENTED)
        }
        Some(_) => match goal::GoalCommand::parse(args) {
            Some(command) => command.run(out, err),
            None => {
                writeln!(err, "usage: kaisa \"<goal>\"")?;
                Ok(EXIT_USAGE)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{EXIT_NOT_IMPLEMENTED, EXIT_SUCCESS, VERSION, run};

    /// What a user sees after running one `kaisa` command line.
    struct Transcript {
        status: u8,
        stdout: String,
        stderr: String,
    }

    /// Runs the CLI as if the user typed `kaisa <words…>`.
    fn user_types(words: &[&str]) -> Transcript {
        let args: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
        let mut stdout: Vec<u8> = Vec::new();
        let mut stderr: Vec<u8> = Vec::new();
        let status: u8 =
            run(&args, &mut stdout, &mut stderr).expect("writing to memory cannot fail");
        Transcript {
            status,
            stdout: String::from_utf8(stdout).expect("CLI output is UTF-8"),
            stderr: String::from_utf8(stderr).expect("CLI diagnostics are UTF-8"),
        }
    }

    #[test]
    fn asking_for_the_version_prints_the_workspace_version_and_succeeds() {
        let transcript: Transcript = user_types(&["--version"]);

        assert_eq!(
            transcript.status, EXIT_SUCCESS,
            "showing the version is not an error"
        );
        assert_eq!(
            transcript.stdout,
            format!("kaisa {VERSION}\n"),
            "the version comes from the workspace manifest, the single source of truth"
        );
    }

    #[test]
    fn running_kaisa_without_arguments_shows_help_with_the_three_verbs() {
        let transcript: Transcript = user_types(&[]);

        assert_eq!(
            transcript.status, EXIT_SUCCESS,
            "bare `kaisa` must not fail"
        );
        for verb in ["kaisa \"<goal>\"", "kaisa watch", "kaisa review"] {
            assert!(
                transcript.stdout.contains(verb),
                "help must teach the three-verb surface (user-flows U1), missing `{verb}`"
            );
        }
    }

    #[test]
    fn an_unimplemented_command_fails_honestly_instead_of_pretending_to_work() {
        let transcript: Transcript = user_types(&["review"]);

        assert_eq!(
            transcript.status, EXIT_NOT_IMPLEMENTED,
            "scripts must be able to tell that nothing happened"
        );
        assert!(
            transcript.stdout.is_empty(),
            "no fake output may appear on stdout"
        );
        assert!(
            transcript.stderr.contains("not implemented yet"),
            "the user must be told why nothing happened"
        );
    }
}
