//! Running a planned mission: each task becomes an attempt, in the order the plan and the
//! ledger allow (T20d, ADR-0028). M1's first pass runs ready tasks one at a time; the plan
//! already guarantees that tasks which may run together own disjoint files.

use std::collections::BTreeMap;
use std::path::Path;

use kaisa_domain::{Event, EventKind, Plan, PlannedTask, TaskId};
use kaisa_ledger::{EventStore, TaskProgress};
use kaisa_sys::ProcessExit;
use serde_json::json;

use crate::goal::{AgentLauncher, GoalError, GoalOutcome, GoalRequest, at, run_goal};

/// Payload schema version of the task events this module writes.
const PAYLOAD_VERSION: u16 = 1;

/// What to run.
pub struct MissionRequest<'request> {
    /// The repository the mission is about.
    pub repository: &'request Path,
    /// Kaisa's state directory.
    pub state_directory: &'request Path,
    /// The validated plan.
    pub plan: &'request Plan,
    /// What each task is asked to do.
    pub instructions: &'request BTreeMap<TaskId, String>,
    /// How to start each task's agent.
    pub launch: AgentLauncher<'request>,
    /// The `kaisa` executable agents' bridges run.
    pub kaisa_executable: &'request Path,
    /// Whether agents run confined.
    pub confine_agent: bool,
}

/// How a mission ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionOutcome {
    /// Each task that ran, in the order it ran, with its attempt's outcome.
    pub ran: Vec<(TaskId, GoalOutcome)>,
    /// The task whose agent failed and stopped the mission, if any.
    pub stopped_at: Option<TaskId>,
}

fn record_task(
    state_directory: &Path,
    kind: EventKind,
    task: &TaskId,
    detail: serde_json::Value,
) -> Result<(), GoalError> {
    let mut ledger: EventStore =
        EventStore::open(&state_directory.join("kaisa.db")).map_err(at("opening the ledger"))?;
    ledger
        .append(Event {
            kind,
            subject: task.as_str().to_owned(),
            payload_version: PAYLOAD_VERSION,
            payload: detail.to_string(),
        })
        .map(|_| ())
        .map_err(at("recording task progress"))
}

/// The commit a task starts from: the preserved work of the last dependency that left any,
/// else `HEAD`. With one dependency, which is M1's pipelines, that is exactly its result.
fn base_for(task: &PlannedTask, commits: &BTreeMap<TaskId, String>) -> String {
    task.depends_on
        .iter()
        .rev()
        .find_map(|dependency| commits.get(dependency).cloned())
        .unwrap_or_else(|| "HEAD".to_owned())
}

/// Runs `request`'s plan until every task is finished or an agent fails.
///
/// # Errors
///
/// Returns [`GoalError`] if a step fails, or if tasks remain that can never become ready.
pub fn run_mission(request: &MissionRequest<'_>) -> Result<MissionOutcome, GoalError> {
    kaisa_sys::ensure_private_directory(request.state_directory)
        .map_err(at("preparing the state directory"))?;
    let mut commits: BTreeMap<TaskId, String> = BTreeMap::new();
    let mut ran: Vec<(TaskId, GoalOutcome)> = Vec::new();
    loop {
        let progress: TaskProgress = EventStore::open(&request.state_directory.join("kaisa.db"))
            .and_then(|ledger| ledger.task_progress())
            .map_err(at("reading task progress"))?;
        let all_done: bool = request
            .plan
            .tasks()
            .iter()
            .all(|task| progress.finished.contains(&task.id));
        if all_done {
            return Ok(MissionOutcome {
                ran,
                stopped_at: None,
            });
        }
        let Some(task) = request
            .plan
            .next_runnable(&progress.finished, &progress.running)
            .first()
            .copied()
        else {
            return Err(GoalError {
                step: "scheduling",
                detail: "tasks remain but none can start".to_owned(),
            });
        };
        let instruction: &str = request
            .instructions
            .get(&task.id)
            .map_or("", String::as_str);
        let base: String = base_for(task, &commits);
        record_task(
            request.state_directory,
            EventKind::TaskStarted,
            &task.id,
            json!({"base": base}),
        )?;
        let outcome: GoalOutcome = run_goal(&GoalRequest {
            repository: request.repository,
            goal: instruction,
            base: &base,
            state_directory: request.state_directory,
            write_set: &task.write_set,
            role: task.role,
            launch: request.launch,
            kaisa_executable: request.kaisa_executable,
            confine_agent: request.confine_agent,
            egress_hosts: &[],
            agent_writable: &[],
            local_model_port: None,
            observed: false,
        })?;
        let succeeded: bool =
            outcome.exit == Some(ProcessExit::Code(0)) && outcome.kept_back.is_empty();
        if let Some(commit) = &outcome.commit {
            commits.insert(task.id.clone(), commit.clone());
        }
        ran.push((task.id.clone(), outcome));
        if !succeeded {
            return Ok(MissionOutcome {
                ran,
                stopped_at: Some(task.id.clone()),
            });
        }
        record_task(
            request.state_directory,
            EventKind::TaskFinished,
            &task.id,
            json!({}),
        )?;
    }
}
