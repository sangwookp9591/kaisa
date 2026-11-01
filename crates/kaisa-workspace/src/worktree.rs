//! Creating attempt worktrees through the git CLI (architecture §5.5).

use std::fmt;
use std::path::{Path, PathBuf};

use crate::git::{EmptyHooksDirectory, GitError, run_git, run_git_for_preservation};
use crate::inventory::{IgnoredPolicy, Inventory, plan_inventory};
use crate::receipt::{PreservationReceipt, UncoveredDeletion, check_deletion};
use crate::status::{StatusParseError, parse_status};

/// A git worktree owned by one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    path: PathBuf,
    branch: String,
    gitdir: PathBuf,
}

impl Worktree {
    pub(crate) fn preservation_policy(&self) -> Result<EmptyHooksDirectory, GitError> {
        let actual = recorded_pointer(&self.path)?;
        if actual != self.gitdir {
            return Err(GitError::Failed {
                args: vec!["verify-worktree".into()],
                stderr: "refusing preservation: worktree .git pointer changed".into(),
            });
        }
        let mut policy = EmptyHooksDirectory::create()?;
        policy.bind(&self.gitdir);
        Ok(policy)
    }

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
    /// Removal was refused because the receipt does not cover every change.
    Uncovered(UncoveredDeletion),
}

impl fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BranchExists(branch) => write!(f, "branch `{branch}` already exists"),
            Self::PathExists(path) => write!(f, "`{}` already exists", path.display()),
            Self::Git(error) => write!(f, "{error}"),
            Self::Status(error) => write!(f, "{error}"),
            Self::Uncovered(refusal) => write!(f, "{refusal}"),
        }
    }
}

impl std::error::Error for WorktreeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Git(error) => Some(error),
            Self::Status(error) => Some(error),
            Self::Uncovered(refusal) => Some(refusal),
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
        gitdir: recorded_pointer(path)?,
    })
}

/// The worktree of `repository` at `path`, as git records it, or `None` if git knows no
/// worktree there. Recovery uses this to pick up an attempt's worktree after a crash; a
/// directory git does not list is never treated as an attempt's worktree.
///
/// # Errors
///
/// Returns [`WorktreeError::Git`] if git fails.
pub fn open_worktree(repository: &Path, path: &Path) -> Result<Option<Worktree>, WorktreeError> {
    let Ok(wanted) = path.canonicalize() else {
        return Ok(None);
    };
    let listing: Vec<u8> = run_git(repository, &["worktree", "list", "--porcelain"])?;
    let listing: String = String::from_utf8_lossy(&listing).into_owned();
    for block in listing.split("\n\n") {
        let mut listed: Option<PathBuf> = None;
        let mut branch: Option<String> = None;
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("worktree ") {
                listed = Some(PathBuf::from(rest));
            } else if let Some(rest) = line.strip_prefix("branch refs/heads/") {
                branch = Some(rest.to_owned());
            }
        }
        if let (Some(listed), Some(branch)) = (listed, branch)
            && listed.canonicalize().ok().as_ref() == Some(&wanted)
        {
            return Ok(Some(Worktree {
                path: path.to_owned(),
                branch,
                gitdir: registered_gitdir(repository, &wanted)?,
            }));
        }
    }
    Ok(None)
}

/// Whether a branch is registered in any worktree, including an unavailable checkout.
/// Recovery uses a listing serialized with Kaisa's worktree mutations so it cannot
/// mistake an entry still being created for a completed manual removal.
///
/// # Errors
///
/// Returns [`GitError`] if listing or locking the shared metadata fails.
pub fn branch_has_worktree(repository: &Path, branch: &str) -> Result<bool, GitError> {
    let listing = run_git(repository, &["worktree", "list", "--porcelain"])?;
    let expected = format!("branch refs/heads/{branch}");
    Ok(String::from_utf8_lossy(&listing)
        .lines()
        .any(|line| line == expected))
}

