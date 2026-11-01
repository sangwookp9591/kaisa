//! Parallel planned missions (T20d, ADR-0028/0029). Task gates establish dependency readiness;
//! final delivery additionally requires checks and independent review of the integrated tree.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use kaisa_context::DetectedCommand;
use kaisa_domain::{Event, EventKind, Plan, PolicySnapshot, TaskId};
use kaisa_ledger::EventStore;
use kaisa_sys::ProcessExit;
use kaisa_workspace::{Candidate, freeze_candidate};
use serde_json::json;

use crate::gate::Delivery;
use crate::goal::{
    GoalError, GoalOutcome, GoalRequest, ParallelAgentLauncher, at, check_cancellation, run_goal,
};
use crate::ids::{fresh_suffix, now_ms};
use crate::integrate::{IntegrationError, check_write_set, merge_commits};
use crate::scheduler::Scheduler;
use crate::team::{VerifiedOutcome, VerifiedRequest, run_verified_task, verify_integration};

/// Inputs and trusted authorities for one mission.
pub struct MissionRequest<'request> {
    /// Repository holding the preserved result refs.
    pub repository: &'request Path,
    /// Kaisa's private ledger and attempt directories.
    pub state_directory: &'request Path,
    /// Validated DAG and write sets.
    pub plan: &'request Plan,
    /// Self-contained instructions indexed by task identity.
    pub instructions: &'request BTreeMap<TaskId, String>,
    /// Task developer launcher.
    pub launch: ParallelAgentLauncher<'request>,
    /// Independent read-only reviewer launcher.
    pub reviewer: ParallelAgentLauncher<'request>,
    /// Checks from the trusted original base profile or explicit human argv.
    pub checks: &'request [DetectedCommand],
    /// Immutable concurrency, failure and verification policy.
    pub policy: &'request PolicySnapshot,
    /// Executable used by each attempt's bridge.
    pub kaisa_executable: &'request Path,
    /// Whether agents run confined.
    pub confine_agent: bool,
    /// Whether developer writes are observed and checked afterwards.
    pub observed: bool,
    /// Allowed broker egress.
    pub egress_hosts: &'request [&'request str],
    /// Agent session-state directories outside its worktree.
    pub agent_writable: &'request [std::path::PathBuf],
    /// Admission basis for reduced containment.
    pub reduced_mode: &'request kaisa_domain::ReducedModeBasis,
}

/// A mission never delivers a partial integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionOutcome {
    /// Developer outcomes in deterministic admission order.
    pub ran: Vec<(TaskId, GoalOutcome)>,
    /// First failing task in admission order.
    pub stopped_at: Option<TaskId>,
    /// Tasks blocked by failure or stop policy.
    pub blocked: Vec<TaskId>,
    /// Typed blocking integration failure with affected files.
    pub conflict: Option<IntegrationError>,
    /// Final integrated verification, only present after every task succeeds.
    pub verification: Option<VerifiedOutcome>,
}

pub(crate) fn record_progress(
    state: &Path,
    kind: EventKind,
    subject: &str,
    mut detail: serde_json::Value,
) -> Result<(), GoalError> {
    detail["at_ms"] = json!(now_ms());
    EventStore::open(&state.join("kaisa.db"))
        .and_then(|mut ledger| {
            ledger.append(Event {
                kind,
                subject: subject.to_owned(),
                payload_version: 1,
                payload: detail.to_string(),
            })
        })
        .map(|_| ())
        .map_err(at("recording mission progress"))
}

fn verified_request<'a>(
    request: &'a MissionRequest<'a>,
    goal: &'a str,
    write_set: &'a kaisa_domain::WriteSet,
) -> VerifiedRequest<'a> {
    VerifiedRequest {
        repository: request.repository,
        state_directory: request.state_directory,
        goal,
        write_set,
        developer: request.launch,
        reviewer: request.reviewer,
        kaisa_executable: request.kaisa_executable,
        confine_agent: request.confine_agent,
        checks: request.checks,
        policy: request.policy,
        observed: request.observed,
        first_pass: None,
        egress_hosts: request.egress_hosts,
        agent_writable: request.agent_writable,
        reduced_mode: request.reduced_mode,
    }
}

