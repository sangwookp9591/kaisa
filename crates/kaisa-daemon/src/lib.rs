//! Kaisa daemon composition root: the only place concrete adapters are wired.
//!
//! Design: `docs/architecture.md`. In M0 a goal runs in one process (`run_goal`); the long-lived
//! daemon, its control endpoint and keeper reconnection wrap the same steps later.

mod gate;
mod goal;
mod ids;
mod learn;
mod mission;
mod recall;
mod recover;
mod review;
mod revision;
mod route;
mod sandbox;
mod team;
mod verify;

pub use gate::{Delivery, RevisionReason, decide_delivery};
pub use goal::{
    AGENT_IDENTITY_FILE, AgentLauncher, GoalError, GoalOutcome, GoalRequest, run_goal,
    whole_repository,
};
pub use learn::learn_from_delivery;
pub use mission::{MissionOutcome, MissionRequest, run_mission};
pub use recall::{MAX_LESSONS_PER_PACK, relevant_lessons};
pub use recover::{Recovered, recover_abandoned};
pub use review::{VerdictError, parse_verdict};
pub use revision::{DEFAULT_REVISION_LIMIT, RevisionBudget, revision_goal};
pub use route::{ROUTE_EFFORT, RouteDecision, decide_route, record_route};
pub use team::{CANDIDATE_TREE_LABEL, VerifiedOutcome, VerifiedRequest, run_verified_goal};
pub use verify::{Evidence, run_trusted_checks};
