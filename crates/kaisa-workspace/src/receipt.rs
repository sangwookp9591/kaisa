//! Preservation receipts and the check that a deletion is fully covered (architecture §5.5).
//!
//! A worktree may only be deleted when every entry that must be preserved appears in a
//! receipt that was written and verified before deletion starts.

use std::collections::BTreeSet;
use std::fmt;

use crate::inventory::Inventory;

/// Proof that the listed paths were preserved (as a WIP commit or an encrypted archive).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PreservationReceipt {
    covered: BTreeSet<String>,
}

impl PreservationReceipt {
    /// A receipt covering exactly `paths`.
    #[must_use]
    pub fn covering(paths: &[&str]) -> Self {
        Self {
            covered: paths.iter().map(|path| (*path).to_owned()).collect(),
        }
    }

    /// True when `path` is covered by this receipt.
    #[must_use]
    pub fn covers(&self, path: &str) -> bool {
        self.covered.contains(path)
    }
}

/// A deletion was refused because some entries are not preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncoveredDeletion {
    /// Paths that must be preserved but are missing from the receipt, in inventory order.
    pub uncovered: Vec<String>,
}

impl fmt::Display for UncoveredDeletion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "refusing to delete unpreserved work: {}",
            self.uncovered.join(", ")
        )
    }
}

impl std::error::Error for UncoveredDeletion {}

/// Allows deleting the worktree described by `inventory` only if `receipt` covers every entry
/// that must be preserved. Disposable build output needs no coverage.
///
/// # Errors
///
/// Returns [`UncoveredDeletion`] listing every entry the receipt does not cover.
pub fn check_deletion(
    inventory: &Inventory,
    receipt: &PreservationReceipt,
) -> Result<(), UncoveredDeletion> {
    let uncovered: Vec<String> = inventory
        .preserve
        .iter()
        .filter(|entry| !receipt.covers(&entry.path))
        .map(|entry| entry.path.clone())
        .collect();
    if uncovered.is_empty() {
        Ok(())
    } else {
        Err(UncoveredDeletion { uncovered })
    }
}

#[cfg(test)]
mod tests {
    use super::{PreservationReceipt, UncoveredDeletion, check_deletion};
    use crate::inventory::{IgnoredPolicy, Inventory, plan_inventory};
    use crate::status::{EntryKind, StatusEntry};

    fn entry(path: &str, kind: EntryKind) -> StatusEntry {
        StatusEntry {
            path: path.to_owned(),
            kind,
        }
    }

    /// The worktree of an attempt that edited the login code, added a test and built.
    fn inventory_of_the_login_fix_attempt() -> Inventory {
        plan_inventory(
            &[
                entry("src/login.rs", EntryKind::Tracked),
                entry("tests/login_test.rs", EntryKind::Untracked),
                entry("target/debug/kaisa", EntryKind::Ignored),
            ],
            &IgnoredPolicy::with_build_output(&["target"]),
        )
    }

    /// A receipt for the WIP commit that captured both pieces of the attempt's work.
    fn receipt_for_the_full_wip_commit() -> PreservationReceipt {
        PreservationReceipt::covering(&["src/login.rs", "tests/login_test.rs"])
    }

    /// A receipt from a preservation that forgot the new test file.
    fn receipt_that_forgot_the_new_test() -> PreservationReceipt {
        PreservationReceipt::covering(&["src/login.rs"])
    }

    #[test]
    fn a_deletion_covered_by_the_receipt_is_allowed() {
        assert_eq!(
            check_deletion(
                &inventory_of_the_login_fix_attempt(),
                &receipt_for_the_full_wip_commit()
            ),
            Ok(()),
            "everything worth keeping is preserved, so the worktree may go"
        );
    }

    #[test]
    fn a_deletion_not_covered_by_the_receipt_is_refused() {
        assert_eq!(
            check_deletion(
                &inventory_of_the_login_fix_attempt(),
                &receipt_that_forgot_the_new_test()
            ),
            Err(UncoveredDeletion {
                uncovered: vec!["tests/login_test.rs".to_owned()]
            }),
            "deleting would lose the new test, so the refusal names it"
        );
    }

    #[test]
    fn disposable_build_output_needs_no_receipt_coverage() {
        let receipt: PreservationReceipt = receipt_for_the_full_wip_commit();

        assert!(
            !receipt.covers("target/debug/kaisa"),
            "the receipt deliberately leaves build output out"
        );
        assert_eq!(
            check_deletion(&inventory_of_the_login_fix_attempt(), &receipt),
            Ok(()),
            "rebuildable output does not block cleanup"
        );
    }

    #[test]
    fn an_empty_worktree_can_be_deleted_with_an_empty_receipt() {
        let nothing_to_keep: Inventory = Inventory::default();

        assert_eq!(
            check_deletion(&nothing_to_keep, &PreservationReceipt::default()),
            Ok(()),
            "an attempt that changed nothing has nothing to preserve"
        );
    }
}
