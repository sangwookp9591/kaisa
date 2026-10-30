//! Git worktrees, copy-on-write warm-up and the preservation protocol.
//!
//! Design: `docs/architecture.md` §5.5.

mod inventory;
mod receipt;
mod source;
mod status;

pub use inventory::{IgnoredPolicy, Inventory, plan_inventory};
pub use receipt::{PreservationReceipt, UncoveredDeletion, check_deletion};
pub use source::{SourceChoice, SourceDecision, decide_source};
pub use status::{EntryKind, StatusEntry, StatusParseError, parse_status};
