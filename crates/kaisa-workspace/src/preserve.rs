//! Preserving an attempt's work before its worktree is removed (architecture §5.5, step 4–5).
//!
//! Clean changes become a WIP commit on the attempt branch, so nothing an agent did is lost when
//! its worktree is reclaimed. The commit uses Kaisa's own identity and skips repository hooks:
//! preservation must not run code from the repository it is preserving.

use crate::git::run_git;
use crate::inventory::Inventory;
use crate::receipt::PreservationReceipt;
use crate::worktree::{Worktree, WorktreeError};

/// The identity preservation commits are made with, independent of any user configuration.
const PRESERVATION_IDENTITY: [&str; 4] =
    ["-c", "user.name=Kaisa", "-c", "user.email=kaisa@localhost"];

/// Commits every entry `inventory` says must be preserved to the worktree's branch and returns
/// a receipt for exactly those paths. A worktree with nothing to preserve gets an empty receipt
/// and no commit.
///
/// # Errors
///
/// Returns [`WorktreeError::Git`] if staging or committing fails; nothing is reported as
/// preserved then.
pub fn preserve_as_commit(
    worktree: &Worktree,
    inventory: &Inventory,
) -> Result<PreservationReceipt, WorktreeError> {
    let paths: Vec<&str> = inventory
        .preserve
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    if paths.is_empty() {
        return Ok(PreservationReceipt::default());
    }
    // `--force` because ignored files that the policy treats as user data must be kept too.
    let mut add: Vec<&str> = vec!["add", "--all", "--force", "--"];
    add.extend(&paths);
    run_git(worktree.path(), &add)?;
    let message: String = format!("kaisa: preserve the work of {}", worktree.branch());
    let mut commit: Vec<&str> = PRESERVATION_IDENTITY.to_vec();
    commit.extend(["commit", "--quiet", "--no-verify", "-m", &message]);
    run_git(worktree.path(), &commit)?;
    let head: Vec<u8> = run_git(worktree.path(), &["rev-parse", "HEAD"])?;
    let head: String = String::from_utf8_lossy(&head).trim().to_owned();
    Ok(PreservationReceipt::committed(&head, &paths))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::preserve_as_commit;
    use crate::git::git_text;
    use crate::inventory::{IgnoredPolicy, Inventory};
    use crate::receipt::{PreservationReceipt, check_deletion};
    use crate::test_support::{TemporaryRepository, a_repository_with_one_commit_on_main};
    use crate::worktree::{Worktree, create_worktree, inventory};

    /// The branch Kaisa would create for run 41's first attempt.
    const ATTEMPT_BRANCH: &str = "kaisa/41-a";
    /// A file the agent created.
    const NEW_SOURCE_FILE: &str = "src/session.rs";

    fn no_build_output() -> IgnoredPolicy {
        IgnoredPolicy::with_build_output(&[])
    }

    fn a_worktree_for_attempt_41a(repository: &TemporaryRepository) -> Worktree {
        let path: PathBuf = repository.sibling("att_41a");
        create_worktree(repository.path(), "main", ATTEMPT_BRANCH, &path)
            .expect("a fresh branch and path can be used")
    }

    fn the_agent_writes(worktree: &Worktree, relative: &str, contents: &str) {
        let file: PathBuf = worktree.path().join(relative);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("the worktree is writable");
        }
        std::fs::write(file, contents).expect("the worktree is writable");
    }

    fn what_needs_preserving(worktree: &Worktree) -> Inventory {
        inventory(worktree, &no_build_output()).expect("git status runs")
    }

    #[test]
    fn preserving_commits_the_changes_to_the_attempt_branch() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let worktree: Worktree = a_worktree_for_attempt_41a(&repository);
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "pub fn refresh() {}\n");

        let receipt: PreservationReceipt =
            preserve_as_commit(&worktree, &what_needs_preserving(&worktree))
                .expect("preserving succeeds");

        assert_eq!(
            receipt.commit().map(str::to_owned),
            Some(git_text(repository.path(), &["rev-parse", ATTEMPT_BRANCH]).expect("git runs")),
            "the work survives on the attempt branch after the worktree is gone"
        );
    }

    #[test]
    fn the_receipt_covers_every_committed_path() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let worktree: Worktree = a_worktree_for_attempt_41a(&repository);
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "pub fn refresh() {}\n");
        the_agent_writes(&worktree, "README.md", "# changed by the agent\n");
        let before: Inventory = what_needs_preserving(&worktree);

        let receipt: PreservationReceipt =
            preserve_as_commit(&worktree, &before).expect("preserving succeeds");

        assert_eq!(
            check_deletion(&before, &receipt),
            Ok(()),
            "every change the inventory listed is covered, so deletion becomes safe"
        );
    }

    #[test]
    fn a_worktree_with_nothing_to_preserve_needs_no_commit() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let worktree: Worktree = a_worktree_for_attempt_41a(&repository);

        let receipt: PreservationReceipt =
            preserve_as_commit(&worktree, &what_needs_preserving(&worktree))
                .expect("preserving succeeds");

        assert_eq!(
            receipt.commit(),
            None,
            "an attempt that changed nothing leaves no empty WIP commits behind"
        );
    }
}
