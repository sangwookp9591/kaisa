//! Kaisa command-line front end.
//!
//! Design: `docs/user-flows.md` §7. Only `--version` and `--help` exist so far; every
//! other command reports that it is not implemented yet instead of pretending to work.

use std::io;
use std::io::Write;

/// Exit status for a command that has not been implemented yet.
pub const EXIT_NOT_IMPLEMENTED: u8 = 2;

/// The version of this build, taken from the workspace manifest (single source of truth).
const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
Kaisa — run coding agents safely in parallel (design stage, nothing implemented yet)

Usage:
  kaisa \"<goal>\"     start a task          (planned)
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
            Ok(0)
        }
        Some("-V" | "--version" | "version") => {
            writeln!(out, "kaisa {VERSION}")?;
            Ok(0)
        }
        Some(command) => {
            writeln!(
                err,
                "kaisa: `{command}` is not implemented yet. See docs/user-flows.md for the plan."
            )?;
            Ok(EXIT_NOT_IMPLEMENTED)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EXIT_NOT_IMPLEMENTED, run};

    fn run_with(args: &[&str]) -> (u8, String, String) {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let status: u8 = run(&args, &mut out, &mut err).expect("writing to a Vec cannot fail");
        let out = String::from_utf8(out).expect("help and version output is UTF-8");
        let err = String::from_utf8(err).expect("diagnostics are UTF-8");
        (status, out, err)
    }

    #[test]
    fn version_prints_the_workspace_version() {
        let (status, out, _) = run_with(&["--version"]);
        assert_eq!(status, 0, "--version must succeed");
        assert_eq!(
            out,
            format!("kaisa {}\n", env!("CARGO_PKG_VERSION")),
            "version line"
        );
    }

    #[test]
    fn no_arguments_prints_help() {
        let (status, out, _) = run_with(&[]);
        assert_eq!(status, 0, "bare `kaisa` must succeed");
        assert!(
            out.contains("kaisa watch"),
            "help must list the three verbs"
        );
    }

    #[test]
    fn unknown_commands_are_not_faked() {
        let (status, out, err) = run_with(&["watch"]);
        assert_eq!(
            status, EXIT_NOT_IMPLEMENTED,
            "unimplemented commands must fail"
        );
        assert!(out.is_empty(), "nothing may be printed to stdout");
        assert!(
            err.contains("not implemented yet"),
            "the user must be told why"
        );
    }
}