struct TaskResult {
    attempt: Option<GoalOutcome>,
    candidate: Option<Candidate>,
    succeeded: bool,
    conflict: Option<IntegrationError>,
    eligible_gate: Option<Event>,
}

/// Runs deterministic concurrent DAG waves and verifies their final integration.
/// Admitted siblings drain on failure, retaining existing process-tree cleanup and receipts.
///
/// # Errors
/// Returns an infrastructure error after all admitted siblings have joined; agent/check/review
/// failures and merge conflicts are blocking outcomes rather than successful delivery.
pub fn run_mission(request: &MissionRequest<'_>) -> Result<MissionOutcome, GoalError> {
    kaisa_sys::ensure_private_directory(request.state_directory)
        .map_err(at("preparing the state directory"))?;
    if request.plan.tasks().is_empty() {
        return Err(GoalError {
            step: "mission admission",
            detail: "a mission needs at least one task".into(),
        });
    }
    for task in request.plan.tasks() {
        if !request
            .instructions
            .get(&task.id)
            .is_some_and(|instruction| !instruction.trim().is_empty())
        {
            return Err(GoalError {
                step: "mission admission",
                detail: format!("task {} needs a nonempty instruction", task.id),
            });
        }
    }
    let policy = request.policy.policy();
    let mut scheduler = Scheduler::new(request.plan, policy.max_parallel, policy.on_failure)
        .map_err(at("scheduling"))?;
    let original =
        freeze_candidate(request.repository, "HEAD").map_err(at("resolving mission base"))?;
    let mission = format!("mis_{}", fresh_suffix());
    let reference = format!("refs/kaisa/integration/{mission}");
    record_progress(
        request.state_directory,
        EventKind::MissionCreated,
        &mission,
        json!({"goal":"planned mission", "base":original.commit}),
    )?;
    EventStore::open(&request.state_directory.join("kaisa.db"))
        .and_then(|mut ledger| ledger.record_policy(request.policy))
        .map_err(at("recording mission policy"))?;
    let mut commits: BTreeMap<TaskId, String> = BTreeMap::new();
    let mut settled = BTreeSet::new();
    let mut outcome = MissionOutcome {
        ran: vec![],
        stopped_at: None,
        blocked: vec![],
        conflict: None,
        verification: None,
    };
    loop {
        check_cancellation()?;
        let wave = scheduler.next_wave().map_err(at("scheduling"))?;
        if wave.is_empty() {
            break;
        }
        // Prepare bases and record starts on the owning thread, in plan order. No completion
        // race may reorder admission. All parents come from successful, preserved task gates.
        let mut prepared = Vec::new();
        for task in wave {
            if check_cancellation().is_err() {
                break;
            }
            let parents: Vec<_> = task
                .depends_on
                .iter()
                .map(|id| commits[id].clone())
                .collect();
            let base = match merge_commits(
                request.repository,
                &original.commit,
                &parents,
                &format!("{reference}/{}", task.id),
            ) {
                Ok(candidate) => candidate.commit,
                Err(conflict) => {
                    record_progress(
                        request.state_directory,
                        EventKind::IntegrationConflict,
                        task.id.as_str(),
                        json!({"mission_id":mission,"files":conflict.files(), "reason":conflict.to_string(),"step":"dependency base"}),
                    )?;
                    outcome.stopped_at.get_or_insert(task.id.clone());
                    outcome.conflict = Some(conflict);
                    scheduler.finish(task.id.clone(), false);
                    settled.insert(task.id);
                    continue;
                }
            };
            record_progress(
                request.state_directory,
                EventKind::TaskScheduled,
                task.id.as_str(),
                json!({"mission_id":mission,"base":base,"write_set":task.write_set.entries().iter().map(|path| path.as_str()).collect::<Vec<_>>() }),
            )?;
            record_progress(
                request.state_directory,
                EventKind::TaskStarted,
                task.id.as_str(),
                json!({"mission_id":mission,"base":base}),
            )?;
            prepared.push((task, base));
        }
        let cancellation = kaisa_sys::Cancellation::current().unwrap_or_default();
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = prepared
                .iter()
                .map(|(task, base)| {
                    let cancellation = &cancellation;
                    let mission = &mission;
                    scope.spawn(move || cancellation.scope(|| {
                        let start = Instant::now();
                        let goal = request
                            .instructions
                            .get(&task.id)
                            .map_or("", String::as_str);
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let (attempt, candidate, passed, eligible_gate) = if policy.verify_each_task {
                                let (result, eligible_gate) = run_verified_task(
                                    &verified_request(request, goal, &task.write_set),
                                    base,
                                    task.role,
                                )?;
                                let attempt = result
                                    .attempts
                                    .iter()
                                    .rev()
                                    .find(|attempt| attempt.commit.is_some())
                                    .cloned();
                                (
                                    attempt,
                                    result.candidate,
                                    matches!(result.delivery, Delivery::Deliver { .. }),
                                    eligible_gate,
                                )
                            } else {
                                let attempt = run_goal(&GoalRequest {
                                    repository: request.repository,
                                    goal,
                                    base,
                                    state_directory: request.state_directory,
                                    write_set: &task.write_set,
                                    role: task.role,
                                    launch: request.launch,
                                    kaisa_executable: request.kaisa_executable,
                                    confine_agent: request.confine_agent && policy.confine_agents,
                                    egress_hosts: request.egress_hosts,
                                    agent_writable: request.agent_writable,
                                    local_model_port: None,
                                    observed: request.observed,
                                    reduced_mode: request.reduced_mode,
                                })?;
                                let candidate = attempt
                                    .commit
                                    .as_deref()
                                    .map(|commit| freeze_candidate(request.repository, commit))
                                    .transpose()
                                    .map_err(at("freezing task result"))?;
                                let passed = attempt.exit == Some(ProcessExit::Code(0))
                                    && attempt.kept_back.is_empty();
                                (Some(attempt), candidate, passed, None)
                            };
                            let conflict = candidate.as_ref().and_then(|candidate| {
                                check_write_set(
                                    request.repository,
                                    base,
                                    candidate,
                                    &task.write_set,
                                )
                                .err()
                            });
                            Ok::<_, GoalError>(TaskResult {
                                attempt,
                                candidate,
                                succeeded: passed && conflict.is_none(),
                                conflict,
                                eligible_gate,
                            })
                        })).unwrap_or_else(|_| Err(GoalError {
                            step: "task worker",
                            detail: "worker panicked; admitted siblings drained".into(),
                        }));
                        let succeeded = result.as_ref().is_ok_and(|result| result.succeeded);
                        record_progress(
                            request.state_directory,
                            EventKind::TaskFinished,
                            task.id.as_str(),
                            json!({"mission_id":mission,"succeeded":succeeded,"elapsed_ms":start.elapsed().as_millis(),"base":base,"error":result.as_ref().err().map(ToString::to_string)}),
                        )?;
                        if succeeded
                            && let Some(gate) = result.as_ref().ok().and_then(|result| result.eligible_gate.as_ref())
                        {
                            EventStore::open(&request.state_directory.join("kaisa.db"))
                                .and_then(|mut ledger| ledger.append(gate.clone()))
                                .map_err(at("recording settled task eligibility"))?;
                        }
                        result
                    }))
                })
                .collect();
            // Never return while a sibling owns a process, even when another worker errors.
            handles
                .into_iter()
                .map(|handle| {
                    handle.join().unwrap_or_else(|_| {
                        Err(GoalError {
                            step: "task worker",
                            detail: "worker panicked; admitted siblings drained".into(),
                        })
                    })
                })
                .collect::<Vec<_>>()
        });
        let mut infrastructure_error = None;
        for ((task, base), result) in prepared.into_iter().zip(results) {
            let succeeded = result.as_ref().is_ok_and(|result| result.succeeded);
            scheduler.finish(task.id.clone(), succeeded);
            settled.insert(task.id.clone());
            if !succeeded {
                outcome.stopped_at.get_or_insert(task.id.clone());
            }
            match result {
                Ok(result) => {
                    if let Some(conflict) = result.conflict {
                        record_progress(
                            request.state_directory,
                            EventKind::IntegrationConflict,
                            task.id.as_str(),
                            json!({"mission_id":mission,"files":conflict.files(),"reason":conflict.to_string(),"step":"task ownership"}),
                        )?;
                        outcome.conflict = Some(conflict);
                    }
                    if let Some(attempt) = result.attempt {
                        outcome.ran.push((task.id.clone(), attempt));
                    }
                    if succeeded {
                        commits.insert(
                            task.id,
                            result.candidate.map_or(base, |candidate| candidate.commit),
                        );
                    }
                }
                Err(mut error) => {
                    error.detail = format!("task {}: {}", task.id, error.detail);
                    infrastructure_error.get_or_insert(error);
                }
            }
        }
        check_cancellation()?;
        if let Some(error) = infrastructure_error {
            return Err(error);
        }
    }
    outcome.blocked = request
        .plan
        .tasks()
        .iter()
        .filter(|task| !settled.contains(&task.id))
        .map(|task| task.id.clone())
        .collect();
    if outcome.stopped_at.is_some() {
        for task in &outcome.blocked {
            record_progress(
                request.state_directory,
                EventKind::TaskFinished,
                task.as_str(),
                json!({"mission_id":mission,"succeeded":false,"scheduled":false,"reason":"blocked by failed dependency or stop policy"}),
            )?;
        }
        return Ok(outcome);
    }
    // Deterministic topological order: tasks were admitted only after every parent settled.
    let ordered: Vec<_> = outcome
        .ran
        .iter()
        .filter_map(|(id, _)| commits.get(id).cloned())
        .collect();
    let integrated = match merge_commits(
        request.repository,
        &original.commit,
        &ordered,
        &format!("{reference}/result"),
    ) {
        Ok(candidate) => candidate,
        Err(conflict) => {
            record_progress(
                request.state_directory,
                EventKind::IntegrationConflict,
                &mission,
                json!({"files":conflict.files(),"reason":conflict.to_string(),"step":"final integration"}),
            )?;
            outcome.conflict = Some(conflict);
            return Ok(outcome);
        }
    };
    for (task, _) in &outcome.ran {
        record_progress(
            request.state_directory,
            EventKind::TaskIntegrated,
            task.as_str(),
            json!({"mission_id":mission,"candidate_tree":integrated.tree,"commit":commits[task]}),
        )?;
    }
    let all_paths = kaisa_domain::WriteSet::new(
        request
            .plan
            .tasks()
            .iter()
            .flat_map(|task| task.write_set.entries().to_vec())
            .collect(),
    );
    let goals = request
        .plan
        .tasks()
        .iter()
        .map(|task| {
            request
                .instructions
                .get(&task.id)
                .map_or("", String::as_str)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let start = Instant::now();
    let verified = verify_integration(
        &verified_request(request, &goals, &all_paths),
        &original.commit,
        &integrated,
    )
    .map_err(|mut error| {
        error.detail = format!("integrated verification: {}", error.detail);
        error
    })?;
    check_cancellation()?;
    if matches!(verified.delivery, Delivery::Deliver { .. }) {
        record_progress(
            request.state_directory,
            EventKind::MissionVerified,
            &mission,
            json!({"candidate_tree":integrated.tree,"commit":integrated.commit,"elapsed_ms":start.elapsed().as_millis(),"verify_each_task":policy.verify_each_task}),
        )?;
    }
    outcome.verification = Some(verified);
    Ok(outcome)
}
