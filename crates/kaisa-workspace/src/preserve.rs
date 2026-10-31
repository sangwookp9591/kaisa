//! Preserving an attempt's work before its worktree is removed (architecture §5.5, step 4–5).
//!
//! Clean changes become a WIP commit on the attempt branch, so nothing an agent did is lost when
//! its worktree is reclaimed. The commit uses Kaisa's own identity and skips repository hooks:
//! preservation must not run code from the repository it is preserving.

use crate::git::{GitError, run_git_for_preservation};
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
    let mut paths: Vec<String> = Vec::new();
    let mut covered: Vec<String> = Vec::new();
    let mut flagged: Vec<(String, Vec<SecretKind>)> = Vec::new();
    for entry in &inventory.preserve {
        let mut files = Vec::new();
        expand_files(worktree, &entry.path, &mut files)?;
        let mut all_clean = true;
        for path in files {
            match secrets_in(worktree, &path) {
                Some(kinds) if !kinds.is_empty() => {
                    all_clean = false;
                    flagged.push((path, kinds));
                }
                _ => {
                    covered.push(path.clone());
                    paths.push(path);
                }
            }
        }
        if all_clean {
            covered.push(entry.path.clone());
        }
    }
    // A previous agent may already have staged credentials. Restore their HEAD versions
    // in the index without touching the files that a person must still preserve separately.
    if !flagged.is_empty() {
        let mut reset = vec!["reset", "--quiet", "HEAD", "--"];
        reset.extend(flagged.iter().map(|(path, _)| path.as_str()));
        run_git_for_preservation(worktree.path(), &reset)?;
    }
    if paths.is_empty() {
        return Ok(Preservation {
            receipt: PreservationReceipt::default(),
            flagged,
        });
    }
    // `--force` because ignored files that the policy treats as user data must be kept too.
    let mut add: Vec<&str> = vec!["add", "--all", "--force", "--"];
    add.extend(paths.iter().map(String::as_str));
    run_git_for_preservation(worktree.path(), &add)?;
    let message: String = format!("kaisa: preserve the work of {}", worktree.branch());
    let mut commit: Vec<&str> = PRESERVATION_IDENTITY.to_vec();
    commit.extend(["commit", "--quiet", "--no-verify", "-m", &message]);
    run_git_for_preservation(worktree.path(), &commit)?;
    let head: Vec<u8> = run_git_for_preservation(worktree.path(), &["rev-parse", "HEAD"])?;
    let head: String = String::from_utf8_lossy(&head).trim().to_owned();
    Ok(Preservation {
        receipt: PreservationReceipt::committed(
            &head,
            &covered.iter().map(String::as_str).collect::<Vec<_>>(),
        ),
        flagged,
    })
}

/// Expand directory inventory entries before scanning or staging. Symlinks are leaves;
/// following a directory link here could walk outside the attempt's worktree.
fn expand_files(
    worktree: &Worktree,
    path: &str,
    files: &mut Vec<String>,
) -> Result<(), WorktreeError> {
    let absolute = worktree.path().join(path);
    if absolute
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_dir())
    {
        let children = std::fs::read_dir(&absolute).map_err(GitError::Spawn)?;
        for child in children {
            let child = child.map_err(GitError::Spawn)?;
            let relative = child.path();
            let relative = relative
                .strip_prefix(worktree.path())
                .expect("a child stays relative");
            expand_files(worktree, &relative.to_string_lossy(), files)?;
        }
    } else {
        files.push(path.to_owned());
    }
    Ok(())
}

