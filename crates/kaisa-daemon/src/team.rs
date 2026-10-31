//! A verified goal (T31, ADR-0030): a developer attempt, trusted checks on its frozen candidate,
//! a reviewer attempt on the same candidate, the delivery gate, and bounded revision. Nothing
//! reaches the user unless the gate says so.

use std::path::{Path, PathBuf};

use kaisa_context::DetectedCommand;
use kaisa_domain::{ReviewVerdict, Role, WriteSet};
use kaisa_sys::ProcessExit;
use kaisa_workspace::{Candidate, CandidateCheckout, RiskFinding, freeze_candidate};

use crate::gate::{Delivery, decide_delivery};
use crate::goal::{AgentLauncher, GoalError, GoalOutcome, GoalRequest, at, run_goal};
use crate::ids::fresh_suffix;
use crate::review::verdict_in_answer;
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
         when the goal is not met or the change is unsafe. End your answer with exactly one \
         unquoted ```kaisa-verdict fence containing only the JSON verdict below, then a closing \
         ``` on its own line. No text may follow it; never repeat that dedicated marker in examples. \
         A one-line-only protocol may instead end with exactly one line starting at column 1 \
         with \"kaisa-verdict: \" followed by the complete JSON. Use exactly one of these formats. \
         Verdict schema: \
         {{\"candidate_tree\": \"...\", \"approve\": true|false, \"findings\": [{{\"severity\": \
         \"blocking\"|\"advisory\", \"path\": \"...\", \"line\": n|null, \"message\": \"...\"}}]}}\n\n\
         Goal: {goal}\n{CANDIDATE_TREE_LABEL}{}\n",
        candidate.tree
    )
}

/// A review is consumable only after a successful process and a complete capture.
fn reviewer_failure(review: &GoalOutcome) -> Option<Delivery> {
    if review.truncated {
        return Some(Delivery::Refuse(
            "the reviewer transcript was truncated".to_owned(),
        ));
    }
    if review.exit != Some(ProcessExit::Code(0)) {
        return Some(Delivery::Refuse(
            "the reviewer did not exit successfully with code 0".to_owned(),
        ));
    }
    None
}

/// The verdict in a reviewer's recorded transcript, if it validates.
fn verdict_from(state_directory: &Path, review: &GoalOutcome, tree: &str) -> Option<ReviewVerdict> {
    let transcript: String = std::fs::read_to_string(
        state_directory
            .join("attempts")
            .join(review.attempt.as_str())
            .join("transcript.log"),
    )
    .ok()?;
    verdict_in_transcript(&transcript, tree)
}

/// The one verdict in the final answer of `transcript`, read the way its agent writes it, if it
/// validates as a verdict on `tree`.
fn verdict_in_transcript(transcript: &str, tree: &str) -> Option<ReviewVerdict> {
    let answer: String = kaisa_agents::final_answer(transcript)?;
    verdict_in_answer(&answer, tree).ok()
}

/// Why a person must decide, naming each risky settings file in `whose` agent settings.
fn settings_widen_authority(whose: &str, risks: &[RiskFinding]) -> Delivery {
    let named: Vec<String> = risks
        .iter()
        .map(|risk| format!("{} ({:?})", risk.path, risk.rule))
        .collect();
    Delivery::NeedsPerson(format!(
        "{whose} agent settings widen what an agent may do: {}",
        named.join(", ")
    ))
}

