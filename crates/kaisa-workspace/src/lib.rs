//! Git worktrees, copy-on-write warm-up and the preservation protocol.
//!
//! Design: `docs/architecture.md` §5.5.

mod status;

pub use status::{EntryKind, StatusEntry, StatusParseError, parse_status};
