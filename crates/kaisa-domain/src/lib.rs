//! Pure domain types for Kaisa: identifiers, events and errors. No I/O.
//!
//! Design: `docs/architecture.md` §5.2, §5.4 and §7.

mod attempt;
mod event;
mod id;

pub use attempt::{
    AttemptState, Axis, Cleanup, Execution, ExitStatus, FailureReason, IllegalTransition, Outcome,
    Verification,
};
pub use event::{Event, EventKind, RecordedEvent, Sequence};
pub use id::{AttemptId, IdError, MAX_SUFFIX_LEN, RunId, TaskId};
