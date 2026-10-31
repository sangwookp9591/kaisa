//! A verified goal (T31, ADR-0030): a developer attempt, trusted checks on its frozen candidate,
//! a reviewer attempt on the same candidate, the delivery gate, and bounded revision. Nothing
//! reaches the user unless the gate says so.

use std::path::{Path, PathBuf};

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
    /// Whether the developer is an observed agent (Codex), whose writes are checked against
    /// the write set only after it finishes.
    pub observed: bool,
    /// A light first-pass reviewer on a local model (F37, T33): its blocking findings send the
    /// candidate back without spending the full review; its approval is only advisory.
    pub first_pass: Option<FirstPass<'request>>,
    /// Hosts the developer and reviewer may reach through the egress broker.
    pub egress_hosts: &'request [&'request str],
    /// Paths outside the worktree the agents must be able to change (for example their own
    /// session files).
    pub agent_writable: &'request [PathBuf],
    /// What may admit an attempt whose containment falls short (D02).
    pub reduced_mode: &'request kaisa_domain::ReducedModeBasis,
}

/// A first-pass reviewer and the loopback port of the local model it talks to.
#[derive(Clone, Copy)]
pub struct FirstPass<'request> {
    /// How to start it.
    pub launch: AgentLauncher<'request>,
    /// The model server's loopback port, which its sandbox must allow.
    pub model_port: Option<u16>,
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
    let mut attempts = Vec::new();
    run_verified_goal_inner(request, &mut attempts).map_err(|mut error| {
        for attempt in attempts.iter().filter(|attempt| !attempt.confined) {
            error.detail = format!("{}\n{}", attempt.summary(), error.detail);
        }
        error
    })
}

fn run_verified_goal_inner(
    request: &VerifiedRequest<'_>,
    attempts: &mut Vec<GoalOutcome>,
) -> Result<VerifiedOutcome, GoalError> {
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
            egress_hosts: request.egress_hosts,
            agent_writable: request.agent_writable,
            local_model_port: None,
            observed: request.observed,
            reduced_mode: request.reduced_mode,
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
                std::mem::take(attempts),
            ));
        };
        let candidate: Candidate =
            freeze_candidate(request.repository, &commit).map_err(at("freezing the candidate"))?;
        // Brokered writes were checked one by one; this also catches an observed agent's, and
        // covers every revision since the original base.
        let outside: Vec<String> =
            kaisa_workspace::changed_paths(request.repository, &original_base.commit, &candidate)
                .map_err(at("listing the candidate's changes"))?
                .into_iter()
                .filter(|path| {
                    !kaisa_domain::RepoPath::try_new(path)
                        .is_ok_and(|path| request.write_set.permits(&path))
                })
                .collect();
        if !outside.is_empty() {
            return Ok(finish(
                Delivery::NeedsPerson(format!(
                    "the candidate changed paths outside its write set: {}",
                    outside.join(", ")
                )),
                Some(candidate),
                std::mem::take(attempts),
            ));
        }
        let evidence: Vec<Evidence> = run_trusted_checks(
            request.repository,
            request.state_directory,
            &original_base.commit,
            &candidate,
            request.checks,
        )?;
        if let Some(first_pass) = request.first_pass {
            let local_basis = match request.reduced_mode {
                kaisa_domain::ReducedModeBasis::Decision(_) => request.reduced_mode.clone(),
                _ if cfg!(windows) => kaisa_domain::ReducedModeBasis::DefaultPolicy(
                    kaisa_domain::DefaultReducedPolicy::WindowsLocalModel,
                ),
                _ => kaisa_domain::ReducedModeBasis::None,
            };
            let glanced: GoalOutcome = run_goal(&GoalRequest {
                repository: request.repository,
                goal: &review_instruction(request.goal, &candidate),
                base: &candidate.commit,
                state_directory: request.state_directory,
                write_set: &no_paths,
                role: Role::Reviewer,
                launch: first_pass.launch,
                kaisa_executable: request.kaisa_executable,
                confine_agent: !cfg!(windows),
                egress_hosts: &[],
                agent_writable: &[],
                local_model_port: first_pass.model_port,
                observed: false,
                reduced_mode: &local_basis,
            })?;
            let glance: Option<ReviewVerdict> =
                verdict_from(request.state_directory, &glanced, &candidate.tree);
            attempts.push(glanced);
            // Only a send-back is acted on; anything else still goes to the full review.
            if let Some(glance) = glance
                && let Delivery::Revise(reasons) =
                    decide_delivery(&candidate, &evidence, Some(&glance))
            {
                if !budget.take() {
                    return Ok(finish(
                        Delivery::Revise(reasons),
                        Some(candidate),
                        std::mem::take(attempts),
                    ));
                }
                base = candidate.commit.clone();
                instruction = revision_goal(request.goal, &reasons);
                continue;
            }
        }
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
            egress_hosts: request.egress_hosts,
            agent_writable: request.agent_writable,
            local_model_port: None,
            observed: false,
            reduced_mode: request.reduced_mode,
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
            other => return Ok(finish(other, Some(candidate), std::mem::take(attempts))),
        }
    }
}
