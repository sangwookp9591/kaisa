//! Creating attempt worktrees through the git CLI (architecture §5.5).

use std::fmt;
use std::path::{Path, PathBuf};

use crate::git::{GitError, run_git};
use crate::inventory::{IgnoredPolicy, Inventory, plan_inventory};
use crate::status::{StatusParseError, parse_status};

/// A git worktree owned by one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    path: PathBuf,
    branch: String,
}

impl Worktree {
    /// The worktree's directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The branch checked out in the worktree.
    #[must_use]
    pub fn branch(&self) -> &str {
        &self.branch
    }
}

/// Why a worktree could not be created.
#[derive(Debug)]
#[non_exhaustive]
pub enum WorktreeError {
    /// The branch name is already used; reusing it could mix two attempts' work.
    BranchExists(String),
    /// Something already exists at the worktree path.
    PathExists(PathBuf),
    /// Git failed.
    Git(GitError),
    /// Git's status output could not be understood.
    Status(StatusParseError),
}

impl fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BranchExists(branch) => write!(f, "branch `{branch}` already exists"),
            Self::PathExists(path) => write!(f, "`{}` already exists", path.display()),
            Self::Git(error) => write!(f, "{error}"),
            Self::Status(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for WorktreeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Git(error) => Some(error),
            Self::Status(error) => Some(error),
            Self::BranchExists(_) | Self::PathExists(_) => None,
        }
    }
}

impl From<GitError> for WorktreeError {
    fn from(error: GitError) -> Self {
        Self::Git(error)
    }
}

/// Creates a worktree at `path` on a new `branch` that starts at `base` (a commit, branch or
/// tag in `repository`).
///
/// # Errors
///
/// Returns [`WorktreeError::BranchExists`] if `branch` already exists,
/// [`WorktreeError::PathExists`] if `path` is taken, or [`WorktreeError::Git`] if git fails.
pub fn create_worktree(
    repository: &Path,
    base: &str,
    branch: &str,
    path: &Path,
) -> Result<Worktree, WorktreeError> {
    if branch_exists(repository, branch) {
        return Err(WorktreeError::BranchExists(branch.to_owned()));
    }
    if path.exists() {
        return Err(WorktreeError::PathExists(path.to_owned()));
    }
    let path_text: String = path.to_string_lossy().into_owned();
    run_git(
        repository,
        &["worktree", "add", "--quiet", "-b", branch, &path_text, base],
    )?;
    Ok(Worktree {
        path: path.to_owned(),
        branch: branch.to_owned(),
    })
}

/// What would have to be preserved before `worktree` is removed, according to git and
/// `policy`. Untracked files are listed individually and ignored files are included, so
/// nothing in the worktree is invisible to the preservation protocol.
///
/// # Errors
///
/// Returns [`WorktreeError::Git`] if git fails or [`WorktreeError::Status`] if its output
/// cannot be parsed.
pub fn inventory(worktree: &Worktree, policy: &IgnoredPolicy) -> Result<Inventory, WorktreeError> {
    let output: Vec<u8> = run_git(
        worktree.path(),
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )?;
    let entries = parse_status(&output).map_err(WorktreeError::Status)?;
    Ok(plan_inventory(&entries, policy))
}

