//! The candidate a verification looks at: an attempt's preserved commit, frozen (T21a,
//! ADR-0015). Its tree hash is the identity every piece of evidence and every review binds to;
//! git derives it from the content, so a changed candidate can never pass for the reviewed one.

use std::path::Path;

use crate::git::{GitError, git_text, run_git};

/// A frozen candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The commit holding the attempt's work.
    pub commit: String,
    /// The hash of that commit's tree: what was actually reviewed and checked.
    pub tree: String,
}

/// Freezes `revision` of `repository` as a candidate, resolving it to a full commit and tree.
///
/// # Errors
///
/// Returns [`GitError`] if the revision does not name a commit.
pub fn freeze_candidate(repository: &Path, revision: &str) -> Result<Candidate, GitError> {
    let commit: String = git_text(
        repository,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{commit}}"),
        ],
    )?;
    let tree: String = git_text(repository, &["rev-parse", &format!("{commit}^{{tree}}")])?;
    Ok(Candidate { commit, tree })
}

/// A clean, detached checkout of a candidate for trusted checks (T21b): nothing the agent left
/// in its own worktree is here, only what the candidate commit holds.
#[derive(Debug)]
pub struct CandidateCheckout {
    repository: std::path::PathBuf,
    path: std::path::PathBuf,
}

impl CandidateCheckout {
    /// Checks `candidate` out, detached, at `path`, which must not exist yet.
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] if git cannot create the checkout.
    pub fn create(repository: &Path, candidate: &Candidate, path: &Path) -> Result<Self, GitError> {
        let path_text: String = path.to_string_lossy().into_owned();
        run_git(
            repository,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                &path_text,
                &candidate.commit,
            ],
        )?;
        Ok(Self {
            repository: repository.to_owned(),
            path: path.to_owned(),
        })
    }

    /// Where the checkout is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for CandidateCheckout {
    fn drop(&mut self) {
        // A check checkout holds nothing to preserve: it is the candidate itself, which stays
        // in its commit. Whatever checks wrote there is scratch.
        let path_text: String = self.path.to_string_lossy().into_owned();
        let _ = run_git(
            &self.repository,
            &["worktree", "remove", "--force", &path_text],
        );
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::{Candidate, CandidateCheckout, freeze_candidate};
    use crate::test_support::{TemporaryRepository, a_repository_with_one_commit_on_main};

    #[test]
    fn a_candidate_is_the_preserved_commit_and_its_tree() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        repository.commit_file("src/auth/session.rs", "pub fn refresh() {}\n", "wip");

        let candidate: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");

        assert_eq!(
            (candidate.commit.len(), candidate.tree.len()),
            (GIT_OBJECT_ID_HEX, GIT_OBJECT_ID_HEX),
            "a candidate is pinned by full object ids, never a movable name"
        );
        assert_ne!(
            candidate.commit, candidate.tree,
            "commit and tree are distinct objects"
        );
    }

    #[test]
    fn a_changed_candidate_has_a_different_tree() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        repository.commit_file("src/auth/session.rs", "pub fn refresh() {}\n", "reviewed");
        let reviewed: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");
        repository.commit_file(
            "src/auth/session.rs",
            "pub fn refresh() { todo() }\n",
            "after",
        );

        let changed: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");

        assert_ne!(
            reviewed.tree, changed.tree,
            "any change to the files gives a new identity, so old evidence no longer applies"
        );
    }

    #[test]
    fn a_name_that_is_not_a_commit_is_not_a_candidate() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();

        let frozen = freeze_candidate(repository.path(), "kaisa/att_missing");

        assert!(
            frozen.is_err(),
            "verification never runs on something that does not exist"
        );
    }

    #[test]
    fn a_check_checkout_holds_the_candidate_and_nothing_left_in_the_agents_worktree() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        repository.commit_file("src/auth/session.rs", "pub fn refresh() {}\n", "candidate");
        std::fs::write(repository.path().join("uncommitted.txt"), "agent leftovers")
            .expect("writable");
        let candidate: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");
        let place = repository.sibling("check-41a");

        let checkout =
            CandidateCheckout::create(repository.path(), &candidate, &place).expect("checked out");
        let has_candidate_file: bool = checkout.path().join("src/auth/session.rs").exists();
        let has_leftover: bool = checkout.path().join("uncommitted.txt").exists();
        drop(checkout);

        assert!(
            has_candidate_file,
            "the checkout holds the candidate's files"
        );
        assert!(
            !has_leftover,
            "nothing outside the candidate commit reaches the checks"
        );
        assert!(
            !place.exists(),
            "the checkout is discarded once the checks are done"
        );
    }

    /// Hex digits of a SHA-1 git object id (the default object format).
    const GIT_OBJECT_ID_HEX: usize = 40;
}
