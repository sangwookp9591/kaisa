//! Git worktrees, copy-on-write warm-up and the preservation protocol.
//!
//! Design: `docs/architecture.md` §5.5.

mod inventory;
mod source;
mod status;

pub use inventory::{IgnoredPolicy, Inventory, plan_inventory};
pub use source::{SourceChoice, SourceDecision, decide_source};
pub use status::{EntryKind, StatusEntry, StatusParseError, parse_status};
