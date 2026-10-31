//! Preserving an attempt's work before its worktree is removed (architecture §5.5, step 4–5).
//!
//! Clean changes become a WIP commit on the attempt branch, so nothing an agent did is lost when
//! its worktree is reclaimed. The commit uses Kaisa's own identity and skips repository hooks:
//! preservation permits user-configured filters but attributes alone cannot install programs.

use crate::git::{GitError, run_git_for_preservation, stage_paths};
use crate::inventory::Inventory;
use crate::receipt::PreservationReceipt;
use crate::secrets::{SecretKind, scan_bytes};
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
    let hooks = worktree.preservation_policy()?;
    let mut paths: Vec<String> = Vec::new();
    let mut covered: Vec<String> = Vec::new();
    let mut flagged: Vec<(String, Vec<SecretKind>)> = Vec::new();
    let mut expanded = Vec::new();
    for entry in &inventory.preserve {
        let mut files = Vec::new();
        expand_files(worktree, &entry.path, &mut files)?;
        expanded.push((entry, files));
    }
    let files: Vec<_> = expanded
        .iter()
        .flat_map(|(_, files)| files.iter())
        .collect();
    let mut encoded = std::collections::HashSet::new();
    // Bound argv size while avoiding one Git process per file in large directories.
    for batch in files.chunks(128) {
        let mut args = vec!["check-attr", "-z", "working-tree-encoding", "--"];
        args.extend(batch.iter().map(|path| path.as_str()));
        let attributes = run_git_for_preservation(worktree.path(), &hooks, &args)?;
        let fields: Vec<_> = attributes.split(|byte| *byte == 0).collect();
        for record in fields.as_chunks::<3>().0 {
            if record[2] != b"unspecified" && record[2] != b"unset" {
                encoded.insert(String::from_utf8_lossy(record[0]).into_owned());
            }
        }
    }
    for (entry, files) in expanded {
        let mut all_clean = true;
        for path in files {
            // Git can decode arbitrary encodings before committing. Raw byte scans
            // cannot certify these files; retain them for manual preservation.
            let kinds = if encoded.contains(&path) {
                Some(vec![SecretKind::Unscannable])
            } else {
                secrets_in(worktree, &path)
            };
            match kinds {
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
    // Restore only flagged entries, keeping staged rename/deletion intent. Read raw
    // index names so lossy inventory paths can never leave staged secrets behind.
    let indexed = run_git_for_preservation(worktree.path(), &hooks, &["ls-files", "-z"])?;
    let mut excluded = Vec::new();
    for raw in indexed
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let name = String::from_utf8_lossy(raw);
        if name.contains('\u{fffd}')
            || flagged.iter().any(|(path, _)| {
                name == *path || name.starts_with(&format!("{}/", path.trim_end_matches('/')))
            })
        {
            excluded.extend_from_slice(raw);
            excluded.push(0);
        }
    }
    crate::git::restore_flagged(worktree.path(), &hooks, &excluded)?;
    if paths.is_empty() {
        return Ok(Preservation {
            receipt: PreservationReceipt::default(),
            flagged,
        });
    }
    // `--force` because ignored files that the policy treats as user data must be kept too.
    let stageable: Vec<String> = paths
        .iter()
        .filter(|path| {
            let absolute = worktree.path().join(path);
            absolute.symlink_metadata().is_ok()
                || indexed.split(|b| *b == 0).any(|raw| raw == path.as_bytes())
        })
        .cloned()
        .collect();
    if !stageable.is_empty() {
        stage_paths(worktree.path(), &hooks, &stageable)?;
    }
    let message: String = format!("kaisa: preserve the work of {}", worktree.branch());
    let mut commit: Vec<&str> = PRESERVATION_IDENTITY.to_vec();
    commit.extend(["commit", "--quiet", "--no-verify", "-m", &message]);
    run_git_for_preservation(worktree.path(), &hooks, &commit)?;
    let head: Vec<u8> = run_git_for_preservation(worktree.path(), &hooks, &["rev-parse", "HEAD"])?;
    let head: String = String::from_utf8_lossy(&head).trim().to_owned();
    let tree = run_git_for_preservation(worktree.path(), &hooks, &["ls-tree", "-r", "-z", &head])?;
    let entries: Vec<(&[u8], &str)> = tree
        .split(|b| *b == 0)
        .filter_map(|record| {
            let (meta, path) = record.split_at(record.iter().position(|b| *b == b'\t')?);
            Some((meta, std::str::from_utf8(&path[1..]).ok()?))
        })
        .collect();
    let committed: Vec<&str> = entries
        .iter()
        .filter(|(meta, _)| !meta.starts_with(b"160000 "))
        .map(|(_, path)| *path)
        .collect();
    covered.retain(|path| {
        let absolute = worktree.path().join(path);
        if !absolute.exists() && !absolute.is_symlink() {
            return !entries.iter().any(|(_, present)| *present == path); // deletion is represented by absence
        }
        if absolute.is_dir() {
            let prefix = format!("{}/", path.trim_end_matches('/'));
            return committed.iter().any(|child| child.starts_with(&prefix))
                && paths
                    .iter()
                    .filter(|child| child.starts_with(&format!("{}/", path.trim_end_matches('/'))))
                    .all(|child| committed.contains(&child.as_str()));
        }
        committed.contains(&path.as_str())
    });
    let mut verified = Vec::new();
    for batch in covered.chunks(128) {
        if content_matches_head(worktree, &hooks, batch) {
            verified.extend_from_slice(batch);
        } else {
            verified.extend(
                batch
                    .iter()
                    .filter(|path| {
                        content_matches_head(worktree, &hooks, std::slice::from_ref(path))
                    })
                    .cloned(),
            );
        }
    }
    covered = verified;
    Ok(Preservation {
        receipt: PreservationReceipt::committed(
            &head,
            &covered.iter().map(String::as_str).collect::<Vec<_>>(),
        ),
        flagged,
    })
}

/// A receipt requires equal contents, not just a path present in the commit.
fn content_matches_head(
    worktree: &Worktree,
    hooks: &crate::git::EmptyHooksDirectory,
    paths: &[String],
) -> bool {
    let mut args = vec![
        "diff",
        "--quiet",
        "--no-ext-diff",
        "--no-textconv",
        "HEAD",
        "--",
    ];
    args.extend(paths.iter().map(String::as_str));
    run_git_for_preservation(worktree.path(), hooks, &args).is_ok()
}

/// Expand directory inventory entries before scanning or staging. Symlinks are leaves;
/// following a directory link here could walk outside the attempt's worktree.
fn expand_files(
    worktree: &Worktree,
    path: &str,
    files: &mut Vec<String>,
) -> Result<(), WorktreeError> {
    // Inventory paths are UTF-8 strings. Never traverse a lossy name: it could
    // collide with a different, valid filename. Genuine replacement-character
    // names are conservatively left for manual preservation too.
    if path.contains('\u{fffd}') {
        files.push(path.to_owned());
        return Ok(());
    }
    let absolute = worktree.path().join(path);
    if absolute
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_dir())
    {
        if absolute.join(".git").symlink_metadata().is_ok() {
            files.push(path.to_owned());
            return Ok(());
        }
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
    if path.contains('\u{fffd}') {
        return Some(vec![SecretKind::Unscannable]);
    }
    let file = worktree.path().join(path);
    if crate::secrets::denied_filename(path) {
        return Some(vec![SecretKind::PrivateKey]);
    }
    if file.is_dir() && file.join(".git").symlink_metadata().is_ok() {
        return Some(vec![SecretKind::Unscannable]);
    }
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
        Ok(bytes) => Some(scan_bytes(&bytes)),
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
    /// Install a real executable that leaves evidence whenever Git invokes it.
    #[cfg(unix)]
    fn a_marker_program(repository: &TemporaryRepository, name: &str) -> (String, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let script = repository.sibling(&format!("{name}.sh"));
        let marker = repository.sibling(&format!("{name}-ran"));
        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf ran > '{}'\nexit 0\n", marker.display()),
        )
        .expect("write marker program");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("make program executable");
        (script.to_string_lossy().into_owned(), marker)
    }

    #[cfg(unix)]
    #[test]
    fn preservation_with_fsmonitor_and_signing_programs_never_executes_them() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "source worth preserving");
        let (monitor, monitor_marker) = a_marker_program(&repository, "fsmonitor");
        let (signer, signer_marker) = a_marker_program(&repository, "gpg");
        for (key, value) in [
            ("core.fsmonitor", monitor.as_str()),
            ("gpg.program", signer.as_str()),
            ("commit.gpgsign", "true"),
        ] {
            git_text(repository.path(), &["config", key, value]).expect("configure executable");
        }
        let before = what_needs_preserving(&worktree);
        let result = preserve_as_commit(&worktree, &before);
        assert!(
            !monitor_marker.exists(),
            "direct fsmonitor configuration must remain disabled"
        );
        assert!(
            !signer_marker.exists(),
            "direct signing configuration must remain disabled"
        );
        result.expect("preserve with direct executable settings");
        let included = repository.sibling("included-executables.config");
        for (key, value) in [
            ("core.fsmonitor", monitor.as_str()),
            ("gpg.program", signer.as_str()),
            ("commit.gpgsign", "true"),
        ] {
            git_text(
                repository.path(),
                &[
                    "config",
                    "--file",
                    included.to_str().expect("UTF-8 fixture"),
                    key,
                    value,
                ],
            )
            .expect("configure included executable");
        }
        git_text(
            repository.path(),
            &[
                "config",
                "include.path",
                included.to_str().expect("UTF-8 fixture"),
            ],
        )
        .expect("include executable configuration");
        the_agent_writes(
            &worktree,
            NEW_SOURCE_FILE,
            "another change worth preserving",
        );
        let before = what_needs_preserving(&worktree);
        let result = preserve_as_commit(&worktree, &before);
        assert!(
            !monitor_marker.exists(),
            "included fsmonitor configuration must remain disabled"
        );
        assert!(
            !signer_marker.exists(),
            "included signing configuration must remain disabled"
        );
        result.expect("preserve with included executable settings");
    }

    #[cfg(unix)]
    #[test]
    fn a_user_configured_attribute_filter_is_allowed_to_execute() {
        for driver in ["clean", "process"] {
            let repository = a_repository_with_one_commit_on_main();
            let worktree = a_worktree_for_attempt_41a(&repository);
            let (filter, marker) = a_marker_program(&repository, "filter");
            the_agent_writes(&worktree, ".gitattributes", "*.rs filter=hostile\n");
            the_agent_writes(&worktree, NEW_SOURCE_FILE, "source worth preserving");
            git_text(
                repository.path(),
                &["config", &format!("filter.hostile.{driver}"), &filter],
            )
            .expect("configure filter");
            git_text(
                repository.path(),
                &["config", "filter.hostile.smudge", &filter],
            )
            .expect("configure smudge filter");
            git_text(
                repository.path(),
                &["config", "filter.hostile.required", "true"],
            )
            .expect("require filter");
            let result = preserve_as_commit(&worktree, &what_needs_preserving(&worktree));
            assert!(
                marker.exists(),
                "a filter defined in user-owned config may execute its {driver} program"
            );
            if driver == "clean" {
                result.expect("configured clean filter runs successfully");
            } else {
                assert!(
                    result.is_err(),
                    "a process filter that violates the Git protocol must refuse preservation"
                );
            }
        }
    }

    fn an_attempt_with_nested_repository(repository: &TemporaryRepository) -> Worktree {
        repository.commit_file(".gitignore", "private/\n", "ignore private user data");
        let worktree = a_worktree_for_attempt_41a(repository);
        let nested = worktree.path().join("private/sub");
        std::fs::create_dir_all(&nested).expect("nested directory is writable");
        git_text(&nested, &["init", "--quiet"]).expect("initialize nested repository");
        std::fs::write(nested.join("notes"), "irreplaceable nested data")
            .expect("write nested data");
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "clean source");
        worktree
    }

    #[test]
    fn an_ignored_nested_repository_is_flagged_and_blocks_reclamation() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = an_attempt_with_nested_repository(&repository);
        let before = what_needs_preserving(&worktree);
        let preservation =
            preserve_as_commit(&worktree, &before).expect("unrelated clean source is preserved");
        assert!(
            preservation
                .flagged
                .iter()
                .any(|(path, _)| path.starts_with("private/")),
            "nested repository must be reported as unpreservable"
        );
        assert!(
            check_deletion(&before, &preservation.receipt).is_err(),
            "nested repository data must block deletion"
        );
        assert!(
            check_deletion(&what_needs_preserving(&worktree), &preservation.receipt).is_err(),
            "fresh inventory must still block reclamation"
        );
        assert!(
            worktree.path().join("private/sub/notes").exists(),
            "nested data must remain available for manual preservation"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_non_utf8_filename_is_flagged_while_clean_work_is_committed() {
        use std::os::unix::ffi::OsStringExt;
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        let name = std::ffi::OsString::from_vec(b"private-\xff".to_vec());
        let file = worktree.path().join(name);
        std::fs::write(&file, LEFTOVER_KEY).expect("write a non-UTF-8 file");
        // Already-staged data must also be excluded from the final commit.
        crate::git::run_git(worktree.path(), &["add", "--all"])
            .expect("agent stages non-UTF-8 key");
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "clean source");
        let before = what_needs_preserving(&worktree);
        let preservation =
            preserve_as_commit(&worktree, &before).expect("clean work still commits");
        assert!(
            !preservation.flagged.is_empty(),
            "unrepresentable filename must be named in the report"
        );
        assert!(
            check_deletion(&before, &preservation.receipt).is_err(),
            "unrepresentable filename must block deletion"
        );
        assert!(
            !committed_files(&worktree).contains("private-"),
            "pre-staged unscannable data must not leak into the commit"
        );
        assert!(
            file.exists(),
            "the original filename and contents must remain on disk"
        );
    }

    fn pgp_private_key() -> &'static str {
        "-----BEGIN PGP PRIVATE KEY BLOCK-----\nprivate material\n"
    }

    fn utf16_private_key(little_endian: bool, with_bom: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        if with_bom {
            bytes.extend(if little_endian {
                [0xff, 0xfe]
            } else {
                [0xfe, 0xff]
            });
        }
        for unit in LEFTOVER_KEY.encode_utf16() {
            bytes.extend(if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        bytes
    }

    fn git_status_with_an_unrepresentable_filename_and_clean_source() -> &'static [u8] {
        b"? private-\xff\0? src/session.rs\0"
    }

    #[test]
    fn pgp_and_utf16_private_keys_are_excluded_from_preservation() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        the_agent_writes(&worktree, "pgp.asc", pgp_private_key());
        let mut names = vec!["pgp.asc".to_owned()];
        for little in [true, false] {
            for bom in [true, false] {
                let name = format!("key-{little}-{bom}");
                std::fs::write(worktree.path().join(&name), utf16_private_key(little, bom))
                    .expect("write UTF-16 key");
                names.push(name);
            }
        }
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "clean source");
        let before = what_needs_preserving(&worktree);
        let preservation = preserve_as_commit(&worktree, &before).expect("clean source commits");
        for name in names {
            assert!(
                preservation
                    .flagged
                    .iter()
                    .any(|(path, kinds)| path == &name
                        && kinds.contains(&crate::secrets::SecretKind::PrivateKey)),
                "private key {name} must be detected in its original encoding"
            );
            assert!(
                !preservation.receipt.covers(&name),
                "private key {name} cannot be covered by a commit receipt"
            );
        }
        assert!(
            check_deletion(&before, &preservation.receipt).is_err(),
            "encoded credentials must keep reclamation blocked"
        );
    }

    #[test]
    fn a_directory_exceeding_process_argument_limits_is_preserved() {
        const FILE_COUNT: usize = 9000;
        const NAME_PADDING: usize = 140;
        let repository = a_repository_with_one_commit_on_main();
        repository.commit_file(".gitignore", "private/\n", "collapse large user directory");
        let worktree = a_worktree_for_attempt_41a(&repository);
        for number in 0..FILE_COUNT {
            the_agent_writes(
                &worktree,
                &format!("private/{number}-{}", "n".repeat(NAME_PADDING)),
                "notes",
            );
        }
        let before = what_needs_preserving(&worktree);
        let preservation = preserve_as_commit(&worktree, &before)
            .expect("pathspec file avoids process argv limits");
        assert_eq!(
            committed_files(&worktree)
                .lines()
                .filter(|path| path.starts_with("private/"))
                .count(),
            FILE_COUNT,
            "every expanded file must reach the commit"
        );
        assert!(
            check_deletion(&before, &preservation.receipt).is_ok(),
            "large directory must have complete coverage"
        );
    }
    #[test]
    fn a_lossily_decoded_status_path_is_flagged_without_losing_clean_work() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        // APFS rejects non-UTF-8 names. Feed real porcelain bytes through the parser
        // and include a replacement-character collision to guard against lossy staging.
        let raw_status = git_status_with_an_unrepresentable_filename_and_clean_source();
        let entries = crate::status::parse_status(raw_status)
            .expect("Git status with arbitrary filename bytes parses");
        let before = crate::inventory::plan_inventory(&entries, &no_build_output());
        let unrepresentable = &before
            .preserve
            .first()
            .expect("status contains unrepresentable path")
            .path;
        the_agent_writes(&worktree, unrepresentable, "collision data");
        git_text(worktree.path(), &["add", "--all"]).expect("agent stages a collision file");
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "clean source");
        let preservation =
            preserve_as_commit(&worktree, &before).expect("clean work still commits");
        assert!(
            preservation
                .flagged
                .iter()
                .any(|(path, kinds)| path == unrepresentable
                    && kinds.contains(&crate::secrets::SecretKind::Unscannable)),
            "lossy status path must be reported as unscannable"
        );
        assert!(
            check_deletion(&before, &preservation.receipt).is_err(),
            "unrepresentable original path must block reclamation"
        );
        assert!(
            !committed_files(&worktree).contains("private-"),
            "a lossy filename collision must never enter the commit"
        );
        assert!(
            committed_files(&worktree).contains(NEW_SOURCE_FILE),
            "unrepresentable data must not prevent clean work preservation"
        );
    }
    #[test]
    fn a_staged_rename_and_deletion_keep_the_old_paths_out_of_the_commit() {
        let repository = a_repository_with_one_commit_on_main();
        repository.commit_file("obsolete.txt", "old data", "add obsolete file");
        let worktree = a_worktree_for_attempt_41a(&repository);
        git_text(worktree.path(), &["mv", "README.md", "renamed.md"]).expect("stage rename");
        git_text(worktree.path(), &["rm", "obsolete.txt"]).expect("stage deletion");
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "clean source");
        let before = what_needs_preserving(&worktree);
        let preservation = preserve_as_commit(&worktree, &before).expect("preserve index intent");
        let tree = committed_files(&worktree);
        assert!(
            !tree
                .lines()
                .any(|path| path == "README.md" || path == "obsolete.txt"),
            "rename sources and staged deletions must stay absent"
        );
        assert!(
            tree.lines().any(|path| path == "renamed.md"),
            "rename destination must survive"
        );
        assert!(
            check_deletion(&before, &preservation.receipt).is_ok(),
            "verified rename and deletion should permit reclamation"
        );
    }

    #[test]
    fn a_repointed_git_file_refuses_inventory_and_preservation_even_after_recovery() {
        let repository = a_repository_with_one_commit_on_main();
        let other = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "irreplaceable work");
        let before = what_needs_preserving(&worktree);
        std::fs::write(
            worktree.path().join(".git"),
            format!("gitdir: {}\n", other.path().join(".git").display()),
        )
        .expect("simulate pointer replacement");
        let recovered = crate::worktree::open_worktree(repository.path(), worktree.path())
            .expect("recover from repository records")
            .expect("registered worktree exists");
        for candidate in [&worktree, &recovered] {
            let failure = preserve_as_commit(candidate, &before)
                .expect_err("changed pointer must refuse preservation");
            assert!(
                failure.to_string().contains("pointer changed"),
                "refusal must explain the tampered Git pointer"
            );
            assert!(
                inventory(candidate, &no_build_output()).is_err(),
                "inventory must also reject the attacker pointer"
            );
        }
        assert!(
            worktree.path().join(NEW_SOURCE_FILE).exists(),
            "refusal must retain work for recovery"
        );
    }

    #[test]
    fn an_empty_gitlink_directory_never_receives_blob_coverage() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        let head =
            git_text(worktree.path(), &["rev-parse", "HEAD"]).expect("initial commit exists");
        git_text(
            worktree.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{head},sub"),
            ],
        )
        .expect("stage gitlink");
        std::fs::create_dir(worktree.path().join("sub"))
            .expect("unpopulated submodule directory exists");
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "clean source");
        let entries = ["sub", NEW_SOURCE_FILE].map(|path| crate::status::StatusEntry {
            path: path.into(),
            kind: crate::status::EntryKind::Tracked,
        });
        let before = crate::inventory::plan_inventory(&entries, &no_build_output());
        let preservation =
            preserve_as_commit(&worktree, &before).expect("clean source commits alongside gitlink");
        assert!(
            !preservation.receipt.covers("sub"),
            "a gitlink has no preserved file contents and must not get directory coverage"
        );
        assert!(
            check_deletion(&before, &preservation.receipt).is_err(),
            "unverified gitlink must block reclamation"
        );
    }

    fn leftover_credentials() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            (
                "aws.env",
                format!("AWS_SECRET_ACCESS_KEY={}\n", "aB1/".repeat(10)).into_bytes(),
            ),
            (
                "stripe-secret.txt",
                format!("sk_live_{}", "a".repeat(24)).into_bytes(),
            ),
            (
                "stripe-restricted.txt",
                format!("rk_live_{}", "a".repeat(24)).into_bytes(),
            ),
            ("keystore.p12", vec![0, 0xff, 0]),
            ("keys.pfx", vec![0, 0xff, 0]),
            ("keys.jks", vec![0, 0xff, 0]),
            ("app.keystore", vec![0, 0xff, 0]),
            (".env", b"opaque secret".to_vec()),
            (".npmrc", b"opaque secret".to_vec()),
            (".netrc", b"opaque secret".to_vec()),
            ("state.tfstate", b"opaque state".to_vec()),
            ("vault.kdbx", vec![0, 0xff, 0]),
            ("slack.txt", format!("xoxb-{}", "a".repeat(24)).into_bytes()),
            ("google.txt", format!("AIza{}", "a".repeat(35)).into_bytes()),
            (
                "openai.txt",
                format!("sk-proj-{}", "a".repeat(24)).into_bytes(),
            ),
            (
                "jwt.txt",
                b"eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.signature".to_vec(),
            ),
            (
                "short-aws.txt",
                b"AWS_SECRET_ACCESS_KEY=shortButSensitive".to_vec(),
            ),
            ("id_rsa", b"opaque SSH key".to_vec()),
            ("id_ed25519", b"opaque SSH key".to_vec()),
        ]
    }

    #[test]
    fn standalone_aws_stripe_and_named_keystores_are_flagged_even_when_staged() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        let credentials = leftover_credentials();
        for (name, bytes) in &credentials {
            std::fs::write(worktree.path().join(name), bytes).expect("write credential fixture");
        }
        git_text(worktree.path(), &["add", "--all"]).expect("agent pre-stages credentials");
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "clean source");
        let before = what_needs_preserving(&worktree);
        let preservation =
            preserve_as_commit(&worktree, &before).expect("clean source still commits");
        let tree = committed_files(&worktree);
        for (name, _) in credentials {
            assert!(
                preservation.flagged.iter().any(|(path, _)| path == name),
                "credential {name} must be flagged"
            );
            assert!(
                !tree.lines().any(|path| path == name),
                "credential {name} must not enter history even when staged"
            );
            assert!(
                !preservation.receipt.covers(name),
                "credential {name} must block reclamation"
            );
        }
        assert!(
            check_deletion(&before, &preservation.receipt).is_err(),
            "leftover credentials must prevent worktree deletion"
        );
    }

    #[cfg(unix)]
    #[test]
    fn user_owned_includes_and_worktree_configuration_keep_their_filters() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        let (program, marker) = a_marker_program(&repository, "included-filter");
        let included = repository.sibling("hostile.config");
        git_text(
            repository.path(),
            &[
                "config",
                "--file",
                included.to_str().expect("UTF-8 fixture"),
                "filter.included.clean",
                &program,
            ],
        )
        .expect("write included executable setting");
        git_text(
            repository.path(),
            &[
                "config",
                "include.path",
                included.to_str().expect("UTF-8 fixture"),
            ],
        )
        .expect("include local config");
        git_text(
            repository.path(),
            &["config", "extensions.worktreeConfig", "true"],
        )
        .expect("enable worktree config");
        git_text(
            worktree.path(),
            &["config", "--worktree", "filter.worktree.clean", &program],
        )
        .expect("configure worktree executable");
        the_agent_writes(
            &worktree,
            ".gitattributes",
            "*.rs filter=included\n*.txt filter=worktree\n",
        );
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "clean source");
        the_agent_writes(&worktree, "note.txt", "worktree-config filter input");
        let before = what_needs_preserving(&worktree);
        let result = preserve_as_commit(&worktree, &before);
        assert!(
            marker.exists(),
            "user-owned includes and worktree config may define executable filters"
        );
        result.expect("trusted filter definitions are retained");
        let policy = worktree
            .preservation_policy()
            .expect("bind trusted Git metadata");
        let config =
            crate::git::run_git_for_preservation(worktree.path(), &policy, &["config", "--list"])
                .expect("read effective config");
        assert!(
            String::from_utf8_lossy(&config).contains("filter.included")
                && String::from_utf8_lossy(&config).contains("filter.worktree"),
            "user-owned includes and per-worktree configuration must remain readable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn global_excludes_autocrlf_and_filters_survive_in_an_isolated_process() {
        let repository = a_repository_with_one_commit_on_main();
        let global = repository.sibling("global.config");
        let system = repository.sibling("system.config");
        let excludes = repository.sibling("global.ignore");
        std::fs::write(&excludes, "target/\n").expect("write global build ignore");
        let (filter, marker) = a_marker_program(&repository, "trusted-filter");
        let lfs_pointer = "version https://git-lfs.github.com/spec/v1\noid sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\nsize 12\n";
        std::fs::write(&filter, format!("#!/bin/sh\nprintf ran > '{}'\ncat >/dev/null\ncat <<'POINTER'\n{lfs_pointer}POINTER\n", marker.display())).expect("install a user-owned LFS-like clean filter");
        for (key, value) in [
            ("core.excludesFile", excludes.to_str().expect("UTF-8 path")),
            ("core.autocrlf", "true"),
            ("filter.trusted.clean", &filter),
            ("core.eol", "lf"),
        ] {
            git_text(
                repository.path(),
                &[
                    "config",
                    "--file",
                    global.to_str().expect("UTF-8 path"),
                    key,
                    value,
                ],
            )
            .expect("write trusted global setting");
        }
        std::fs::write(&system, "[core]\n longpaths = true\n")
            .expect("write trusted system setting");
        let output =
            std::process::Command::new(std::env::current_exe().expect("test binary exists"))
                .args([
                    "--exact",
                    "preserve::tests::trusted_global_configuration_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("GIT_CONFIG_GLOBAL", global)
                .env("GIT_CONFIG_SYSTEM", system)
                .env_remove("GIT_CONFIG_NOSYSTEM")
                .output()
                .expect("run isolated Git environment");
        assert!(
            output.status.success(),
            "global/system configuration regression failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            marker.exists(),
            "a user-installed global filter must be allowed to execute"
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "only run by the parent test with isolated global/system Git settings"]
    fn trusted_global_configuration_child() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        the_agent_writes(&worktree, "target/debug/build-output", "disposable output");
        the_agent_writes(&worktree, "lines.txt", "line one\r\nline two\r\n");
        the_agent_writes(&worktree, ".gitattributes", "*.bin filter=trusted\n");
        the_agent_writes(&worktree, "payload.bin", "filter input");
        let before = inventory(&worktree, &IgnoredPolicy::with_build_output(&["target"]))
            .expect("inventory uses global excludes");
        assert!(
            before
                .disposable
                .iter()
                .any(|entry| entry.path.starts_with("target/")),
            "global excludes must classify build output as disposable"
        );
        let preservation =
            preserve_as_commit(&worktree, &before).expect("preserve with global settings");
        let bytes = crate::git::run_git(worktree.path(), &["show", "HEAD:lines.txt"])
            .expect("read normalized blob");
        assert_eq!(
            bytes, b"line one\nline two\n",
            "global autocrlf must normalize CRLF"
        );
        assert!(
            !committed_files(&worktree).contains("target/"),
            "global ignored build output must stay out of the commit"
        );
        let policy = worktree.preservation_policy().expect("trusted policy");
        let longpaths = crate::git::run_git_for_preservation(
            worktree.path(),
            &policy,
            &["config", "--get", "core.longpaths"],
        )
        .expect("system setting survives");
        assert_eq!(
            longpaths, b"true\n",
            "system configuration must remain enabled"
        );
        assert!(
            check_deletion(&before, &preservation.receipt).is_ok(),
            "trusted settings must preserve normal reclamation"
        );
    }
    #[test]
    fn attributes_without_a_user_defined_filter_preserve_the_original_bytes() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        the_agent_writes(
            &worktree,
            ".gitattributes",
            "*.rs filter=not-installed-by-user\n",
        );
        let source = "source must survive an undefined filter\n";
        the_agent_writes(&worktree, NEW_SOURCE_FILE, source);
        let before = what_needs_preserving(&worktree);
        let preservation = preserve_as_commit(&worktree, &before)
            .expect("attributes cannot install an executable filter");
        let blob = crate::git::run_git(
            worktree.path(),
            &["show", &format!("HEAD:{NEW_SOURCE_FILE}")],
        )
        .expect("read original blob");
        assert_eq!(
            blob,
            source.as_bytes(),
            "an agent-owned attribute cannot define a new filter command"
        );
        assert!(
            check_deletion(&before, &preservation.receipt).is_ok(),
            "undefined filters must not lose otherwise clean work"
        );
    }
    #[test]
    fn a_deletion_without_new_files_is_committed_whether_staged_or_unstaged() {
        for staged in [false, true] {
            let repository = a_repository_with_one_commit_on_main();
            let worktree = a_worktree_for_attempt_41a(&repository);
            if staged {
                git_text(worktree.path(), &["rm", "README.md"]).expect("stage deletion");
            } else {
                std::fs::remove_file(worktree.path().join("README.md"))
                    .expect("delete tracked file");
            }
            let before = what_needs_preserving(&worktree);
            let preservation =
                preserve_as_commit(&worktree, &before).expect("deletions alone require a commit");
            assert!(
                !committed_files(&worktree)
                    .lines()
                    .any(|path| path == "README.md"),
                "deleted path must remain absent, staged={staged}"
            );
            assert!(
                check_deletion(&before, &preservation.receipt).is_ok(),
                "committed deletion must be covered, staged={staged}"
            );
        }
    }
    #[test]
    fn working_tree_encoding_files_are_flagged_before_git_decodes_a_secret() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        let encoded_path = "encoded.txt";
        the_agent_writes(
            &worktree,
            ".gitattributes",
            "encoded.txt working-tree-encoding=UTF-32LE\n",
        );
        let secret = "AWS_KEY=AKIAABCDEFGHIJKLMNOP\n";
        let encoded: Vec<u8> = secret
            .chars()
            .flat_map(|character| u32::from(character).to_le_bytes())
            .collect();
        std::fs::write(worktree.path().join(encoded_path), encoded).expect("write encoded secret");
        git_text(worktree.path(), &["add", "--", encoded_path])
            .expect("agent stages encoded secret");
        assert_eq!(
            git_text(worktree.path(), &["show", ":encoded.txt"]).expect("read Git's decoded blob"),
            secret.trim(),
            "the fixture must reproduce a plain-text secret in Git's staged blob"
        );
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "clean source");
        let before = what_needs_preserving(&worktree);
        let preservation = preserve_as_commit(&worktree, &before).expect("preserve clean source");
        assert!(
            preservation
                .flagged
                .iter()
                .any(|(path, kinds)| path == encoded_path
                    && kinds.contains(&crate::secrets::SecretKind::Unscannable)),
            "Git's encoding conversion must never bypass the raw byte scanner"
        );
        assert!(
            !committed_files(&worktree)
                .lines()
                .any(|path| path == encoded_path),
            "encoded secrets must stay out of history"
        );
        assert!(
            check_deletion(&before, &preservation.receipt).is_err(),
            "encoded data must remain available for manual preservation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_gitlink_with_hostile_nested_filters_is_unscannable_without_executing_them() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        let nested = worktree.path().join("sub");
        std::fs::create_dir_all(&nested).expect("create nested checkout");
        git_text(&nested, &["init", "--quiet"]).expect("initialize hostile nested repository");
        std::fs::write(nested.join("notes"), "original nested data").expect("write nested file");
        git_text(&nested, &["add", "notes"]).expect("stage nested file");
        git_text(
            &nested,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@localhost",
                "commit",
                "--quiet",
                "-m",
                "nested base",
            ],
        )
        .expect("commit nested data");
        let head = git_text(&nested, &["rev-parse", "HEAD"]).expect("read nested commit");
        git_text(
            worktree.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{head},sub"),
            ],
        )
        .expect("stage matching parent gitlink");
        git_text(
            worktree.path(),
            &["commit", "--quiet", "-m", "record gitlink"],
        )
        .expect("commit gitlink before hostile configuration");
        let marker = repository.sibling("nested-filter-executed");
        let filter = format!("touch '{}'; cat", marker.display());
        git_text(&nested, &["config", "filter.hostile.clean", &filter])
            .expect("install hostile nested filter");
        std::fs::write(nested.join(".gitattributes"), "* filter=hostile\n")
            .expect("set nested attributes");
        std::fs::write(nested.join("notes"), "modified nested data").expect("modify nested file");
        the_agent_writes(&worktree, NEW_SOURCE_FILE, "clean source");
        git_text(
            worktree.path(),
            &["status", "--porcelain=v2", "--ignore-submodules=none"],
        )
        .expect("demonstrate unsafe nested inspection");
        assert!(
            marker.exists(),
            "the hostile fixture must execute its filter with ordinary Git status"
        );
        std::fs::remove_file(&marker).expect("clear reproduction marker");
        let unbound_policy = crate::git::EmptyHooksDirectory::create()
            .expect("create policy before metadata binding");
        crate::git::run_git_for_preservation(
            worktree.path(),
            &unbound_policy,
            &["status", "--porcelain=v2"],
        )
        .expect("configuration discovery policy must suppress nested inspection too");
        assert!(
            !marker.exists(),
            "every preservation command must suppress nested configuration even before metadata binding"
        );
        let before = what_needs_preserving(&worktree);
        assert!(
            !marker.exists(),
            "inventory must not execute a gitlink repository's filter"
        );
        let preservation = preserve_as_commit(&worktree, &before)
            .expect("preserve clean source beside nested repository");
        assert!(
            !marker.exists(),
            "add and commit must not execute nested configuration either"
        );
        assert!(
            preservation
                .flagged
                .iter()
                .any(|(path, kinds)| path == "sub"
                    && kinds.contains(&crate::secrets::SecretKind::Unscannable)),
            "ignored submodule status must still flag nested data"
        );
        assert!(
            check_deletion(&before, &preservation.receipt).is_err(),
            "nested repository contents must block reclamation"
        );
    }

    #[test]
    fn an_existing_head_path_with_changed_contents_has_no_content_coverage() {
        let repository = a_repository_with_one_commit_on_main();
        let worktree = a_worktree_for_attempt_41a(&repository);
        let policy = worktree
            .preservation_policy()
            .expect("bind safe Git policy");
        assert!(
            super::content_matches_head(&worktree, &policy, &["README.md".to_owned()]),
            "unchanged tracked contents should match HEAD"
        );
        the_agent_writes(&worktree, "README.md", "uncommitted replacement");
        assert!(
            !super::content_matches_head(&worktree, &policy, &["README.md".to_owned()]),
            "path existence alone cannot prove content preservation"
        );
    }
}
