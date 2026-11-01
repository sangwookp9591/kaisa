//! Running the `git` CLI (architecture §5.5: mutations always go through `git` itself).
//!
//! Confined agents cannot write root `.git`, `.gitattributes` or `.gitmodules` controls
//! through broker writes or confined execution (F2c). Nested controls with those names
//! remain intentionally writable; nested repository configuration is untrusted and must
//! never be consulted by preservation. Disable submodule inspection and recursion.
//! Repository metadata outside the tree and global/system configuration are user-trusted;
//! configured filters (including LFS) may execute. Observed agents already have user
//! authority. Pin Git to recorded metadata as defense in depth against pointer changes;
//! disable hooks, fsmonitor and signing because preservation does not need them.

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
    /// Shared worktree metadata could not be locked; no mutation was attempted.
    MetadataLock(std::io::Error),
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
            Self::MetadataLock(error) => write!(f, "could not lock Git worktree metadata: {error}"),
            Self::Failed { args, stderr } => write!(f, "`git {}` failed: {stderr}", args.join(" ")),
        }
    }
}

impl std::error::Error for GitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) | Self::HooksUnavailable(error) | Self::MetadataLock(error) => {
                Some(error)
            }
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
    run_command(args, || {
        let mut command = Command::new("git");
        command
            .current_dir(directory)
            .env("GIT_TERMINAL_PROMPT", "0");
        command
    })
}

/// Both regular and preservation runners use this boundary. All worktree subcommands
/// (including add/remove/prune/move/repair and list) participate, so new callers cannot
/// accidentally bypass serialization. Discovery uses the same pinned Git configuration
/// as the mutation, and never recurses through the locking runner.
fn run_command(args: &[&str], command: impl Fn() -> Command) -> Result<Vec<u8>, GitError> {
    let run = || command_output(command().args(args).output(), args);
    if args.first() == Some(&"worktree") {
        let discovery = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
        let common = command_output(command().args(discovery).output(), &discovery)?;
        let common = String::from_utf8_lossy(&common);
        crate::worktree_lock::with_lock(Path::new(common.trim()), run)
    } else {
        run()
    }
}

fn command_output(output: std::io::Result<Output>, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let output = output.map_err(GitError::Spawn)?;
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
    git_dir: Option<PathBuf>,
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
            // The hooks setting is passed to Git as text, so a non-UTF-8 location is refused.
            if path.to_str().is_none() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "hooks path is not UTF-8",
                ));
            }
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
                    // The setting names the very directory created above, in the platform's
                    // spelling (`\` on Windows), so Git and the owner agree on one path.
                    let setting = format!("core.hooksPath={}", git_cli_path(&hooks_path));
                    return Ok(Self {
                        path: hooks_path,
                        setting,
                        empty_file,
                        git_dir: None,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
    /// Pin commands to the metadata recorded outside the agent-writable tree.
    pub(crate) fn bind(&mut self, gitdir: &Path) {
        self.git_dir = Some(gitdir.to_owned());
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
    run_command(args, || preservation_command(directory, hooks))
}

/// Every preservation command, including configuration discovery, uses this policy.
fn preservation_command(directory: &Path, hooks: &EmptyHooksDirectory) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_CONFIG")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT")
        .args([
            "-c",
            &hooks.setting,
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "diff.ignoreSubmodules=all",
            "-c",
            "submodule.recurse=false",
            "--literal-pathspecs",
        ]);
    if let Some(gitdir) = &hooks.git_dir {
        command.arg(format!("--git-dir={}", git_cli_path(gitdir)));
        command.arg(format!("--work-tree={}", git_cli_path(directory)));
    }
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(key);
    }
    command
}

/// Canonical paths retain extended Windows prefixes for identity comparisons;
/// only Git CLI arguments use ordinary drive/UNC spelling.
#[must_use]
pub fn git_cli_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    {
        ordinary_windows_git_path(&text)
    }
    #[cfg(not(windows))]
    {
        text.into_owned()
    }
}

#[cfg(any(windows, test))]
fn ordinary_windows_git_path(path: &str) -> String {
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
    if let Some(drive) = path.strip_prefix(r"\\?\")
        && drive.as_bytes().get(1..3) == Some(b":\\")
        && drive
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
    {
        return drive.to_owned();
    }
    path.to_owned()
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

/// Restore flagged index entries using raw NUL-delimited names, never lossy argv.
pub(crate) fn restore_flagged(
    directory: &Path,
    hooks: &EmptyHooksDirectory,
    names: &[u8],
) -> Result<(), GitError> {
    if names.is_empty() {
        return Ok(());
    }
    let pathspec = hooks.empty_file.with_file_name("paths");
    std::fs::write(&pathspec, names).map_err(GitError::Spawn)?;
    run_git_for_preservation(
        directory,
        hooks,
        &[
            "reset",
            "--quiet",
            "HEAD",
            &format!("--pathspec-from-file={}", pathspec.display()),
            "--pathspec-file-nul",
        ],
    )?;
    std::fs::remove_file(pathspec).map_err(GitError::Spawn)
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
    fn a_bound_git_command_ignores_a_pointer_changed_after_validation() {
        let repository = a_repository_with_one_commit_on_main();
        let other = a_repository_with_one_commit_on_main();
        other.commit_file(
            "attacker.txt",
            "attacker repository",
            "different attacker commit",
        );
        let worktree = crate::worktree::create_worktree(
            repository.path(),
            "main",
            "attempt",
            &repository.sibling("attempt"),
        )
        .expect("create attempt");
        let policy = worktree
            .preservation_policy()
            .expect("validate pointer and bind metadata");
        let expected =
            git_text(repository.path(), &["rev-parse", "HEAD"]).expect("read trusted commit");
        std::fs::write(
            worktree.path().join(".git"),
            format!("gitdir: {}\n", other.path().join(".git").display()),
        )
        .expect("replace pointer after validation");
        let actual = run_git_for_preservation(worktree.path(), &policy, &["rev-parse", "HEAD"])
            .expect("Git uses recorded metadata");
        assert_eq!(
            String::from_utf8(actual).expect("commit is text").trim(),
            expected,
            "explicit git-dir must prevent a pointer race from redirecting Git"
        );
    }
    #[test]
    fn extended_windows_git_paths_use_ordinary_drive_and_unc_arguments() {
        let spellings = [
            (r"\\?\C:\repo\.git", r"C:\repo\.git"),
            (r"\\?\UNC\server\share\repo", r"\\server\share\repo"),
            (r"C:\repo\.git", r"C:\repo\.git"),
            ("/unix/repo/.git", "/unix/repo/.git"),
        ];
        for (canonical, ordinary) in spellings {
            assert_eq!(
                super::ordinary_windows_git_path(canonical),
                ordinary,
                "Git arguments must use ordinary spelling while stored canonical identity remains intact"
            );
        }
    }
}
