//! Confirming a detected command by running it (architecture §5.9: "commands are confirmed by a
//! successful check run").
//!
//! Detection only reads files. A command becomes trusted for verification only after it ran in
//! this repository and exited successfully within a time limit.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kaisa_sys::{OwnedProcess, ProcessExit, ProcessSpec};

use crate::profile::{Confirmation, DetectedCommand};

/// The exit code of a command that succeeded.
const SUCCESS: i32 = 0;

/// Runs `command` in the repository at `root` and returns it with its confirmation set.
///
/// A program that is not installed, a non-zero exit and running past `limit` all leave the
/// command `Failed`; a command that runs past the limit is ended with its whole process tree.
///
/// # Errors
///
/// Returns the operating system's error only if a started process cannot be waited for or ended.
pub fn confirm(
    command: &DetectedCommand,
    root: &Path,
    limit: Duration,
) -> io::Result<DetectedCommand> {
    let Some((program, args)) = command.argv.split_first() else {
        return Ok(with_confirmation(command, Confirmation::Failed));
    };
    let working_directory: PathBuf = command.directory.as_ref().map_or_else(
        || root.to_owned(),
        |directory| root.join(directory.as_str()),
    );
    let Ok(mut process) = OwnedProcess::spawn(&ProcessSpec {
        program: PathBuf::from(program),
        args: args.to_vec(),
        working_directory,
    }) else {
        return Ok(with_confirmation(command, Confirmation::Failed));
    };
    let outcome: Option<ProcessExit> = process.wait_timeout(limit)?;
    // Ends anything the command left running, and the command itself if it ran too long.
    process.kill_tree()?;
    let confirmation: Confirmation = match outcome {
        Some(ProcessExit::Code(SUCCESS)) => Confirmation::Confirmed,
        _ => Confirmation::Failed,
    };
    Ok(with_confirmation(command, confirmation))
}

fn with_confirmation(command: &DetectedCommand, confirmation: Confirmation) -> DetectedCommand {
    DetectedCommand {
        confirmation,
        ..command.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::confirm;
    use crate::profile::{Confirmation, DetectedCommand, Language, Purpose};
    use crate::test_support::ScratchProject;

    /// Generous for `cargo --version`, even on a busy CI runner.
    const CONFIRMATION_LIMIT: Duration = Duration::from_secs(60);

    /// A check whose program exists on every CI runner, as the Rust toolchain is installed.
    fn a_check(argv: &[&str]) -> DetectedCommand {
        DetectedCommand {
            language: Language::Rust,
            purpose: Purpose::Test,
            argv: argv.iter().map(|&part| part.to_owned()).collect(),
            directory: None,
            confirmation: Confirmation::Detected,
        }
    }

    #[test]
    fn a_command_that_succeeds_is_confirmed() {
        let project: ScratchProject = ScratchProject::with_files(&[]);

        let checked: DetectedCommand = confirm(
            &a_check(&["cargo", "--version"]),
            project.root(),
            CONFIRMATION_LIMIT,
        )
        .expect("running the check works");

        assert_eq!(
            checked.confirmation,
            Confirmation::Confirmed,
            "a check that ran and passed here can be relied on"
        );
    }

    #[test]
    fn a_command_that_fails_stays_unconfirmed() {
        let project: ScratchProject = ScratchProject::with_files(&[]);

        let checked: DetectedCommand = confirm(
            &a_check(&["cargo", "kaisa-no-such-subcommand"]),
            project.root(),
            CONFIRMATION_LIMIT,
        )
        .expect("running the check works");

        assert_eq!(
            checked.confirmation,
            Confirmation::Failed,
            "verification must never rest on a command that does not work in this repository"
        );
    }

    #[test]
    fn a_command_that_is_not_installed_stays_unconfirmed() {
        let project: ScratchProject = ScratchProject::with_files(&[]);

        let checked: DetectedCommand = confirm(
            &a_check(&["kaisa-test-tool-that-is-not-installed"]),
            project.root(),
            CONFIRMATION_LIMIT,
        )
        .expect("a missing tool is a result, not an error");

        assert_eq!(
            checked.confirmation,
            Confirmation::Failed,
            "a project that names a tool the machine lacks gets told so, not a crash"
        );
    }
}
