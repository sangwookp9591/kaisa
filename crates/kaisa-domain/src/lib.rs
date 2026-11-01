//! Pure domain types for Kaisa: identifiers, events and errors. No I/O.
//!
//! Design: `docs/architecture.md` §5.2, §5.4 and §7.

mod attempt;
mod attempt_codes;
mod authority;
mod catalog;
mod containment;
mod effect;
mod event;
mod grade;
mod grader;
mod id;
mod knowledge;
mod plan;
mod policy;
mod repo_path;
mod sandbox;
mod verdict;
mod write_set;

pub use attempt::{
    AttemptState, Axis, Cleanup, Execution, ExitStatus, FailureReason, IllegalTransition, Outcome,
    Verification,
};
pub use authority::{Authority, Operation};
pub use catalog::{AgentEntry, AgentKind, Catalog, CatalogError, CatalogModel};
pub use containment::{
    Admission, ContainmentAgentKind, ContainmentCapability, DefaultReducedPolicy,
    LANDLOCK_NETWORK_ABI, LANDLOCK_TRUNCATE_ABI, ReducedModeBasis, ReducedModeDecision, admit,
    admit_agent_on, admit_on, codes, landlock_missing, platform_limits,
};
pub use effect::EffectKind;
pub use event::{Event, EventKind, RecordedEvent, Sequence};
pub use grade::{BelowFloor, Grade, GradeFloor};
pub use grader::{
    GradeDecision, GradeReason, IMPACTED_FILES_THRESHOLD, REVISION_FAILURES_THRESHOLD, TaskSignals,
    WRITE_SET_PATHS_THRESHOLD, grade_task,
};
pub use id::{AttemptId, IdError, IntentId, MAX_SUFFIX_LEN, MissionId, RunId, TaskId};
pub use knowledge::{ContentDigest, DIGEST_HEX_LEN, DigestError, KnowledgeKind};
pub use plan::{Plan, PlanError, PlannedTask};
pub use policy::{OnFailure, Policy, PolicyError, PolicySnapshot, default_max_parallel};
pub use repo_path::{RepoPath, RepoPathError};
pub use sandbox::{Network, Role, SandboxProfile, sandbox_profile};
pub use verdict::{Finding, ReviewVerdict, Severity};
pub use write_set::WriteSet;
