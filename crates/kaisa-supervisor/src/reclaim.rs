//! The reclaimer: preserve an attempt's work, then remove its worktree (architecture §5.5).
//!
//! Every step is a checked transition on the attempt's cleanup axis, so the order cannot be
//! broken: nothing is removed before a receipt covers it, and a retained attempt is never touched.

use std::fmt;
use std::path::Path;

use kaisa_domain::{AttemptState, Cleanup, IllegalTransition};
use kaisa_workspace::{
    IgnoredPolicy, Inventory, Preservation, PreservationReceipt, Worktree, WorktreeError,
    check_deletion, inventory, preserve_as_commit, remove_worktree,
};

/// What a reclamation run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reclamation {
    /// The work is preserved and the worktree removed.
    Reclaimed(PreservationReceipt),
    /// Some paths could not be preserved (for example, a file that looks like a credential), so
    /// the worktree is kept; the attempt stays in `Preserving` until they are.
    Blocked {
        /// Paths the receipt does not cover.
        uncovered: Vec<String>,
    },
    /// A human asked to keep the attempt's resources.
    Retained,
}

/// Why the reclaimer stopped.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReclaimError {
    /// The attempt is not in a state that allows this step (for example, still running).
    Illegal(IllegalTransition),
    /// Git failed while inventorying, preserving or removing.
    Workspace(WorktreeError),
}

