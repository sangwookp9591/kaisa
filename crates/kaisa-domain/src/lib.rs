//! Pure domain types for Kaisa: identifiers, events and errors. No I/O.
//!
//! Design: `docs/architecture.md` §5.2 and §7.

mod id;

pub use id::{AttemptId, IdError, MAX_SUFFIX_LEN, RunId, TaskId};
