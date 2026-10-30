//! Git worktrees, copy-on-write warm-up and the preservation protocol.
//!
//! Design: `docs/architecture.md` §5.5.

mod source;
mod status;

pub use source::{SourceChoice, SourceDecision, decide_source};
pub use status::{EntryKind, StatusEntry, StatusParseError, parse_status};
