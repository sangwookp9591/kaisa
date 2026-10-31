//! Git worktrees, copy-on-write warm-up and the preservation protocol.
//!
//! Design: `docs/architecture.md` §5.5.

mod candidate;
mod git;
mod inventory;
mod preserve;
mod receipt;
mod secrets;
mod settings_risk;
mod source;
mod status;
#[cfg(test)]
mod test_support;
mod worktree;

pub use candidate::{
    Candidate, CandidateCheckout, changed_check_definitions, changed_paths, freeze_candidate,
    weakened_tests,
};
pub use git::{GitError, git_cli_path};
pub use inventory::{IgnoredPolicy, Inventory, plan_inventory};
pub use preserve::{Preservation, preserve_as_commit};
pub use receipt::{PreservationReceipt, UncoveredDeletion, check_deletion};
pub use secrets::{SecretKind, scan as scan_for_secrets};
pub use settings_risk::{
    MAX_RISK_EXCERPT_BYTES, MAX_SETTINGS_FILE_BYTES, RiskFinding, RiskRule, scan_settings,
};
pub use source::{SourceChoice, SourceDecision, decide_source};
pub use status::{EntryKind, StatusEntry, StatusParseError, parse_status};
pub use worktree::{
    Worktree, WorktreeError, create_worktree, inventory, open_worktree, remove_worktree,
};
