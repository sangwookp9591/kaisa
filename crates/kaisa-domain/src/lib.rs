//! Pure domain types for Kaisa: identifiers, events and errors. No I/O.
//!
//! Design: `docs/architecture.md` §5.2, §5.4 and §7.

mod attempt;
mod attempt_codes;
mod authority;
mod containment;
mod effect;
mod event;
mod grade;
mod id;
mod knowledge;
mod plan;
mod repo_path;
mod sandbox;
mod write_set;

pub use attempt::{
    AttemptState, Axis, Cleanup, Execution, ExitStatus, FailureReason, IllegalTransition, Outcome,
    Verification,
};
pub use authority::{Authority, Operation};
pub use containment::{Admission, ContainmentCapability, ReducedModeDecision, admit};
pub use effect::EffectKind;
pub use event::{Event, EventKind, RecordedEvent, Sequence};
pub use grade::{BelowFloor, Grade, GradeFloor};
pub use id::{AttemptId, IdError, IntentId, MAX_SUFFIX_LEN, MissionId, RunId, TaskId};
pub use knowledge::{ContentDigest, DIGEST_HEX_LEN, DigestError, KnowledgeKind};
pub use plan::{Plan, PlanError, PlannedTask};
pub use repo_path::{RepoPath, RepoPathError};
pub use sandbox::{Network, Role, SandboxProfile, sandbox_profile};
pub use write_set::WriteSet;
