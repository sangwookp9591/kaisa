//! Preserving an attempt's work before its worktree is removed (architecture §5.5, step 4–5).
//!
//! Clean changes become a WIP commit on the attempt branch, so nothing an agent did is lost when
//! its worktree is reclaimed. The commit uses Kaisa's own identity and skips repository hooks:
//! preservation must not run code from the repository it is preserving.

use crate::git::run_git;
use crate::inventory::Inventory;
use crate::receipt::PreservationReceipt;
use crate::secrets::{SecretKind, scan};
use crate::worktree::{Worktree, WorktreeError};

/// Files larger than this are not scanned in memory; they are treated as flagged, so they are
/// never committed unseen.
const SCAN_LIMIT_BYTES: u64 = 4 * 1024 * 1024;

/// The result of preserving a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preservation {
    /// Proof of what was committed.
    pub receipt: PreservationReceipt,
    /// Files left out because they look like they contain credentials (or could not be
    /// scanned). They are not covered, so the worktree cannot be removed until they are
    /// preserved another way.
    pub flagged: Vec<(String, Vec<SecretKind>)>,
}

/// The identity preservation commits are made with, independent of any user configuration.
const PRESERVATION_IDENTITY: [&str; 4] =
    ["-c", "user.name=Kaisa", "-c", "user.email=kaisa@localhost"];

/// Scans every entry `inventory` says must be preserved, commits the clean ones to the
/// worktree's branch, and returns a receipt for exactly those paths together with the flagged
/// ones. A worktree with nothing clean to preserve gets an empty receipt and no commit.
///
/// # Errors
///
/// Returns [`WorktreeError::Git`] if staging or committing fails; nothing is reported as
/// preserved then.
pub fn preserve_as_commit(
    worktree: &Worktree,
    inventory: &Inventory,
) -> Result<Preservation, WorktreeError> {
    let mut paths: Vec<&str> = Vec::new();
    let mut flagged: Vec<(String, Vec<SecretKind>)> = Vec::new();
    for entry in &inventory.preserve {
        match secrets_in(worktree, &entry.path) {
            Some(kinds) if !kinds.is_empty() => flagged.push((entry.path.clone(), kinds)),
            _ => paths.push(entry.path.as_str()),
        }
    }
    if paths.is_empty() {
        return Ok(Preservation {
            receipt: PreservationReceipt::default(),
            flagged,
        });
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
    Ok(Preservation {
        receipt: PreservationReceipt::committed(&head, &paths),
        flagged,
    })
}

/// The credentials found in the file at `path`, or `None` for an entry that is not a readable
/// file (a deletion). A file too large or unreadable to scan is reported as flagged.
fn secrets_in(worktree: &Worktree, path: &str) -> Option<Vec<SecretKind>> {
    let file = worktree.path().join(path);
    let metadata = std::fs::metadata(&file).ok()?;
    if !metadata.is_file() {
        return None;
    }
    if metadata.len() > SCAN_LIMIT_BYTES {
        return Some(vec![SecretKind::Unscannable]);
    }
    match std::fs::read(&file) {
        Ok(bytes) => Some(scan(&String::from_utf8_lossy(&bytes))),
        Err(_) => Some(vec![SecretKind::Unscannable]),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Preservation, preserve_as_commit};
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
                .expect("preserving succeeds")
                .receipt;

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

        let receipt: PreservationReceipt = preserve_as_commit(&worktree, &before)
            .expect("preserving succeeds")
            .receipt;

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
                .expect("preserving succeeds")
                .receipt;

        assert_eq!(
            receipt.commit(),
            None,
            "an attempt that changed nothing leaves no empty WIP commits behind"
        );
    }

    /// A private key an agent generated and left in the worktree.
    const LEFTOVER_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n";
    /// Where the agent left it.
    const KEY_FILE: &str = "deploy/id_ed25519";

    #[test]
    fn a_file_with_a_private_key_is_never_committed() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let worktree: Worktree = a_worktree_for_attempt_41a(&repository);
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "pub fn refresh() {}\n");
        the_agent_writes(&worktree, KEY_FILE, LEFTOVER_KEY);

        let preservation: Preservation =
            preserve_as_commit(&worktree, &what_needs_preserving(&worktree))
                .expect("preserving succeeds");
        let committed: String = git_text(
            worktree.path(),
            &["show", "--name-only", "--format=", "HEAD"],
        )
        .expect("git runs");

        assert!(
            !committed.lines().any(|line| line == KEY_FILE),
            "a credential must never reach git history, got commit files {committed:?}"
        );
        assert!(
            preservation
                .flagged
                .iter()
                .any(|(path, _)| path == KEY_FILE),
            "the key is reported so a human can deal with it"
        );
    }

    #[test]
    fn a_flagged_file_blocks_reclamation() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let worktree: Worktree = a_worktree_for_attempt_41a(&repository);
        the_agent_writes(&worktree, KEY_FILE, LEFTOVER_KEY);
        let before: Inventory = what_needs_preserving(&worktree);

        let preservation: Preservation =
            preserve_as_commit(&worktree, &before).expect("preserving succeeds");

        assert!(
            check_deletion(&before, &preservation.receipt).is_err(),
            "an unpreserved credential file must not be deleted silently either"
        );
    }
}