impl fmt::Display for ReclaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Illegal(error) => write!(f, "{error}"),
            Self::Workspace(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ReclaimError {}

impl From<IllegalTransition> for ReclaimError {
    fn from(error: IllegalTransition) -> Self {
        Self::Illegal(error)
    }
}

impl From<WorktreeError> for ReclaimError {
    fn from(error: WorktreeError) -> Self {
        Self::Workspace(error)
    }
}

/// Preserves and removes `worktree` of the attempt in `state`, moving its cleanup axis through
/// `Preserving → Preserved → Reclaiming → Reclaimed`.
///
/// # Errors
///
/// Returns [`ReclaimError::Illegal`] if the attempt has not exited or was already reclaimed, and
/// [`ReclaimError::Workspace`] if git fails; the worktree is never removed in either case.
pub fn reclaim(
    state: &mut AttemptState,
    repository: &Path,
    worktree: &Worktree,
    policy: &IgnoredPolicy,
) -> Result<Reclamation, ReclaimError> {
    if state.cleanup() == Cleanup::Retained {
        return Ok(Reclamation::Retained);
    }
    state.begin_preservation()?;
    let before: Inventory = inventory(worktree, policy)?;
    let preservation: Preservation = preserve_as_commit(worktree, &before)?;
    if let Err(uncovered) = check_deletion(&before, &preservation.receipt) {
        return Ok(Reclamation::Blocked {
            uncovered: uncovered.uncovered,
        });
    }
    state.record_preservation()?;
    state.begin_reclaim()?;
    remove_worktree(repository, worktree, &preservation.receipt, policy)?;
    state.finish_reclaim()?;
    Ok(Reclamation::Reclaimed(preservation.receipt))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use kaisa_domain::{AttemptState, ExitStatus};
    use kaisa_workspace::{IgnoredPolicy, Worktree, create_worktree};

    use super::{Reclamation, reclaim};

    /// Distinguishes repositories created by tests running in parallel.
    static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);
    /// The branch of run 41's first attempt.
    const ATTEMPT_BRANCH: &str = "kaisa/41-a";
    /// A private key the agent left behind.
    const LEFTOVER_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n";

    /// A repository with one commit on `main`, removed when dropped.
    struct Repository {
        root: PathBuf,
    }

    impl Repository {
        fn new() -> Self {
            let root: PathBuf = std::env::temp_dir().join(format!(
                "kaisa-supervisor-test-{}-{}",
                std::process::id(),
                NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed)
            ));
            let repo: PathBuf = root.join("repo");
            std::fs::create_dir_all(&repo).expect("the temp directory is writable");
            git(&repo, &["init", "--quiet", "--initial-branch=main"]);
            std::fs::write(repo.join("README.md"), "# demo\n").expect("writable");
            git(&repo, &["add", "README.md"]);
            git(
                &repo,
                &[
                    "-c",
                    "user.name=Kaisa Test",
                    "-c",
                    "user.email=kaisa-test@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "initial",
                ],
            );
            Self { root }
        }

        fn path(&self) -> PathBuf {
            self.root.join("repo")
        }

        fn worktree_for_attempt_41a(&self) -> Worktree {
            create_worktree(
                &self.path(),
                "main",
                ATTEMPT_BRANCH,
                &self.root.join("att_41a"),
            )
            .expect("a fresh worktree can be created")
        }
    }

    impl Drop for Repository {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn git(directory: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(directory)
            .status()
            .expect("git is installed on every CI runner");
        assert!(status.success(), "fixture git command failed: {args:?}");
    }

    fn the_agent_writes(worktree: &Worktree, relative: &str, contents: &str) {
        let file: PathBuf = worktree.path().join(relative);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("the worktree is writable");
        }
        std::fs::write(file, contents).expect("the worktree is writable");
    }

    fn an_attempt_that_exited() -> AttemptState {
        let mut state = AttemptState::admitted();
        state.start().expect("an admitted attempt can start");
        state.confirm_running().expect("a starting attempt can run");
        state
            .record_exit(ExitStatus::Code(ExitStatus::SUCCESS_CODE))
            .expect("a running attempt can exit");
        state
    }

    fn no_build_output() -> IgnoredPolicy {
        IgnoredPolicy::with_build_output(&[])
    }

    #[test]
    fn an_exited_attempt_is_preserved_before_its_worktree_is_removed() {
        let repository: Repository = Repository::new();
        let worktree: Worktree = repository.worktree_for_attempt_41a();
        the_agent_writes(&worktree, "src/session.rs", "pub fn refresh() {}\n");
        let mut state: AttemptState = an_attempt_that_exited();

        let outcome = reclaim(
            &mut state,
            &repository.path(),
            &worktree,
            &no_build_output(),
        )
        .expect("reclaiming succeeds");

        assert!(
            matches!(&outcome, Reclamation::Reclaimed(receipt) if receipt.commit().is_some()),
            "the work is committed before anything is deleted, got {outcome:?}"
        );
        assert!(
            !worktree.path().exists(),
            "a reclaimed attempt leaves no worktree behind"
        );
    }

    #[test]
    fn uncovered_changes_block_reclamation() {
        let repository: Repository = Repository::new();
        let worktree: Worktree = repository.worktree_for_attempt_41a();
        the_agent_writes(&worktree, "deploy/id_ed25519", LEFTOVER_KEY);
        let mut state: AttemptState = an_attempt_that_exited();

        let outcome = reclaim(
            &mut state,
            &repository.path(),
            &worktree,
            &no_build_output(),
        )
        .expect("reclaiming runs");

        assert!(
            matches!(&outcome, Reclamation::Blocked { uncovered } if !uncovered.is_empty()),
            "work that could not be preserved blocks deletion, got {outcome:?}"
        );
        assert!(
            worktree.path().exists(),
            "the worktree stays until every file in it is preserved"
        );
    }

    #[test]
    fn a_retained_attempt_is_not_reclaimed() {
        let repository: Repository = Repository::new();
        let worktree: Worktree = repository.worktree_for_attempt_41a();
        the_agent_writes(&worktree, "src/session.rs", "pub fn refresh() {}\n");
        let mut state: AttemptState = an_attempt_that_exited();
        state.retain().expect("an exited attempt can be retained");

        let outcome = reclaim(
            &mut state,
            &repository.path(),
            &worktree,
            &no_build_output(),
        )
        .expect("reclaiming runs");

        assert_eq!(
            (outcome, worktree.path().exists()),
            (Reclamation::Retained, true),
            "work the user asked to keep is never reclaimed"
        );
    }
}