/// The risky agent settings in `candidate`'s own tree. A reviewer or revision developer starts
/// in a worktree of that commit and loads what it holds, which the developer may have changed.
fn candidate_settings_risks(
    request: &VerifiedRequest<'_>,
    candidate: &Candidate,
) -> Result<Vec<RiskFinding>, GoalError> {
    std::fs::create_dir_all(request.state_directory.join("checks"))
        .map_err(at("preparing checks"))?;
    let place: PathBuf = request
        .state_directory
        .join("checks")
        .join(format!("settings_{}", fresh_suffix()));
    let checkout = CandidateCheckout::create(request.repository, candidate, &place)
        .map_err(at("checking out the candidate"))?;
    kaisa_workspace::scan_settings(checkout.path())
        .map_err(at("scanning the candidate's agent settings"))
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
        return Ok(VerifiedOutcome {
            delivery: settings_widen_authority("the repository's", &risks),
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
            egress_hosts: request.egress_hosts,
            agent_writable: request.agent_writable,
            local_model_port: None,
            observed: request.observed,
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
                attempts,
            ));
        }
        // Every later agent starts from this candidate, so its settings are judged again first.
        let risks: Vec<RiskFinding> = candidate_settings_risks(request, &candidate)?;
        if !risks.is_empty() {
            return Ok(finish(
                settings_widen_authority("the candidate's", &risks),
                Some(candidate),
                attempts,
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
            let glanced: GoalOutcome = run_goal(&GoalRequest {
                repository: request.repository,
                goal: &review_instruction(request.goal, &candidate),
                base: &candidate.commit,
                state_directory: request.state_directory,
                write_set: &no_paths,
                role: Role::Reviewer,
                launch: first_pass.launch,
                kaisa_executable: request.kaisa_executable,
                confine_agent: request.confine_agent && request.policy.policy().confine_agents,
                egress_hosts: &[],
                agent_writable: &[],
                local_model_port: first_pass.model_port,
                observed: false,
            })?;
            if let Some(failure) = reviewer_failure(&glanced) {
                attempts.push(glanced);
                return Ok(finish(failure, Some(candidate), attempts));
            }
            let glance: Option<ReviewVerdict> =
                verdict_from(request.state_directory, &glanced, &candidate.tree);
            attempts.push(glanced);
            // Only valid blocking findings act on the advisory first pass; an invalid answer
            // still requires the full reviewer to make the decision.
            if let Some(glance) = glance
                && let Delivery::Revise(reasons) =
                    decide_delivery(&candidate, &evidence, Some(&glance))
            {
                if !budget.take() {
                    return Ok(finish(Delivery::Revise(reasons), Some(candidate), attempts));
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
        })?;
        if let Some(failure) = reviewer_failure(&reviewed) {
            attempts.push(reviewed);
            return Ok(finish(failure, Some(candidate), attempts));
        }
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

#[cfg(test)]
mod tests {
    use super::verdict_in_transcript;

    /// The tree the captured reviews approve: git's empty tree.
    const REVIEWED_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    /// Another candidate's tree.
    const OTHER_TREE: &str = "9daeafb9864cf43055ae93beb0afd6c7d144bfa4";
    /// A real Claude Code stream-json review whose answer wraps an approval in prose and a fence.
    const CLAUDE_APPROVES: &str =
        include_str!("../../kaisa-agents/src/fixtures/claude-review-approves.jsonl");
    /// The same review answered in prose only: its last line is still a `result` event.
    const CLAUDE_PROSE_ONLY: &str =
        include_str!("../../kaisa-agents/src/fixtures/claude-review-prose-only.jsonl");
    /// The review exactly as captured: the model invented a severity and a field name.
    const CLAUDE_AS_CAPTURED: &str =
        include_str!("../../kaisa-agents/src/fixtures/claude-review-captured.jsonl");
    /// A real Codex `exec --json` review whose last agent message is a fenced approval.
    const CODEX_APPROVES: &str =
        include_str!("../../kaisa-agents/src/fixtures/codex-review-approves.jsonl");

    #[test]
    fn a_real_claude_review_transcript_delivers_its_approval() {
        let verdict = verdict_in_transcript(CLAUDE_APPROVES, REVIEWED_TREE)
            .expect("the approval in Claude's result text is found");

        assert!(
            verdict.approves(REVIEWED_TREE),
            "a stream-json reviewer's approval reaches the gate, not its trailing result line"
        );
    }

    #[test]
    fn a_claude_result_without_a_verdict_never_counts_as_approval() {
        assert_eq!(
            verdict_in_transcript(CLAUDE_PROSE_ONLY, REVIEWED_TREE),
            None,
            "an approving tone in prose is not a verdict"
        );
    }

    #[test]
    fn a_real_claude_verdict_outside_the_schema_never_counts_as_approval() {
        assert_eq!(
            verdict_in_transcript(CLAUDE_AS_CAPTURED, REVIEWED_TREE),
            None,
            "a verdict with an unknown severity is malformed, so it is a failed review"
        );
    }

    #[test]
    fn a_real_codex_review_transcript_delivers_its_approval() {
        let verdict = verdict_in_transcript(CODEX_APPROVES, REVIEWED_TREE)
            .expect("the approval in Codex's last agent message is found");

        assert!(
            verdict.approves(REVIEWED_TREE),
            "a Codex reviewer's approval reaches the gate, not its usage event"
        );
    }

    #[test]
    fn a_real_review_of_another_candidate_never_counts_as_approval() {
        assert_eq!(
            (
                verdict_in_transcript(CLAUDE_APPROVES, OTHER_TREE),
                verdict_in_transcript(CODEX_APPROVES, OTHER_TREE)
            ),
            (None, None),
            "finding the verdict in an agent's answer still binds it to the reviewed tree"
        );
    }
}