/// The credentials found in the file at `path`, or `None` for an entry that is not a readable
/// file (a deletion). A file too large or unreadable to scan is reported as flagged.
fn secrets_in(worktree: &Worktree, path: &str) -> Option<Vec<SecretKind>> {
    let file = worktree.path().join(path);
    let metadata = match std::fs::symlink_metadata(&file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(vec![SecretKind::Unscannable]),
    };
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
    const IGNORED_KEY_FILE: &str = "private/nested/key.pem";
    const IGNORED_NOTE_FILE: &str = "private/notes.txt";

    fn an_attempt_with_ignored_private_data(repository: &TemporaryRepository) -> Worktree {
        repository.commit_file(".gitignore", "private/\n", "ignore private data");
        let worktree = a_worktree_for_attempt_41a(repository);
        the_agent_writes(&worktree, IGNORED_KEY_FILE, LEFTOVER_KEY);
        the_agent_writes(&worktree, IGNORED_NOTE_FILE, "notes worth preserving\n");
        worktree
    }

    fn committed_files(worktree: &Worktree) -> String {
        git_text(worktree.path(), &["ls-tree", "-r", "--name-only", "HEAD"])
            .expect("the preservation commit can be inspected")
    }

    #[test]
    fn an_ignored_directory_with_a_private_key_commits_only_scanned_clean_files() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = an_attempt_with_ignored_private_data(&repository);
        let before = what_needs_preserving(&worktree);
        assert!(
            before.preserve.iter().any(|entry| entry.path == "private/"),
            "the regression must exercise git's collapsed ignored directory inventory"
        );

        let preservation =
            preserve_as_commit(&worktree, &before).expect("clean notes are preserved");
        let committed = committed_files(&worktree);

        assert!(
            !committed.lines().any(|path| path == IGNORED_KEY_FILE),
            "expanding ignored directories must prevent a nested PEM key entering history"
        );
        assert!(
            committed.lines().any(|path| path == IGNORED_NOTE_FILE),
            "clean ignored user data must still be preserved"
        );
        assert!(
            preservation
                .flagged
                .iter()
                .any(|(path, _)| path == IGNORED_KEY_FILE),
            "the report must name the nested key without recording its contents"
        );
        assert!(
            check_deletion(&before, &preservation.receipt).is_err(),
            "a partially preserved directory must keep reclamation blocked"
        );
    }

    #[test]
    fn an_ignored_directory_with_only_clean_files_has_full_receipt_coverage() {
        let repository = a_repository_with_one_commit_on_main();
        repository.commit_file(".gitignore", "private/\n", "ignore private data");
        let worktree = a_worktree_for_attempt_41a(&repository);
        the_agent_writes(&worktree, IGNORED_NOTE_FILE, "notes worth preserving\n");
        let before = what_needs_preserving(&worktree);

        let preservation = preserve_as_commit(&worktree, &before).expect("notes are preserved");

        assert_eq!(
            check_deletion(&before, &preservation.receipt),
            Ok(()),
            "every child of the collapsed directory was scanned and committed"
        );
    }

    #[test]
    fn an_already_staged_private_key_is_removed_from_the_preservation_index() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        the_agent_writes(&worktree, KEY_FILE, LEFTOVER_KEY);
        git_text(worktree.path(), &["add", "--", KEY_FILE]).expect("the agent staged its key");
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "pub fn refresh() {}\n");

        let preservation = preserve_as_commit(&worktree, &what_needs_preserving(&worktree))
            .expect("clean source is preserved");

        assert!(
            !committed_files(&worktree)
                .lines()
                .any(|path| path == KEY_FILE),
            "a pre-staged secret must be removed before committing the whole index"
        );
        assert!(
            preservation
                .flagged
                .iter()
                .any(|(path, _)| path == KEY_FILE),
            "unstaging must keep the secret named in the preservation report"
        );
        assert!(
            worktree.path().join(KEY_FILE).exists(),
            "removing a secret from the index must retain its working copy"
        );
    }

    #[cfg(unix)]
    fn install_marker_hooks(repository: &TemporaryRepository) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let hooks = repository.path().join(".git/marker-hooks");
        std::fs::create_dir_all(&hooks).expect("the hook directory is writable");
        for name in ["prepare-commit-msg", "post-commit", "post-index-change"] {
            let hook = hooks.join(name);
            std::fs::write(&hook, format!("#!/bin/sh\nprintf hook > {name}-was-run\n"))
                .expect("a marker hook can be installed");
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
                .expect("the hook can be made executable");
        }
        git_text(
            repository.path(),
            &["config", "core.hooksPath", &hooks.to_string_lossy()],
        )
        .expect("the repository uses its marker hooks");
        hooks
    }

    #[cfg(unix)]
    #[test]
    fn preservation_with_repository_hooks_never_runs_commit_or_index_hooks() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "pub fn refresh() {}\n");
        let before = what_needs_preserving(&worktree);
        install_marker_hooks(&repository);

        preserve_as_commit(&worktree, &before)
            .expect("preservation bypasses every repository hook");

        for name in ["prepare-commit-msg", "post-commit", "post-index-change"] {
            assert!(
                !worktree.path().join(format!("{name}-was-run")).exists(),
                "{name} must not execute during preservation"
            );
        }
    }
}
