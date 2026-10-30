//! Git worktrees, copy-on-write warm-up and the preservation protocol.
//!
//! Design: `docs/architecture.md` §5.5.

mod git;
mod inventory;
mod receipt;
mod source;
mod status;
#[cfg(test)]
mod test_support;
mod worktree;

pub use git::GitError;
pub use inventory::{IgnoredPolicy, Inventory, plan_inventory};
pub use receipt::{PreservationReceipt, UncoveredDeletion, check_deletion};
pub use source::{SourceChoice, SourceDecision, decide_source};
pub use status::{EntryKind, StatusEntry, StatusParseError, parse_status};
pub use worktree::{Worktree, WorktreeError, create_worktree, inventory};