/// True when `refs/heads/<branch>` exists in `repository`.
fn branch_exists(repository: &Path, branch: &str) -> bool {
    let reference: String = format!("refs/heads/{branch}");
    run_git(
        repository,
        &["rev-parse", "--verify", "--quiet", &reference],
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Worktree, WorktreeError, create_worktree, inventory};
    use crate::git::git_text;
    use crate::inventory::{IgnoredPolicy, Inventory};
    use crate::status::{EntryKind, StatusEntry};
    use crate::test_support::{TemporaryRepository, a_repository_with_one_commit_on_main};

    /// The branch Kaisa would create for run 41's first attempt.
    const ATTEMPT_BRANCH: &str = "kaisa/41-a";

    fn commit_of(directory: &std::path::Path, revision: &str) -> String {
        git_text(directory, &["rev-parse", revision]).expect("the revision exists")
    }

    fn the_attempt_worktree_path(repository: &TemporaryRepository) -> PathBuf {
        repository.sibling("att_41a")
    }

    fn a_worktree_for_attempt_41a(repository: &TemporaryRepository) -> Worktree {
        create_worktree(
            repository.path(),
            "main",
            ATTEMPT_BRANCH,
            &the_attempt_worktree_path(repository),
        )
        .expect("a fresh branch and path can be used")
    }

    #[test]
    fn a_worktree_created_from_main_starts_at_mains_commit() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();

        let worktree: Worktree = a_worktree_for_attempt_41a(&repository);

        assert_eq!(
            commit_of(worktree.path(), "HEAD"),
            commit_of(repository.path(), "main"),
            "the attempt starts from exactly the commit it was asked to start from"
        );
    }

    #[test]
    fn a_created_worktree_is_checked_out_on_its_new_branch() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();

        let worktree: Worktree = a_worktree_for_attempt_41a(&repository);

        assert_eq!(
            git_text(worktree.path(), &["branch", "--show-current"]).expect("git runs"),
            ATTEMPT_BRANCH,
            "the attempt's commits land on its own branch, never on main"
        );
    }

    #[test]
    fn creating_a_worktree_on_an_existing_branch_name_is_refused() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        a_worktree_for_attempt_41a(&repository);

        let second: Result<Worktree, WorktreeError> = create_worktree(
            repository.path(),
            "main",
            ATTEMPT_BRANCH,
            &repository.sibling("att_41a-again"),
        );

        assert!(
            matches!(&second, Err(WorktreeError::BranchExists(branch)) if branch == ATTEMPT_BRANCH),
            "two attempts must never share a branch, got {second:?}"
        );
    }

    #[test]
    fn creating_a_worktree_where_a_directory_already_exists_is_refused() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let occupied: PathBuf = the_attempt_worktree_path(&repository);
        std::fs::create_dir_all(&occupied).expect("the temp directory is writable");

        let created: Result<Worktree, WorktreeError> =
            create_worktree(repository.path(), "main", ATTEMPT_BRANCH, &occupied);

        assert!(
            matches!(&created, Err(WorktreeError::PathExists(path)) if *path == occupied),
            "Kaisa never writes into a directory it did not create, got {created:?}"
        );
    }

    /// The build policy of the demo project: `target` is rebuildable.
    fn demo_build_policy() -> IgnoredPolicy {
        IgnoredPolicy::with_build_output(&["target"])
    }

    /// The agent writes `contents` to `relative_path` inside the worktree.
    fn the_agent_writes(worktree: &Worktree, relative_path: &str, contents: &str) {
        let file: PathBuf = worktree.path().join(relative_path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("the worktree is writable");
        }
        std::fs::write(file, contents).expect("the worktree is writable");
    }

    fn inventory_of(worktree: &Worktree) -> Inventory {
        inventory(worktree, &demo_build_policy()).expect("git status runs and parses")
    }

    fn entry(path: &str, kind: EntryKind) -> StatusEntry {
        StatusEntry {
            path: path.to_owned(),
            kind,
        }
    }

    #[test]
    fn a_fresh_worktree_has_an_empty_inventory() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let worktree: Worktree = a_worktree_for_attempt_41a(&repository);

        assert_eq!(
            inventory_of(&worktree),
            Inventory::default(),
            "an attempt that has not done anything has nothing to preserve"
        );
    }

    #[test]
    fn a_new_file_in_a_worktree_appears_in_its_inventory() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let worktree: Worktree = a_worktree_for_attempt_41a(&repository);

        the_agent_writes(&worktree, "tests/login_test.rs", "#[test] fn logs_in() {}");

        assert_eq!(
            inventory_of(&worktree).preserve,
            [entry("tests/login_test.rs", EntryKind::Untracked)],
            "a file the agent created must be preserved even though git does not track it"
        );
    }

    #[test]
    fn an_edited_tracked_file_appears_in_its_inventory() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let worktree: Worktree = a_worktree_for_attempt_41a(&repository);

        the_agent_writes(&worktree, "README.md", "# demo\n\nNow with login.\n");

        assert_eq!(
            inventory_of(&worktree).preserve,
            [entry("README.md", EntryKind::Tracked)],
            "edits to committed files are part of the attempt's work"
        );
    }

    #[test]
    fn ignored_build_output_in_a_worktree_is_disposable() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        repository.commit_file(".gitignore", "target/\n", "ignore build output");
        let worktree: Worktree = a_worktree_for_attempt_41a(&repository);

        the_agent_writes(&worktree, "target/debug/demo", "compiled bytes");

        let planned: Inventory = inventory_of(&worktree);
        assert!(
            planned.preserve.is_empty(),
            "rebuildable output is not work to preserve"
        );
        assert_eq!(
            planned.disposable,
            [entry("target/", EntryKind::Ignored)],
            "git reports the ignored directory, and the policy marks it disposable"
        );
    }
}
