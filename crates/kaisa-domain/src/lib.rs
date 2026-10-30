//! Pure domain types for Kaisa: identifiers, events and errors. No I/O.
//!
//! Design: `docs/architecture.md` §5.2, §5.4 and §7.

mod event;
mod id;

pub use event::{Event, EventKind, RecordedEvent, Sequence};
pub use id::{AttemptId, IdError, MAX_SUFFIX_LEN, RunId, TaskId};
