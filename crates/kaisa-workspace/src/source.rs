//! Which source state a new attempt starts from (architecture §5.5, user-flows F31).
//!
//! Attempts start from committed state by default. When the user's tree has uncommitted
//! changes, Kaisa shows them and asks; the safe default is to leave them out.

use crate::status::{EntryKind, StatusEntry};

/// What to do with uncommitted changes when creating an attempt's worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SourceChoice {
    /// Use the last commit only; uncommitted changes stay where they are.
    CommittedOnly,
    /// Snapshot the uncommitted changes into a temporary commit and start from it.
    IncludeUncommitted,
    /// Ask the user to commit first.
    CommitFirst,
}

/// Whether the user needs to be asked about uncommitted changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceDecision {
    /// Nothing is uncommitted; the attempt starts from the last commit without asking.
    Clean,
    /// Something is uncommitted; the user is shown `dirty` and offered a choice.
    NeedsChoice {
        /// The choice made if the user just confirms.
        default: SourceChoice,
        /// The uncommitted entries, to show in the preview.
        dirty: Vec<StatusEntry>,
    },
}

/// Decides from `git status` entries whether the user must choose a source state.
#[must_use]
pub fn decide_source(entries: &[StatusEntry]) -> SourceDecision {
    let dirty: Vec<StatusEntry> = entries
        .iter()
        .filter(|entry| entry.kind != EntryKind::Ignored)
        .cloned()
        .collect();
    if dirty.is_empty() {
        SourceDecision::Clean
    } else {
        SourceDecision::NeedsChoice {
            default: SourceChoice::CommittedOnly,
            dirty,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SourceChoice, SourceDecision, decide_source};
    use crate::status::{EntryKind, StatusEntry};

    fn modified(path: &str) -> StatusEntry {
        StatusEntry {
            path: path.to_owned(),
            kind: EntryKind::Tracked,
        }
    }

    fn untracked(path: &str) -> StatusEntry {
        StatusEntry {
            path: path.to_owned(),
            kind: EntryKind::Untracked,
        }
    }

    fn ignored(path: &str) -> StatusEntry {
        StatusEntry {
            path: path.to_owned(),
            kind: EntryKind::Ignored,
        }
    }

    /// A tree where the user is halfway through editing the login code.
    fn a_tree_with_unfinished_login_work() -> Vec<StatusEntry> {
        vec![
            modified("src/login.rs"),
            untracked("notes.md"),
            ignored("target/"),
        ]
    }

    #[test]
    fn a_clean_tree_needs_no_source_choice() {
        let nothing_changed: Vec<StatusEntry> = Vec::new();

        assert_eq!(
            decide_source(&nothing_changed),
            SourceDecision::Clean,
            "with nothing uncommitted there is nothing to ask (U8: no needless prompts)"
        );
    }

    #[test]
    fn a_tree_with_only_ignored_files_is_clean() {
        let only_build_output: Vec<StatusEntry> = vec![ignored("target/"), ignored(".venv/")];

        assert_eq!(
            decide_source(&only_build_output),
            SourceDecision::Clean,
            "build output is not the user's unfinished work"
        );
    }

    #[test]
    fn a_dirty_tree_defaults_to_committed_only() {
        let decision: SourceDecision = decide_source(&a_tree_with_unfinished_login_work());

        assert!(
            matches!(
                decision,
                SourceDecision::NeedsChoice {
                    default: SourceChoice::CommittedOnly,
                    ..
                }
            ),
            "the safe default never copies unfinished work without asking, got {decision:?}"
        );
    }

    #[test]
    fn the_preview_lists_every_uncommitted_entry_but_not_ignored_ones() {
        let SourceDecision::NeedsChoice { dirty, .. } =
            decide_source(&a_tree_with_unfinished_login_work())
        else {
            panic!("a tree with unfinished work needs a choice");
        };

        assert_eq!(
            dirty,
            [modified("src/login.rs"), untracked("notes.md")],
            "the user sees exactly what would be left out or copied"
        );
    }
}
