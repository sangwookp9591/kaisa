//! Running the `git` CLI (architecture §5.5: mutations always go through `git` itself).

use std::fmt;
use std::path::Path;
use std::process::{Command, Output};

/// Why a git command did not succeed.
#[derive(Debug)]
#[non_exhaustive]
pub enum GitError {
    /// `git` could not be started (not installed, or not on `PATH`).
    Spawn(std::io::Error),
    /// `git` ran and exited unsuccessfully.
    Failed {
        /// The arguments that were passed to git.
        args: Vec<String>,
        /// What git printed on stderr, trimmed.
        stderr: String,
    },
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "could not start git: {error}"),
            Self::Failed { args, stderr } => write!(f, "`git {}` failed: {stderr}", args.join(" ")),
        }
    }
}

impl std::error::Error for GitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            Self::Failed { .. } => None,
        }
    }
}

/// Runs `git <args>` in `directory` and returns its stdout.
///
/// Git's own configuration of the user is not trusted to be present: callers that create
/// commits pass identity with `-c` options.
///
/// # Errors
///
/// Returns [`GitError::Spawn`] if git cannot be started, or [`GitError::Failed`] if it exits
/// unsuccessfully.
pub(crate) fn run_git(directory: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let output: Output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(GitError::Spawn)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(GitError::Failed {
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

/// Runs git and returns its stdout as trimmed text.
///
/// # Errors
///
/// As [`run_git`].
pub(crate) fn git_text(directory: &Path, args: &[&str]) -> Result<String, GitError> {
    Ok(String::from_utf8_lossy(&run_git(directory, args)?)
        .trim()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::{GitError, git_text};
    use crate::test_support::{TemporaryRepository, a_repository_with_one_commit_on_main};

    #[test]
    fn a_fixture_repository_starts_on_main() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();

        assert_eq!(
            git_text(repository.path(), &["branch", "--show-current"]).expect("git runs"),
            "main",
            "tests rely on the default branch being called main"
        );
    }

    #[test]
    fn a_failing_git_command_reports_its_arguments_and_stderr() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();

        let failure: Result<String, GitError> = git_text(
            repository.path(),
            &["rev-parse", "--verify", "no-such-branch"],
        );

        assert!(
            matches!(
                &failure,
                Err(GitError::Failed { args, stderr })
                    if args.iter().any(|arg| arg == "no-such-branch") && !stderr.is_empty()
            ),
            "a failure must say which command failed and what git said, got {failure:?}"
        );
    }
}
