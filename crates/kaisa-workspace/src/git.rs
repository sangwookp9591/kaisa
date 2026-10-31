//! Running the `git` CLI (architecture §5.5: mutations always go through `git` itself).

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

/// Why a git command did not succeed.
#[derive(Debug)]
#[non_exhaustive]
pub enum GitError {
    /// `git` could not be started (not installed, or not on `PATH`).
    Spawn(std::io::Error),
    /// Preservation was refused because no private empty hooks directory could be created.
    HooksUnavailable(std::io::Error),
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
            Self::HooksUnavailable(error) => write!(
                f,
                "refusing preservation: could not create an empty hooks directory: {error}"
            ),
            Self::Failed { args, stderr } => write!(f, "`git {}` failed: {stderr}", args.join(" ")),
        }
    }
}

impl std::error::Error for GitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) | Self::HooksUnavailable(error) => Some(error),
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

static NEXT_HOOKS_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// Owned by one preservation operation, outside the agent's worktree. Never reuse an
/// existing directory: it may contain hooks. A private parent also holds empty configuration
/// and transient NUL-delimited pathspec files; Drop removes only owned files and empty directories.
pub(crate) struct EmptyHooksDirectory {
    path: PathBuf,
    setting: String,
    empty_file: PathBuf,
}

impl EmptyHooksDirectory {
    pub(crate) fn create() -> Result<Self, GitError> {
        Self::create_in(&std::env::temp_dir()).map_err(GitError::HooksUnavailable)
    }