/// Lists registered worktree paths while holding the shared metadata lock.
/// Private QA uses this to exclude every developer checkout from its read grants.
///
/// # Errors
///
/// Returns [`GitError`] if listing or locking the shared metadata fails.
pub fn worktree_paths(repository: &Path) -> Result<Vec<PathBuf>, GitError> {
    let listing = run_git(repository, &["worktree", "list", "--porcelain", "-z"])?;
    Ok(String::from_utf8_lossy(&listing)
        .split('\0')
        .filter_map(|field| field.strip_prefix("worktree ").map(PathBuf::from))
        .collect())
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
    let hooks = worktree.preservation_policy()?;
    let output: Vec<u8> = run_git_for_preservation(
        worktree.path(),
        &hooks,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
            "--ignore-submodules=all",
        ],
    )?;
    let mut entries = parse_status(&output).map_err(WorktreeError::Status)?;
    // Ignoring submodules must not make their unscannable contents disposable.
    // Read only the parent index, never the nested repository's config or status.
    let index = run_git_for_preservation(worktree.path(), &hooks, &["ls-files", "--stage", "-z"])?;
    for record in index
        .split(|byte| *byte == 0)
        .filter(|record| record.starts_with(b"160000 "))
    {
        if let Some(separator) = record.iter().position(|byte| *byte == b'\t') {
            let path = String::from_utf8_lossy(&record[separator + 1..]).into_owned();
            if !entries.iter().any(|entry| entry.path == path) {
                entries.push(crate::status::StatusEntry {
                    path,
                    kind: crate::status::EntryKind::Tracked,
                });
            }
        }
    }
    Ok(plan_inventory(&entries, policy))
}

/// Removes `worktree` from `repository`, but only after checking that `receipt` covers every
/// change git reports in it. The worktree's branch is kept, because preserved work may live
/// on it as a WIP commit.
///
/// # Errors
///
/// Returns [`WorktreeError::Uncovered`] (and removes nothing) if the receipt misses any change,
/// or another [`WorktreeError`] if git fails.
pub fn remove_worktree(
    repository: &Path,
    worktree: &Worktree,
    receipt: &PreservationReceipt,
    policy: &IgnoredPolicy,
) -> Result<(), WorktreeError> {
    check_deletion(&inventory(worktree, policy)?, receipt).map_err(WorktreeError::Uncovered)?;
    let path_text: String = worktree.path().to_string_lossy().into_owned();
    // `--force` is required because untracked and ignored files are still present; the
    // coverage check above is what makes discarding them safe.
    let mut hooks = EmptyHooksDirectory::create()?;
    let main_gitdir = crate::git::git_text(repository, &["rev-parse", "--absolute-git-dir"])?;
    hooks.bind(Path::new(&main_gitdir));
    run_git_for_preservation(
        repository,
        &hooks,
        &["worktree", "remove", "--force", &path_text],
    )?;
    Ok(())
}

fn recorded_pointer(path: &Path) -> Result<PathBuf, GitError> {
    let pointer = std::fs::read_to_string(path.join(".git")).map_err(GitError::Spawn)?;
    let target = pointer
        .trim()
        .strip_prefix("gitdir: ")
        .ok_or_else(|| GitError::Failed {
            args: vec!["verify-worktree".into()],
            stderr: "invalid worktree .git pointer".into(),
        })?;
    path.join(target).canonicalize().map_err(GitError::Spawn)
}

