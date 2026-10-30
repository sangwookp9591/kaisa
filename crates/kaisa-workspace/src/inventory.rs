//! What must be preserved before an attempt's worktree is removed (architecture §5.5, B6).
//!
//! Every change is preserved except ignored build output named by the policy. Ignored files
//! the policy does not recognise are treated as user data and preserved too, because deleting
//! them could lose work.

use crate::status::{EntryKind, StatusEntry};

/// Which ignored directories are disposable build output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredPolicy {
    build_output_directories: Vec<String>,
}

impl IgnoredPolicy {
    /// A policy treating the given directories (relative to the worktree root, without a
    /// trailing slash) as rebuildable build output.
    #[must_use]
    pub fn with_build_output(directories: &[&str]) -> Self {
        Self {
            build_output_directories: directories
                .iter()
                .map(|directory| directory.trim_end_matches('/').to_owned())
                .collect(),
        }
    }

    /// True when `path` is inside one of the build output directories.
    #[must_use]
    pub fn is_build_output(&self, path: &str) -> bool {
        self.build_output_directories.iter().any(|directory| {
            path == directory.as_str()
                || path
                    .strip_prefix(directory.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    }
}

/// The plan for one worktree: what to preserve and what may simply be deleted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inventory {
    /// Entries that must be covered by a preservation receipt before deletion.
    pub preserve: Vec<StatusEntry>,
    /// Ignored build output that can be deleted without preserving it.
    pub disposable: Vec<StatusEntry>,
}

/// Splits `git status` entries into what must be preserved and what is disposable.
#[must_use]
pub fn plan_inventory(entries: &[StatusEntry], policy: &IgnoredPolicy) -> Inventory {
    let mut inventory = Inventory::default();
    for entry in entries {
        let disposable: bool =
            entry.kind == EntryKind::Ignored && policy.is_build_output(&entry.path);
        if disposable {
            inventory.disposable.push(entry.clone());
        } else {
            inventory.preserve.push(entry.clone());
        }
    }
    inventory
}

#[cfg(test)]
mod tests {
    use super::{IgnoredPolicy, Inventory, plan_inventory};
    use crate::status::{EntryKind, StatusEntry};

    fn entry(path: &str, kind: EntryKind) -> StatusEntry {
        StatusEntry {
            path: path.to_owned(),
            kind,
        }
    }

    /// The policy of a Rust and JavaScript repository: `target` and `node_modules` rebuild.
    fn rust_and_node_build_policy() -> IgnoredPolicy {
        IgnoredPolicy::with_build_output(&["target/", "node_modules"])
    }

    fn inventory_of(entries: &[StatusEntry]) -> Inventory {
        plan_inventory(entries, &rust_and_node_build_policy())
    }

    #[test]
    fn tracked_changes_are_included_in_the_preservation_inventory() {
        let edited_login: StatusEntry = entry("src/login.rs", EntryKind::Tracked);

        assert_eq!(
            inventory_of(std::slice::from_ref(&edited_login)).preserve,
            [edited_login],
            "edits to tracked files are the attempt's work"
        );
    }

    #[test]
    fn untracked_files_are_included_in_the_preservation_inventory() {
        let new_test: StatusEntry = entry("tests/login_test.rs", EntryKind::Untracked);

        assert_eq!(
            inventory_of(std::slice::from_ref(&new_test)).preserve,
            [new_test],
            "new files are work too, even though git does not track them yet"
        );
    }

    #[test]
    fn conflicted_files_are_included_in_the_preservation_inventory() {
        let half_merged: StatusEntry = entry("src/session.rs", EntryKind::Conflicted);

        assert_eq!(
            inventory_of(std::slice::from_ref(&half_merged)).preserve,
            [half_merged],
            "an unresolved merge must never be thrown away"
        );
    }

    #[test]
    fn ignored_build_output_is_left_out_of_the_inventory() {
        let compiled: StatusEntry = entry("target/debug/kaisa", EntryKind::Ignored);

        let inventory: Inventory = inventory_of(std::slice::from_ref(&compiled));

        assert!(inventory.preserve.is_empty(), "build output can be rebuilt");
        assert_eq!(
            inventory.disposable,
            [compiled],
            "it is listed as disposable instead"
        );
    }

    #[test]
    fn ignored_files_outside_the_build_policy_are_kept_as_user_data() {
        let local_secrets_template: StatusEntry = entry(".env.local", EntryKind::Ignored);

        assert_eq!(
            inventory_of(std::slice::from_ref(&local_secrets_template)).preserve,
            [local_secrets_template],
            "an ignored file the policy does not know may be the user's data"
        );
    }

    #[test]
    fn a_path_that_only_shares_a_prefix_with_a_build_directory_is_kept() {
        let look_alike: StatusEntry = entry("targets.md", EntryKind::Ignored);

        assert_eq!(
            inventory_of(std::slice::from_ref(&look_alike)).preserve,
            [look_alike],
            "`target` must match the directory, not every name starting with those letters"
        );
    }
}
