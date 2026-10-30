//! Pure domain types for Kaisa: identifiers, events and errors. No I/O.
//!
//! Design: `docs/architecture.md` §5.2, §5.4 and §7.

mod attempt;
mod effect;
mod event;
mod id;
mod repo_path;

pub use attempt::{
    AttemptState, Axis, Cleanup, Execution, ExitStatus, FailureReason, IllegalTransition, Outcome,
    Verification,
};
pub use effect::EffectKind;
pub use event::{Event, EventKind, RecordedEvent, Sequence};
pub use id::{AttemptId, IdError, IntentId, MAX_SUFFIX_LEN, MissionId, RunId, TaskId};
pub use repo_path::{RepoPath, RepoPathError};
