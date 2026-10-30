//! The candidate a verification looks at: an attempt's preserved commit, frozen (T21a,
//! ADR-0015). Its tree hash is the identity every piece of evidence and every review binds to;
//! git derives it from the content, so a changed candidate can never pass for the reviewed one.

use std::path::Path;

use crate::git::{GitError, git_text};

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

#[cfg(test)]
mod tests {
    use super::{Candidate, freeze_candidate};
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

    /// Hex digits of a SHA-1 git object id (the default object format).
    const GIT_OBJECT_ID_HEX: usize = 40;
}