    fn create_in(parent: &Path) -> Result<Self, std::io::Error> {
        let parent = std::path::absolute(parent)?;
        loop {
            let path = parent.join(format!(
                "kaisa-preservation-hooks-{}-{}",
                std::process::id(),
                NEXT_HOOKS_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            let text = path.to_str().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "hooks path is not UTF-8")
            })?;
            let builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            let builder = {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = builder;
                builder.mode(0o700);
                builder
            };
            match builder.create(&path) {
                Ok(()) => {
                    let empty_file = path.join("empty");
                    if let Err(error) = std::fs::write(&empty_file, []) {
                        let _ = std::fs::remove_dir(&path);
                        return Err(error);
                    }
                    let hooks_path = path.join("hooks");
                    if let Err(error) = std::fs::create_dir(&hooks_path) {
                        let _ = std::fs::remove_file(&empty_file);
                        let _ = std::fs::remove_dir(&path);
                        return Err(error);
                    }
                    return Ok(Self {
                        path: hooks_path,
                        setting: format!("core.hooksPath={text}/hooks"),
                        empty_file,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for EmptyHooksDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.path);
        let _ = std::fs::remove_file(self.empty_file.with_file_name("paths"));
        let _ = std::fs::remove_file(&self.empty_file);
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

/// Preservation never executes repository hooks, including hooks not covered by
/// `commit --no-verify`. The caller keeps the empty directory alive for every Git call.
pub(crate) fn run_git_for_preservation(
    directory: &Path,
    hooks: &EmptyHooksDirectory,
    args: &[&str],
) -> Result<Vec<u8>, GitError> {
    let mut command = preservation_command(directory, hooks);
    // `git config` only reads configuration; it does not run filters. Override every
    // configured driver, including filters named only by worktree attributes.
    let configured = preservation_command(directory, hooks)
        .args([
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            "^filter\\..*\\.(clean|smudge|process|required)$",
        ])
        .output()
        .map_err(GitError::Spawn)?;
    if !configured.status.success() && configured.status.code() != Some(1) {
        return Err(GitError::Failed {
            args: vec!["config".into()],
            stderr: String::from_utf8_lossy(&configured.stderr).into_owned(),
        });
    }
    for key in configured
        .stdout
        .split(|byte| *byte == 0)
        .filter(|key| !key.is_empty())
    {
        let key = std::str::from_utf8(key).map_err(|error| {
            GitError::Spawn(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })?;
        let value = if key.ends_with(".required") {
            "false"
        } else {
            ""
        };
        command.args(["-c", &format!("{key}={value}")]);
    }
    let output = command.args(args).output().map_err(GitError::Spawn)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(GitError::Failed {
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

/// Every preservation command, including configuration discovery, uses this policy.
fn preservation_command(directory: &Path, hooks: &EmptyHooksDirectory) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &hooks.empty_file)
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env_remove("GIT_CONFIG")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT")
        .args([
            "-c",
            &hooks.setting,
            "-c",
            "core.fsmonitor=false",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "core.sshCommand=",
            "-c",
            "diff.external=",
            "--literal-pathspecs",
        ])
        .args([
            "-c",
            &format!("core.attributesFile={}", hooks.empty_file.display()),
        ]);
    command
}

/// Stage literal paths without growing the process argument vector. The private file
/// belongs to the same operation as the hooks policy and is removed before returning.
pub(crate) fn stage_paths(
    directory: &Path,
    hooks: &EmptyHooksDirectory,
    paths: &[String],
) -> Result<(), GitError> {
    let pathspec = hooks.empty_file.with_file_name("paths");
    let mut bytes = Vec::new();
    for path in paths {
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
    }
    std::fs::write(&pathspec, bytes).map_err(GitError::Spawn)?;
    let result = run_git_for_preservation(
        directory,
        hooks,
        &[
            "add",
            "--all",
            "--force",
            &format!("--pathspec-from-file={}", pathspec.display()),
            "--pathspec-file-nul",
        ],
    );
    let _ = std::fs::remove_file(pathspec);
    result.map(|_| ())
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
    use super::{EmptyHooksDirectory, GitError, git_text, run_git_for_preservation};
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
    #[test]
    fn a_preservation_hooks_directory_is_empty_and_removed_after_its_owner_drops() {
        let repository = a_repository_with_one_commit_on_main();
        let hooks = EmptyHooksDirectory::create().expect("temporary storage is available");
        let path = hooks.path.clone();
        let configured = run_git_for_preservation(
            repository.path(),
            &hooks,
            &["config", "--get", "core.hooksPath"],
        )
        .expect("the preservation Git configuration is readable");

        assert_eq!(
            String::from_utf8(configured)
                .expect("the path is UTF-8")
                .trim(),
            path.to_str().expect("the owned path is UTF-8"),
            "Git must use the actual owned empty directory on every operating system"
        );
        assert!(
            std::fs::read_dir(&path)
                .expect("the directory exists")
                .next()
                .is_none(),
            "the directory must contain no hooks that Git could execute"
        );
        drop(hooks);
        assert!(
            !path.exists(),
            "the operation owns and cleans up its temporary directory"
        );
    }

    #[test]
    fn an_uncreatable_hooks_directory_refuses_preservation_with_the_reason() {
        let repository = a_repository_with_one_commit_on_main();
        let file_instead_of_a_directory = repository.path().join("README.md");

        let refusal = EmptyHooksDirectory::create_in(&file_instead_of_a_directory)
            .err()
            .map(GitError::HooksUnavailable)
            .expect("a file cannot hold a hooks directory");

        assert!(
            refusal.to_string().contains("refusing preservation"),
            "creation failure must explicitly refuse preservation rather than enable hooks"
        );
        assert!(
            std::error::Error::source(&refusal).is_some(),
            "the refusal must retain the OS error explaining why directory creation failed"
        );
    }
    #[test]
    fn preservation_overrides_executable_settings_and_uses_empty_global_configuration() {
        let repository = a_repository_with_one_commit_on_main();
        let hooks = EmptyHooksDirectory::create().expect("create private Git policy");
        for (key, value) in [
            ("core.fsmonitor", "false"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
            ("core.sshCommand", ""),
            ("diff.external", ""),
        ] {
            let output =
                run_git_for_preservation(repository.path(), &hooks, &["config", "--get", key])
                    .expect("read effective setting");
            assert_eq!(
                String::from_utf8(output).expect("setting is text").trim(),
                value,
                "preservation must enforce {key}"
            );
        }
        let command = super::preservation_command(repository.path(), &hooks);
        let environment: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            environment.get(std::ffi::OsStr::new("GIT_CONFIG_NOSYSTEM")),
            Some(&Some(std::ffi::OsStr::new("1"))),
            "system configuration must be disabled"
        );
        assert_eq!(
            environment.get(std::ffi::OsStr::new("GIT_CONFIG_GLOBAL")),
            Some(&Some(hooks.empty_file.as_os_str())),
            "global configuration must use the operation's empty file"
        );
        assert!(
            std::fs::read(&hooks.empty_file)
                .expect("read global configuration")
                .is_empty(),
            "global settings must not inherit executable programs"
        );
        let attributes = run_git_for_preservation(
            repository.path(),
            &hooks,
            &["config", "--get", "core.attributesFile"],
        )
        .expect("read attributes policy");
        assert_eq!(
            String::from_utf8(attributes).expect("path is UTF-8").trim(),
            hooks.empty_file.to_str().expect("owned path is UTF-8"),
            "user attribute configuration must use the empty file"
        );
    }
}
