//! A verified goal (T31, ADR-0030): a developer attempt, trusted checks on its frozen candidate,
//! a reviewer attempt on the same candidate, the delivery gate, and bounded revision. Nothing
//! reaches the user unless the gate says so.

use std::path::Path;

use kaisa_context::DetectedCommand;
use kaisa_domain::{ReviewVerdict, Role, WriteSet};
use kaisa_sys::ProcessExit;
use kaisa_workspace::{Candidate, freeze_candidate};

use crate::gate::{Delivery, decide_delivery};
use crate::goal::{AgentLauncher, GoalError, GoalOutcome, GoalRequest, at, run_goal};
use crate::review::parse_verdict;
use crate::revision::{RevisionBudget, revision_goal};
use crate::verify::{Evidence, run_trusted_checks};

/// The line of a review pack that names the candidate's tree.
pub const CANDIDATE_TREE_LABEL: &str = "candidate_tree: ";

/// What to run.
pub struct VerifiedRequest<'request> {
    /// The repository.
    pub repository: &'request Path,
    /// Kaisa's state directory.
    pub state_directory: &'request Path,
    /// What the user asked for.
    pub goal: &'request str,
    /// The paths the developer may change.
    pub write_set: &'request WriteSet,
    /// How to start the developer agent.
    pub developer: AgentLauncher<'request>,
    /// How to start the reviewer agent.
    pub reviewer: AgentLauncher<'request>,
    /// The `kaisa` executable agents' bridges run.
    pub kaisa_executable: &'request Path,
    /// Whether these agents can run confined; they do only when the policy also asks it.
    pub confine_agent: bool,
    /// The project's trusted test commands, taken from the base revision's profile.
    pub checks: &'request [DetectedCommand],
    /// The policy this run adopts: its revision limit and confinement govern the run.
    pub policy: &'request kaisa_domain::PolicySnapshot,
}

/// How a verified goal ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedOutcome {
    /// The gate's decision on the last candidate.
    pub delivery: Delivery,
    /// The last candidate, if the developer produced one.
    pub candidate: Option<Candidate>,
    /// Every attempt, developer and reviewer, in order.
    pub attempts: Vec<GoalOutcome>,
    /// Revisions that ran.
    pub revisions: u32,
}

fn review_instruction(goal: &str, candidate: &Candidate) -> String {
    format!(
        "Review the candidate below for the goal it was made for. Report blocking problems only \
         when the goal is not met or the change is unsafe. Answer with one JSON line, last: \
         {{\"candidate_tree\": \"...\", \"approve\": true|false, \"findings\": [{{\"severity\": \
         \"blocking\"|\"advisory\", \"path\": \"...\", \"line\": n|null, \"message\": \"...\"}}]}}\n\n\
         Goal: {goal}\n{CANDIDATE_TREE_LABEL}{}\n",
        candidate.tree
    )
}

/// The verdict on the last non-empty line of a reviewer's transcript, if it validates.
fn verdict_from(state_directory: &Path, review: &GoalOutcome, tree: &str) -> Option<ReviewVerdict> {
    let transcript: String = std::fs::read_to_string(
        state_directory
            .join("attempts")
            .join(review.attempt.as_str())
            .join("transcript.log"),
    )
    .ok()?;
    let last: &str = transcript
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())?;
    parse_verdict(last.trim(), tree).ok()
}

/// Runs `request` until the gate delivers, refuses, needs a person, or revisions run out.
///
/// # Errors
///
/// Returns [`GoalError`] if a step cannot run; a failing agent or check is an outcome.
pub fn run_verified_goal(request: &VerifiedRequest<'_>) -> Result<VerifiedOutcome, GoalError> {
    // An agent loads these settings when it starts, so they are judged before any agent runs.
    let risks = kaisa_workspace::scan_settings(request.repository)
        .map_err(at("scanning the repository's agent settings"))?;
    if !risks.is_empty() {
        let named: Vec<String> = risks
            .iter()
            .map(|risk| format!("{} ({:?})", risk.path, risk.rule))
            .collect();
        return Ok(VerifiedOutcome {
            delivery: Delivery::NeedsPerson(format!(
                "the repository's agent settings widen what an agent may do: {}",
                named.join(", ")
            )),
            candidate: None,
            attempts: Vec::new(),
            revisions: 0,
        });
    }
    std::fs::create_dir_all(request.state_directory)
        .map_err(at("preparing the state directory"))?;
    kaisa_ledger::EventStore::open(&request.state_directory.join("kaisa.db"))
        .and_then(|mut ledger| ledger.record_policy(request.policy))
        .map_err(at("recording the run's policy"))?;
    let original_base: Candidate =
        freeze_candidate(request.repository, "HEAD").map_err(at("resolving the base"))?;
    let mut budget = RevisionBudget::new(request.policy.policy().revision_limit);
    let mut base: String = original_base.commit.clone();
    let mut instruction: String = request.goal.to_owned();
    let mut attempts: Vec<GoalOutcome> = Vec::new();
    let no_paths = WriteSet::default();
    loop {
        let developed: GoalOutcome = run_goal(&GoalRequest {
            repository: request.repository,
            goal: &instruction,
            base: &base,
            state_directory: request.state_directory,
            write_set: request.write_set,
            role: Role::Developer,
            launch: request.developer,
            kaisa_executable: request.kaisa_executable,
            confine_agent: request.confine_agent && request.policy.policy().confine_agents,
            egress_hosts: &[],
            agent_writable: &[],
            local_model_port: None,
        })?;
        let commit: Option<String> = developed.commit.clone();
        let agent_ok: bool = developed.exit == Some(ProcessExit::Code(0));
        attempts.push(developed);
        let revisions: u32 = budget.used();
        let finish = |delivery: Delivery, candidate: Option<Candidate>, attempts| VerifiedOutcome {
            delivery,
            candidate,
            attempts,
            revisions,
        };
        let Some(commit) = commit.filter(|_| agent_ok) else {
            return Ok(finish(
                Delivery::Refuse("the developer agent produced no successful change".to_owned()),
                None,
                attempts,
            ));
        };
        let candidate: Candidate =
            freeze_candidate(request.repository, &commit).map_err(at("freezing the candidate"))?;
        let evidence: Vec<Evidence> = run_trusted_checks(
            request.repository,
            request.state_directory,
            &original_base.commit,
            &candidate,
            request.checks,
        )?;
        let reviewed: GoalOutcome = run_goal(&GoalRequest {
            repository: request.repository,
            goal: &review_instruction(request.goal, &candidate),
            base: &candidate.commit,
            state_directory: request.state_directory,
            write_set: &no_paths,
            role: Role::Reviewer,
            launch: request.reviewer,
            kaisa_executable: request.kaisa_executable,
            confine_agent: request.confine_agent && request.policy.policy().confine_agents,
            egress_hosts: &[],
            agent_writable: &[],
            local_model_port: None,
        })?;
        let verdict: Option<ReviewVerdict> =
            verdict_from(request.state_directory, &reviewed, &candidate.tree);
        attempts.push(reviewed);
        let delivery: Delivery = decide_delivery(&candidate, &evidence, verdict.as_ref());
        match delivery {
            Delivery::Revise(reasons) if budget.take() => {
                base = candidate.commit.clone();
                instruction = revision_goal(request.goal, &reasons);
            }
            other => return Ok(finish(other, Some(candidate), attempts)),
        }
    }
}
