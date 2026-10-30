//! Kaisa daemon composition root: the only place concrete adapters are wired.
//!
//! Design: `docs/architecture.md`. In M0 a goal runs in one process (`run_goal`); the long-lived
//! daemon, its control endpoint and keeper reconnection wrap the same steps later.

mod goal;
mod ids;

pub use goal::{AgentLauncher, GoalError, GoalOutcome, GoalRequest, run_goal, whole_repository};