fn registered_gitdir(repository: &Path, wanted: &Path) -> Result<PathBuf, GitError> {
    let common = crate::git::git_text(
        repository,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    for entry in std::fs::read_dir(Path::new(&common).join("worktrees")).map_err(GitError::Spawn)? {
        let entry = entry.map_err(GitError::Spawn)?;
        if let Ok(backlink) = std::fs::read_to_string(entry.path().join("gitdir"))
            && Path::new(backlink.trim())
                .parent()
                .and_then(|p| p.canonicalize().ok())
                .as_deref()
                == Some(wanted)
        {
            return entry.path().canonicalize().map_err(GitError::Spawn);
        }
    }
    Err(GitError::Failed {
        args: vec!["verify-worktree".into()],
        stderr: "worktree has no registered gitdir".into(),
    })
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

    use super::{Worktree, WorktreeError, create_worktree, inventory, remove_worktree};
    use crate::git::git_text;
    use crate::inventory::{IgnoredPolicy, Inventory};
    use crate::receipt::{PreservationReceipt, UncoveredDeletion};
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

    /// The new test file an attempt typically leaves behind.
    const NEW_TEST_FILE: &str = "tests/login_test.rs";

    fn an_attempt_that_added_a_test(repository: &TemporaryRepository) -> Worktree {
        let worktree: Worktree = a_worktree_for_attempt_41a(repository);
        the_agent_writes(&worktree, NEW_TEST_FILE, "#[test] fn logs_in() {}");
        worktree
    }

    fn a_receipt_that_forgot_everything() -> PreservationReceipt {
        PreservationReceipt::default()
    }

    fn a_receipt_covering_the_new_test() -> PreservationReceipt {
        PreservationReceipt::covering(&[NEW_TEST_FILE])
    }

    #[test]
    fn removing_a_worktree_with_uncovered_changes_is_refused() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let worktree: Worktree = an_attempt_that_added_a_test(&repository);

        let removal = remove_worktree(
            repository.path(),
            &worktree,
            &a_receipt_that_forgot_everything(),
            &demo_build_policy(),
        );

        assert!(
            matches!(
                &removal,
                Err(WorktreeError::Uncovered(UncoveredDeletion { uncovered }))
                    if uncovered.iter().any(|path| path == NEW_TEST_FILE)
            ),
            "unpreserved work must block removal and be named, got {removal:?}"
        );
    }

    #[test]
    fn a_refused_removal_leaves_the_worktree_and_its_files_in_place() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let worktree: Worktree = an_attempt_that_added_a_test(&repository);

        let _refused = remove_worktree(
            repository.path(),
            &worktree,
            &a_receipt_that_forgot_everything(),
            &demo_build_policy(),
        );

        assert!(
            worktree.path().join(NEW_TEST_FILE).exists(),
            "a refusal must not delete anything, not even partially"
        );
    }

    #[test]
    fn removing_a_worktree_after_preservation_deletes_its_directory() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let worktree: Worktree = an_attempt_that_added_a_test(&repository);

        remove_worktree(
            repository.path(),
            &worktree,
            &a_receipt_covering_the_new_test(),
            &demo_build_policy(),
        )
        .expect("covered work may be removed");

        assert!(
            !worktree.path().exists(),
            "the worktree directory is gone after cleanup"
        );
    }

    #[test]
    fn removing_a_worktree_keeps_its_branch_for_the_preserved_work() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let worktree: Worktree = an_attempt_that_added_a_test(&repository);

        remove_worktree(
            repository.path(),
            &worktree,
            &a_receipt_covering_the_new_test(),
            &demo_build_policy(),
        )
        .expect("covered work may be removed");

        assert_eq!(
            git_text(repository.path(), &["branch", "--list", worktree.branch()])
                .expect("git runs")
                .trim_start_matches(['*', '+', ' ']),
            worktree.branch(),
            "the branch may hold the WIP commit, so cleanup must not delete it"
        );
    }

    #[test]
    fn an_attempts_worktree_can_be_opened_again_after_a_crash() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let created: Worktree = a_worktree_for_attempt_41a(&repository);

        let reopened: Option<Worktree> =
            super::open_worktree(repository.path(), created.path()).expect("git runs");

        assert_eq!(
            reopened,
            Some(created),
            "recovery finds the same worktree on the same branch"
        );
    }

    #[test]
    fn a_directory_git_does_not_list_is_not_an_attempts_worktree() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let stray: PathBuf = repository.sibling("stray-directory");
        std::fs::create_dir_all(&stray).expect("writable");

        let opened: Option<Worktree> =
            super::open_worktree(repository.path(), &stray).expect("git runs");
        let _ = std::fs::remove_dir_all(&stray);

        assert_eq!(
            opened, None,
            "recovery never preserves or deletes a directory git does not own"
        );
    }
}
