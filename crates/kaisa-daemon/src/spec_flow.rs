//! Specification-bound workflow. Publication is exclusively downstream of the pure gate.
use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_context::{LintPolicy, SourceFile, lint_tests};
use kaisa_domain::spec::TestMapping;
use kaisa_domain::{EventKind, Role, SpecId, Specification, WriteSet};
use kaisa_ledger::EventStore;
use kaisa_ledger::spec::{SpecStore, StoredSpec};
use kaisa_sys::{ProcessExit, ProcessSpec};
use kaisa_workspace::{Candidate, CandidateCheckout, QaKind, QaSuite, freeze_candidate};
use serde_json::{Value, json};

use crate::acceptance::{
    AcceptanceDecision, AcceptanceEvidence, AcceptanceStatus, GateCheck, acceptance_gate,
};
use crate::goal::{AttemptContext, GoalError, GoalRequest, at, record, run_goal_with_context};
use crate::qa::{
    QaIsolation, QaRequest, QaResult, QaSuiteRequest, QaToolchain, qa_isolation, revision_feedback,
    run_private_qa, run_qa_suite,
};
pub use crate::spec_adapters::{AgentStagedReview, LedgerCharacterize};
use crate::traceability::{TraceabilityReport, traceability_report};
use crate::{
    GoalOutcome, LabelledTest, TestOrigin, TestReviewInput, TestReviewRequest, VerifiedRequest,
    prepare_review_repository, run_trusted_checks,
};

/// Shared frozen inputs for review and characterization. Baselines have no candidate.
#[derive(Clone, Copy)]
pub struct HookContext<'a> {
    /// Original repository, never a mutable developer workspace.
    pub repository: &'a Path,
    /// Private run state.
    pub state_directory: &'a Path,
    /// Run receipt identity.
    pub run_id: &'a str,
    /// Trusted original base.
    pub base: &'a Candidate,
    /// Frozen candidate, absent before development.
    pub candidate: Option<&'a Candidate>,
    /// Frozen, verified specification.
    pub specification: &'a Specification,
}
/// WP6 seam: baseline before development, then evaluate each frozen candidate.
pub trait Characterize {
    /// Record baseline evidence before any developer attempt.
    fn baseline(&self, context: &HookContext<'_>) -> Result<GateCheck, GoalError>;
    /// Re-run/check baseline evidence on the final tree and explain criterion corrections.
    fn evaluate(
        &self,
        context: &HookContext<'_>,
        baseline: &GateCheck,
    ) -> Result<GateCheck, GoalError>;
}
/// Explicit no-op for tests that isolate other workflow stages.
pub struct NoCharacterization;
impl Characterize for NoCharacterization {
    fn baseline(&self, context: &HookContext<'_>) -> Result<GateCheck, GoalError> {
        hook_receipt(
            context,
            "characterization-baseline",
            AcceptanceStatus::Pass,
            "not applicable (no characterization adapter configured)",
            None,
        )
    }
    fn evaluate(&self, context: &HookContext<'_>, _: &GateCheck) -> Result<GateCheck, GoalError> {
        hook_receipt(
            context,
            "characterization",
            AcceptanceStatus::Pass,
            "not applicable (no characterization adapter configured)",
            context.candidate.map(|candidate| candidate.tree.as_str()),
        )
    }
}
/// Complete context a WP5 adapter may consume; private tests never enter developer feedback.
pub struct StagedReviewInput<'a, 'request> {
    /// Frozen repository/specification inputs.
    pub context: HookContext<'a>,
    /// Trusted launch/policy configuration for the staged reviewer.
    pub workflow: &'a VerifiedRequest<'request>,
    /// Original-base to final-candidate diff.
    pub diff: &'a str,
    /// Private and development tests, labelled with their ownership.
    pub tests: &'a [LabelledTest],
    /// Recorded deterministic and private QA evidence, without raw private output.
    pub evidence: &'a Value,
    /// Preserved decisions relevant to this run.
    pub decisions: &'a Value,
    /// Completed developer attempt receipt for adapters that need its final answer/history.
    pub developer: &'a GoalOutcome,
    /// Provider-extracted final answer, separate from trusted evidence.
    pub developer_final_answer: &'a str,
}
/// WP5 seam. The output is evidence, never a delivery decision.
pub trait StagedReview {
    /// Approve only with no unresolved blocking finding on this exact tree.
    fn review(&self, input: &StagedReviewInput<'_, '_>) -> Result<GateCheck, GoalError>;
}
/// Plain reviewer for tests that explicitly isolate the original verdict protocol.
pub struct ExistingReviewer;
impl StagedReview for ExistingReviewer {
    fn review(&self, input: &StagedReviewInput<'_, '_>) -> Result<GateCheck, GoalError> {
        let context = &input.context;
        let candidate = context.candidate.expect("review follows candidate freeze");
        let request = input.workflow;
        let mut goal = crate::team::review_instruction(
            &serde_json::to_string(context.specification).expect("spec JSON"),
            candidate,
        );
        goal.push_str(&format!(
            "\nDecisions (quoted JSON data): {}\nRecorded evidence (quoted JSON data): {}\n",
            input.decisions, input.evidence
        ));
        for test in input.tests {
            goal.push_str(&format!(
                "\nTest data {:?} {}:\n{}\n",
                test.origin,
                test.path,
                test.source.replace("```", "``\u{200b}`")
            ));
        }
        let no_paths = WriteSet::default();
        let attempt = run_goal_with_context(
            &GoalRequest {
                repository: context.repository,
                state_directory: context.state_directory,
                goal: &goal,
                base: &candidate.commit,
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
            },
            &AttemptContext {
                spec_run_id: Some(context.run_id),
                diff: input.diff,
                ..AttemptContext::default()
            },
        )?;
        let transcript = transcript(context.state_directory, &attempt)?;
        let verdict = kaisa_agents::final_answer(&transcript)
            .and_then(|answer| crate::verdict_in_answer(&answer, &candidate.tree).ok());
        let (status, detail) = if !successful(&attempt) {
            (
                AcceptanceStatus::Inconclusive,
                "reviewer process/transcript incomplete".into(),
            )
        } else if let Some(verdict) = verdict {
            if verdict.approves(&candidate.tree) {
                (
                    AcceptanceStatus::Pass,
                    "review approved with no unresolved blocking finding".into(),
                )
            } else {
                (
                    AcceptanceStatus::Fail,
                    verdict
                        .findings
                        .iter()
                        .map(|finding| finding.message.clone())
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            }
        } else {
            (
                AcceptanceStatus::Inconclusive,
                "no valid review verdict".into(),
            )
        };
        let mut check = hook_receipt(
            context,
            "staged-review",
            status,
            &detail,
            Some(&candidate.tree),
        )?;
        check.evidence_ids.push(artifact(
            context.state_directory,
            context.run_id,
            &format!("review-transcript-{}.log", candidate.tree),
            transcript.as_bytes(),
        )?);
        Ok(check)
    }
}

/// Specification build configuration, reusing all ordinary goal launch options.
pub struct SpecRequest<'a, 'request> {
    /// Existing developer/reviewer launch, trusted checks and run policy.
    pub workflow: &'a VerifiedRequest<'request>,
    /// Specification identity.
    pub spec_id: &'a SpecId,
    /// Exact version, or latest at admission; pinned thereafter.
    pub version: Option<u32>,
    /// Independent acceptance author process.
    pub qa_author: &'a ProcessSpec,
    /// Independent adversarial author process.
    pub qa_blind: &'a ProcessSpec,
    /// Trusted QA compiler/runner.
    pub qa_toolchain: &'a QaToolchain,
    /// Domain test review launcher.
    pub test_reviewer: crate::AgentLauncher<'a>,
    /// Baseline adapter (production uses `LedgerCharacterize`).
    pub characterize: &'a dyn Characterize,
    /// Context-aware review adapter (production uses `AgentStagedReview`).
    pub staged_review: &'a dyn StagedReview,
    /// Explicit policy acceptance of a named reduced QA isolation boundary; absent by default.
    pub accepted_qa_isolation: Option<&'a str>,
    /// Optional development test mappings. Unknown tests/criteria are lint-blocking.
    pub development_mappings: &'a [TestMapping],
}
/// Result includes receipts and recovery attempts; only PASS includes a delivery branch.
pub struct SpecOutcome {
    /// Run id usable with `kaisa spec report`.
    pub run_id: String,
    /// Orchestrator gate receipt.
    pub acceptance: AcceptanceDecision,
    /// Final frozen candidate if any.
    pub candidate: Option<Candidate>,
    /// Published only after PASS.
    pub result_branch: Option<String>,
    /// Durable Markdown report location.
    pub report_path: PathBuf,
    /// Developer attempts kept for recovery, never advertised as delivery on failure.
    pub attempts: Vec<GoalOutcome>,
}
fn successful(attempt: &GoalOutcome) -> bool {
    attempt.exit == Some(ProcessExit::Code(0))
        && !attempt.truncated
        && attempt.kept_back.is_empty()
        && attempt.permission_denials.is_empty()
}
fn transcript(state: &Path, attempt: &GoalOutcome) -> Result<String, GoalError> {
    std::fs::read_to_string(
        state
            .join("attempts")
            .join(attempt.attempt.as_str())
            .join("transcript.log"),
    )
    .map_err(at("reading recorded transcript"))
}
fn run_directory(state: &Path, run: &str) -> PathBuf {
    state.join("spec-runs").join(run)
}
pub(crate) fn artifact(
    state: &Path,
    run: &str,
    name: &str,
    bytes: &[u8],
) -> Result<String, GoalError> {
    let path = run_directory(state, run).join(name);
    std::fs::create_dir_all(path.parent().expect("artifact parent"))
        .map_err(at("preparing specification run artifacts"))?;
    std::fs::write(&path, bytes).map_err(at("preserving specification evidence"))?;
    Ok(format!("{name}:blake3:{}", blake3::hash(bytes).to_hex()))
}
pub(crate) fn hook_receipt(
    context: &HookContext<'_>,
    name: &str,
    status: AcceptanceStatus,
    detail: &str,
    tree: Option<&str>,
) -> Result<GateCheck, GoalError> {
    let bytes = serde_json::to_vec(&json!({"status":status,"detail":detail,"candidate_tree":tree}))
        .expect("receipt JSON");
    let evidence = artifact(
        context.state_directory,
        context.run_id,
        &format!(
            "{name}-{}.json",
            context
                .candidate
                .map_or("base", |candidate| candidate.tree.as_str())
        ),
        &bytes,
    )?;
    Ok(GateCheck::recorded(status, detail, evidence, tree))
}
pub(crate) fn event(
    state: &Path,
    run: &str,
    kind: EventKind,
    mut payload: Value,
) -> Result<(), GoalError> {
    payload["run_id"] = json!(run);
    let mut ledger = EventStore::open(&state.join("kaisa.db"))
        .map_err(at("opening specification workflow ledger"))?;
    record(&mut ledger, kind, run, payload)
}
fn containment_receipts(
    state: &Path,
    run_id: &str,
) -> Result<Vec<crate::acceptance::AttemptContainment>, GoalError> {
    let ledger =
        EventStore::open(&state.join("kaisa.db")).map_err(at("opening containment receipts"))?;
    ledger
        .verify()
        .map_err(at("verifying containment receipts"))?;
    let events = ledger
        .read_from(kaisa_domain::Sequence::FIRST)
        .map_err(at("reading containment receipts"))?;
    let mut receipts = std::collections::BTreeMap::new();
    for entry in events {
        let recorded = entry.recorded;
        let payload: Value = serde_json::from_str(&recorded.event.payload)
            .map_err(at("parsing containment receipt"))?;
        if payload["run_id"] != run_id {
            continue;
        }
        let id = &recorded.event.subject;
        if recorded.event.kind == EventKind::AttemptAdmitted {
            receipts.insert(
                id.clone(),
                crate::acceptance::AttemptContainment {
                    attempt: id.clone(),
                    role: payload["role"].as_str().unwrap_or("unknown").into(),
                    missing: Vec::new(),
                    admitted: false,
                    observed: false,
                    evidence_id: format!("ledger:{}", recorded.sequence.value()),
                },
            );
        } else if recorded.event.kind == EventKind::AttemptStateChanged
            && !payload["containment"].is_null()
        {
            let receipt = receipts.get_mut(id).ok_or_else(|| GoalError {
                step: "containment receipt",
                detail: "actual containment has no run-scoped admission".into(),
            })?;
            let containment = &payload["containment"];
            receipt.missing = serde_json::from_value(containment["missing"].clone())
                .map_err(at("reading missing containment"))?;
            receipt.admitted = containment["admitted"] == true;
            receipt.observed = containment["observed"] == true;
            receipt.evidence_id = format!("ledger:{}", recorded.sequence.value());
        }
    }
    Ok(receipts.into_values().collect())
}

fn missing_evidence() -> AcceptanceEvidence {
    let unknown = || GateCheck::missing("stage has not produced evidence");
    AcceptanceEvidence {
        final_tree: None,
        attempts: Vec::new(),
        provisional: false,
        reduced_assurance_approval: false,
        approval: unknown(),
        spec_digest: unknown(),
        traceability: unknown(),
        deterministic: unknown(),
        private_qa: unknown(),
        test_review: unknown(),
        staged_review: unknown(),
        characterization: unknown(),
        preservation: unknown(),
        qa_isolation: unknown(),
    }
}
fn approval_check(
    stored: &StoredSpec,
    policy: &kaisa_domain::PolicySnapshot,
    evidence: &str,
) -> GateCheck {
    match stored.approval.as_ref().filter(|_| stored.frozen) {
        Some(record)
            if record.kind == "human-approved"
                && record.receipt["attempts_unconfined_since_draft"] == true =>
        {
            GateCheck::recorded(
                if policy.policy().accept_reduced_assurance_approval {
                    AcceptanceStatus::Pass
                } else {
                    AcceptanceStatus::Inconclusive
                },
                format!(
                    "human-approved (reduced assurance); terminal receipt verified; accept_reduced_assurance_approval={}",
                    policy.policy().accept_reduced_assurance_approval
                ),
                evidence,
                None,
            )
        }
        Some(record) if record.kind == "human-approved" => GateCheck::recorded(
            AcceptanceStatus::Pass,
            "human-approved; terminal receipt verified",
            evidence,
            None,
        ),
        Some(record) if record.kind == "provisional" && policy.policy().allow_provisional_spec => {
            GateCheck::recorded(
                AcceptanceStatus::Pass,
                "provisional spec; explicitly allowed by policy",
                evidence,
                None,
            )
        }
        Some(_) => {
            GateCheck::missing("provisional spec refused: allow_provisional_spec policy is false")
        }
        None => GateCheck::missing("specification is not frozen with verified approval"),
    }
}
/// Runs a bound goal through independent verification and bounded revisions.
/// # Errors
/// Returns errors for unavailable state/storage, never delivery on an incomplete run.
pub fn run_spec_goal(request: &SpecRequest<'_, '_>) -> Result<SpecOutcome, GoalError> {
    let workflow = request.workflow;
    let state = workflow.state_directory;
    kaisa_sys::ensure_private_directory(state).map_err(at("preparing specification state"))?;
    let run_id = format!("run_{}", crate::ids::fresh_suffix());
    let mut evidence = missing_evidence();
    let mut attempts = Vec::new();
    let mut candidate = None;
    EventStore::open(&state.join("kaisa.db"))
        .and_then(|mut ledger| ledger.record_policy(workflow.policy))
        .map_err(at("recording specification policy"))?;
    event(
        state,
        &run_id,
        EventKind::RunStarted,
        json!({"goal":workflow.goal,"spec_id":request.spec_id.as_str(),"version":request.version,"accepted_qa_isolation":request.accepted_qa_isolation,"repository":crate::learn::repository_identity(workflow.repository)}),
    )?;
    let store = SpecStore::new(state);
    let stored = match store.read(request.spec_id, request.version) {
        Ok(stored) => stored,
        Err(error) => {
            evidence.spec_digest = GateCheck::recorded(
                AcceptanceStatus::Fail,
                error.to_string(),
                artifact(
                    state,
                    &run_id,
                    "spec-load-error.json",
                    error.to_string().as_bytes(),
                )?,
                None,
            );
            return finish(request, run_id, evidence, candidate, attempts, None);
        }
    };
    let spec = &stored.specification;
    evidence.provisional = stored
        .approval
        .as_ref()
        .is_some_and(|approval| approval.kind == "provisional");
    evidence.reduced_assurance_approval = stored.approval.as_ref().is_some_and(|approval| {
        approval.kind == "human-approved"
            && approval.receipt["attempts_unconfined_since_draft"] == true
    });
    let spec_receipt = artifact(
        state,
        &run_id,
        "frozen-spec.json",
        &serde_json::to_vec(
            &json!({"specification":spec,"digest":stored.digest,"approval":stored.approval}),
        )
        .expect("spec JSON"),
    )?;
    evidence.approval = approval_check(&stored, workflow.policy, &spec_receipt);
    evidence.spec_digest = GateCheck::recorded(
        AcceptanceStatus::Pass,
        "frozen digest and approval receipt verified",
        &spec_receipt,
        None,
    );
    let initial_trace = traceability_report(&run_id, spec, None, &[], &[]);
    if !spec.validate().is_empty() {
        evidence.traceability = GateCheck::recorded(
            AcceptanceStatus::Fail,
            format!("{:?}", spec.validate()),
            &spec_receipt,
            None,
        );
        return finish(
            request,
            run_id,
            evidence,
            candidate,
            attempts,
            Some(initial_trace),
        );
    }
    if evidence.approval.status != AcceptanceStatus::Pass {
        return finish(
            request,
            run_id,
            evidence,
            candidate,
            attempts,
            Some(initial_trace),
        );
    }
    let base = freeze_candidate(workflow.repository, "HEAD")
        .map_err(at("freezing trusted specification base"))?;
    let baseline_context = HookContext {
        repository: workflow.repository,
        state_directory: state,
        run_id: &run_id,
        base: &base,
        candidate: None,
        specification: spec,
    };
    let baseline = match request.characterize.baseline(&baseline_context) {
        Ok(receipt) => receipt,
        Err(error) => hook_receipt(
            &baseline_context,
            "characterization-unavailable",
            AcceptanceStatus::Inconclusive,
            &error.to_string(),
            None,
        )?,
    };
    if baseline.status != AcceptanceStatus::Pass {
        evidence.characterization = baseline;
        return finish(
            request,
            run_id,
            evidence,
            candidate,
            attempts,
            Some(initial_trace),
        );
    }
    let settings = kaisa_workspace::scan_settings(workflow.repository)
        .map_err(at("checking original agent settings"))?;
    if !settings.is_empty() {
        evidence.deterministic = hook_receipt(
            &baseline_context,
            "unsafe-settings",
            AcceptanceStatus::Fail,
            "repository agent settings widen authority",
            None,
        )?;
        return finish(
            request,
            run_id,
            evidence,
            candidate,
            attempts,
            Some(initial_trace),
        );
    }
    let protected_copies = spec_copies(workflow.repository, &base, spec)?;
    let mut suites: Vec<QaSuite> = Vec::new();
    let mut feedback = Vec::new();
    let mut development_base = base.commit.clone();
    let mut final_trace = initial_trace;
    for revision in 0..=workflow.policy.policy().revision_limit {
        let instruction = format!(
            "Implement the frozen specification below. Requirements and criteria are immutable. Private QA source is unavailable; fix behaviour from observable failures.\n{}",
            String::from_utf8(spec.canonical_json()).expect("JSON UTF-8")
        );
        let developed = match run_goal_with_context(
            &GoalRequest {
                repository: workflow.repository,
                goal: &instruction,
                base: &development_base,
                state_directory: state,
                write_set: workflow.write_set,
                role: Role::Developer,
                launch: workflow.developer,
                kaisa_executable: workflow.kaisa_executable,
                confine_agent: workflow.confine_agent && workflow.policy.policy().confine_agents,
                egress_hosts: workflow.egress_hosts,
                agent_writable: workflow.agent_writable,
                local_model_port: None,
                observed: workflow.observed,
                reduced_mode: workflow.reduced_mode,
            },
            &AttemptContext {
                findings: &feedback,
                spec_run_id: Some(&run_id),
                ..AttemptContext::default()
            },
        ) {
            Ok(attempt) => attempt,
            Err(error) => {
                evidence.deterministic = hook_receipt(
                    &baseline_context,
                    "developer-admission",
                    AcceptanceStatus::Inconclusive,
                    &error.to_string(),
                    None,
                )?;
                break;
            }
        };
        let commit = developed.commit.clone();
        let developer_ok = successful(&developed);
        attempts.push(developed);
        if !developer_ok || commit.is_none() {
            evidence.deterministic = hook_receipt(
                &baseline_context,
                "developer-failed",
                AcceptanceStatus::Fail,
                "developer attempt failed, was denied protected writes, or produced no preserved candidate",
                None,
            )?;
            break;
        }
        let frozen = freeze_candidate(
            workflow.repository,
            commit.as_deref().expect("successful commit"),
        )
        .map_err(at("freezing specification candidate"))?;
        evidence.final_tree = Some(frozen.tree.clone());
        candidate = Some(frozen.clone());
        event(
            state,
            &run_id,
            EventKind::CandidateFrozen,
            json!({"candidate_tree":frozen.tree,"commit":frozen.commit,"revision":revision}),
        )?;
        let context = HookContext {
            candidate: Some(&frozen),
            ..baseline_context
        };
        // Re-read from the ledger-backed store, checking receipts as well as content bytes.
        let unchanged = store
            .read(&spec.id, Some(spec.version))
            .is_ok_and(|fresh| fresh == stored);
        let copies_unchanged = protected_copies.iter().all(|(path, bytes)| {
            git_blob(workflow.repository, &frozen.tree, path).is_ok_and(|current| current == *bytes)
        });
        evidence.spec_digest = hook_receipt(
            &context,
            "spec-integrity",
            if unchanged && copies_unchanged {
                AcceptanceStatus::Pass
            } else {
                AcceptanceStatus::Fail
            },
            if unchanged && copies_unchanged {
                "approved specification and repository copies unchanged"
            } else {
                "approved specification, approval ledger, or criterion in repository copy changed"
            },
            None,
        )?;
        if evidence.spec_digest.status == AcceptanceStatus::Fail {
            break;
        }
        let mut stage = EvaluationStage::Deterministic;
        let evaluation = evaluate_candidate(
            request,
            &context,
            &attempts,
            &mut suites,
            &mut evidence,
            &mut stage,
        );
        let (trace, next_feedback) = match evaluation {
            Ok(result) => result,
            Err(error) => {
                let check = stage.check(&mut evidence);
                let failure = hook_receipt(
                    &context,
                    "incomplete-stage",
                    if check.status == AcceptanceStatus::Fail {
                        AcceptanceStatus::Fail
                    } else {
                        AcceptanceStatus::Inconclusive
                    },
                    &format!("{}; {error}", check.detail),
                    None,
                )?;
                check.detail = failure.detail;
                check.status = failure.status;
                check.evidence_ids.extend(failure.evidence_ids);
                break;
            }
        };
        final_trace = trace;
        feedback = next_feedback;
        evidence.characterization = match request.characterize.evaluate(&context, &baseline) {
            Ok(receipt) => receipt,
            Err(error) => hook_receipt(
                &context,
                "characterization-incomplete",
                AcceptanceStatus::Inconclusive,
                &error.to_string(),
                Some(&frozen.tree),
            )?,
        };
        // Every agent, including both reviewers and hook agents, precedes this last rehash.
        if !store
            .read(&spec.id, Some(spec.version))
            .is_ok_and(|fresh| fresh == stored)
        {
            evidence.spec_digest = hook_receipt(
                &context,
                "spec-integrity-final",
                AcceptanceStatus::Fail,
                "approved spec or receipt changed during verification",
                None,
            )?;
        }
        evidence.attempts = containment_receipts(state, &run_id)?;
        let decision = acceptance_gate(&evidence);
        if decision.status != AcceptanceStatus::Fail
            || evidence.spec_digest.status == AcceptanceStatus::Fail
            || revision == workflow.policy.policy().revision_limit
        {
            break;
        }
        event(
            state,
            &run_id,
            EventKind::GateAcceptance,
            serde_json::to_value(&decision).expect("gate JSON"),
        )?;
        event(
            state,
            &run_id,
            EventKind::RevisionStarted,
            json!({"candidate_tree":frozen.tree,"revision":revision+1}),
        )?;
        development_base = frozen.commit;
        // All tree-dependent evidence must be replaced on the next revision.
        evidence = AcceptanceEvidence {
            provisional: evidence.provisional,
            reduced_assurance_approval: evidence.reduced_assurance_approval,
            approval: evidence.approval,
            spec_digest: evidence.spec_digest,
            ..missing_evidence()
        };
    }
    if !store
        .read(&spec.id, Some(spec.version))
        .is_ok_and(|fresh| fresh == stored)
    {
        evidence.spec_digest = hook_receipt(
            &baseline_context,
            "spec-integrity-after-attempts",
            AcceptanceStatus::Fail,
            "approved spec or approval receipt changed during an attempt",
            None,
        )?;
    }
    finish(
        request,
        run_id,
        evidence,
        candidate,
        attempts,
        Some(final_trace),
    )
}

#[derive(Clone, Copy)]
enum EvaluationStage {
    Deterministic,
    PrivateQa,
    Traceability,
    TestReview,
    StagedReview,
    Preservation,
}
impl EvaluationStage {
    fn check(self, evidence: &mut AcceptanceEvidence) -> &mut GateCheck {
        match self {
            Self::Deterministic => &mut evidence.deterministic,
            Self::PrivateQa => &mut evidence.private_qa,
            Self::Traceability => &mut evidence.traceability,
            Self::TestReview => &mut evidence.test_review,
            Self::StagedReview => &mut evidence.staged_review,
            Self::Preservation => &mut evidence.preservation,
        }
    }
}
// Retain every observed gap: a later full admission cannot undo a previous disclosure.
fn worst_qa_isolation(isolations: impl IntoIterator<Item = QaIsolation>) -> QaIsolation {
    let mut gaps = std::collections::BTreeSet::new();
    for isolation in isolations {
        if let QaIsolation::Reduced(reason) = isolation {
            gaps.insert(reason);
        }
    }
    if gaps.is_empty() {
        QaIsolation::Full
    } else {
        QaIsolation::Reduced(gaps.into_iter().collect::<Vec<_>>().join("; "))
    }
}

fn evaluate_candidate(
    request: &SpecRequest<'_, '_>,
    context: &HookContext<'_>,
    developers: &[GoalOutcome],
    suites: &mut Vec<QaSuite>,
    evidence: &mut AcceptanceEvidence,
    stage: &mut EvaluationStage,
) -> Result<(TraceabilityReport, Vec<String>), GoalError> {
    let developer = developers.last().expect("developer receipt");
    let workflow = request.workflow;
    let candidate = context.candidate.expect("candidate frozen");
    let state = context.state_directory;
    let tree = candidate.tree.as_str();
    let paths = kaisa_workspace::changed_paths(context.repository, &context.base.commit, candidate)
        .map_err(at("checking candidate write ownership"))?;
    let outside: Vec<_> = paths
        .iter()
        .filter(|path| {
            !kaisa_domain::RepoPath::try_new(path)
                .is_ok_and(|path| workflow.write_set.permits(&path))
        })
        .collect();
    let checks = run_trusted_checks(
        context.repository,
        state,
        &state.join("attempts").join(developer.attempt.as_str()),
        &context.base.commit,
        candidate,
        workflow.checks,
    )?;
    let mut deterministic_failures: Vec<String> = outside
        .iter()
        .map(|path| format!("write outside authorized set: {path}"))
        .collect();
    for check in &checks {
        event(
            state,
            context.run_id,
            EventKind::CheckEvidence,
            json!({"candidate_tree":check.candidate_tree,"command":check.check,"passed":check.passed(),"output_digest":check.output_digest,"confined":check.confined,"weakened_tests":check.weakened_tests,"definitions_changed":check.definitions_changed}),
        )?;
        if !check.passed() || !check.applies_to(candidate) {
            deterministic_failures.push(format!("trusted check failed: {:?}", check.check));
        }
        deterministic_failures.extend(check.weakened_tests.clone());
        deterministic_failures.extend(
            check
                .definitions_changed
                .iter()
                .map(|path| format!("check definitions changed: {path}")),
        );
    }
    // Protection also applies when there are no project-provided check commands.
    deterministic_failures.extend(
        kaisa_workspace::weakened_tests(context.repository, &context.base.commit, candidate)
            .map_err(at("checking protected tests"))?,
    );
    deterministic_failures.extend(
        kaisa_workspace::changed_check_definitions(
            context.repository,
            &context.base.commit,
            candidate,
        )
        .map_err(at("checking trusted definitions"))?
        .into_iter()
        .map(|path| format!("check definitions changed: {path}")),
    );
    let checkout_path =
        run_directory(state, context.run_id).join(format!("inspect-{}", candidate.tree));
    let checkout = CandidateCheckout::create(context.repository, candidate, &checkout_path)
        .map_err(at("checking out candidate for deterministic lint"))?;
    let risks = kaisa_workspace::scan_settings(checkout.path())
        .map_err(at("checking candidate agent settings"))?;
    deterministic_failures.extend(
        risks
            .iter()
            .map(|risk| format!("agent settings widen authority: {}", risk.path)),
    );
    evidence.deterministic = hook_receipt(
        context,
        "trusted-checks",
        if deterministic_failures.is_empty() {
            AcceptanceStatus::Inconclusive
        } else {
            AcceptanceStatus::Fail
        },
        &format!(
            "trusted checks complete; lint pending; {}",
            deterministic_failures.join("; ")
        ),
        Some(tree),
    )?;
    let isolation = worst_qa_isolation(developers.iter().map(qa_isolation));
    *stage = EvaluationStage::PrivateQa;
    // Matching bytes cannot prove that candidate code was denied writes to private inputs.
    // In particular Windows check execution is unconfined. Refuse this evidence boundary
    // explicitly rather than turning an unavailable guarantee into a test failure or PASS.
    if !crate::qa::qa_write_deny_available() {
        return Err(GoalError {
            step: "verifying private QA isolation",
            detail: "private QA write-deny unavailable; digests alone cannot verify private input isolation".into(),
        });
    }
    let results: Vec<QaResult> = if suites.is_empty() {
        let spec_json = serde_json::to_value(context.specification).expect("spec JSON");
        let outcome = run_private_qa(&QaRequest {
            repository: context.repository,
            base: &context.base.commit,
            state_directory: state,
            run_id: context.run_id,
            candidate,
            frozen_spec: &spec_json,
            developer,
            author: request.qa_author,
            blind: request.qa_blind,
            toolchain: request.qa_toolchain,
        })?;
        *suites = outcome.suites;
        outcome.results
    } else {
        let mut results = Vec::new();
        for suite in suites.iter() {
            results.extend(run_qa_suite(&QaSuiteRequest {
                repository: context.repository,
                base: &context.base.commit,
                state,
                run_id: context.run_id,
                candidate,
                suite,
                toolchain: request.qa_toolchain,
                attempt: developer,
            })?);
        }
        results
    };
    let results_json: Vec<_> = results.iter().map(|result| json!({"kind":result.kind.code(),"test_id":result.test_id,"criterion_ids":result.criterion_ids,"passed":result.passed,"candidate_tree":result.candidate_tree,"output_digest":result.output_digest,"qa_isolation":result.qa_isolation.label(),"qa_evidence":result.qa_evidence,"positive_evidence":result.positive_evidence,"private_test_integrity":result.private_test_integrity,"blocking_findings":result.blocking_findings})).collect();
    let qa_id = artifact(
        state,
        context.run_id,
        &format!("qa-{}.json", tree),
        &serde_json::to_vec(&results_json).expect("QA JSON"),
    )?;
    let expected_count: usize = suites
        .iter()
        .map(|suite| suite.verified_tests().map(|tests| tests.len()))
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(at("verifying private suites"))?
        .into_iter()
        .sum();
    let both_suites = suites
        .iter()
        .any(|suite| suite.kind() == QaKind::Acceptance)
        && suites
            .iter()
            .any(|suite| suite.kind() == QaKind::Adversarial);
    let qa_pass = both_suites
        && !results.is_empty()
        && results.len() == expected_count
        && results.iter().all(|result| {
            result.passed
                && result.candidate_tree == tree
                && result.source_unchanged
                && result.private_test_unchanged
                && result.positive_evidence
                && result.blocking_findings.is_empty()
        });
    evidence.private_qa = GateCheck::recorded(
        if qa_pass {
            AcceptanceStatus::Pass
        } else {
            AcceptanceStatus::Fail
        },
        format!(
            "acceptance + adversarial: {}/{} passed on final tree; qa_evidence: candidate-influenceable-in-process",
            results.iter().filter(|result| result.passed).count(),
            expected_count
        ),
        &qa_id,
        Some(tree),
    );
    evidence.qa_isolation = GateCheck::recorded(
        if isolation == QaIsolation::Full
            || request.accepted_qa_isolation == Some(isolation.label().as_str())
        {
            AcceptanceStatus::Pass
        } else {
            AcceptanceStatus::Inconclusive
        },
        format!(
            "qa_isolation: {}; explicit acceptance: {:?}",
            isolation.label(),
            request.accepted_qa_isolation
        ),
        &qa_id,
        Some(tree),
    );
    *stage = EvaluationStage::Deterministic;
    let development_sources = test_sources(checkout.path())?;
    let mut all_sources = development_sources.clone();
    let mut mappings = request.development_mappings.to_vec();
    let mut labelled: Vec<_> = development_sources
        .iter()
        .map(|source| LabelledTest {
            origin: TestOrigin::Development,
            path: source.path.clone(),
            source: source.source.clone(),
        })
        .collect();
    for suite in suites.iter() {
        for test in suite
            .verified_tests()
            .map_err(at("loading pinned tests for domain review"))?
        {
            let path = format!("private/{}/{}", suite.kind().code(), test.file);
            all_sources.push(SourceFile {
                path: path.clone(),
                source: test.contents.clone(),
            });
            labelled.push(LabelledTest {
                origin: TestOrigin::PrivateQa,
                path: path.clone(),
                source: test.contents,
            });
            if let Some(result) = results
                .iter()
                .find(|result| result.kind == suite.kind() && result.test_id == test.test_id)
            {
                for name in &result.expected_tests {
                    mappings.push(TestMapping {
                        test_id: format!("{path}::{name}"),
                        criterion_ids: test.criterion_ids.clone(),
                    });
                }
            }
        }
    }
    let coverage_mappings: Vec<_> = mappings
        .iter()
        .filter(|mapping| {
            !mapping.criterion_ids.is_empty() || request.development_mappings.contains(mapping)
        })
        .cloned()
        .collect();
    let coverage =
        kaisa_context::check_coverage(&context.specification.criteria, &coverage_mappings);
    if !coverage.is_complete() {
        deterministic_failures.push(format!("criterion coverage: {coverage:?}"));
    }
    let lint = lint_tests(
        &all_sources,
        &context.specification.criteria,
        &mappings,
        &LintPolicy::default(),
    );
    deterministic_failures.extend(
        lint.iter()
            .filter(|finding| finding.severity == kaisa_context::LintSeverity::Blocking)
            .map(ToString::to_string),
    );
    let development_lint = lint_tests(
        &development_sources,
        &context.specification.criteria,
        request.development_mappings,
        &LintPolicy::default(),
    );
    let valid_development: Vec<_> = request
        .development_mappings
        .iter()
        .filter(|mapping| {
            !development_lint.iter().any(|finding| {
                finding.rule == kaisa_context::LintRule::UnknownTest
                    && finding.test == mapping.test_id
            })
        })
        .cloned()
        .collect();
    *stage = EvaluationStage::Traceability;
    let trace = traceability_report(
        context.run_id,
        context.specification,
        Some(tree),
        &results,
        &valid_development,
    );
    let trace_id = artifact(
        state,
        context.run_id,
        &format!("traceability-{tree}.json"),
        &serde_json::to_vec_pretty(&trace).expect("trace JSON"),
    )?;
    evidence.traceability = GateCheck::recorded(
        if trace.gaps.is_empty() {
            AcceptanceStatus::Pass
        } else {
            AcceptanceStatus::Fail
        },
        if trace.gaps.is_empty() {
            "all requirements and criteria mapped to final-tree acceptance tests".into()
        } else {
            trace.gaps.join("; ")
        },
        trace_id,
        Some(tree),
    );
    *stage = EvaluationStage::Deterministic;
    let lint_id = artifact(state, context.run_id, &format!("deterministic-{tree}.json"), &serde_json::to_vec(&json!({"blocking":deterministic_failures,"lint":lint.iter().map(ToString::to_string).collect::<Vec<_>>()})).expect("lint JSON"))?;
    evidence.deterministic = GateCheck::recorded(
        if deterministic_failures.is_empty() {
            AcceptanceStatus::Pass
        } else {
            AcceptanceStatus::Fail
        },
        if deterministic_failures.is_empty() {
            "trusted checks, protected assets and deterministic lint passed".into()
        } else {
            deterministic_failures.join("; ")
        },
        lint_id,
        Some(tree),
    );
    *stage = EvaluationStage::TestReview;
    let review_repo = run_directory(state, context.run_id).join(format!("test-review-{tree}"));
    prepare_review_repository(&review_repo).map_err(at("preparing isolated domain test review"))?;
    let spec_text = String::from_utf8(context.specification.canonical_json()).expect("spec UTF-8");
    let domain = crate::test_review::run_test_review_with_context(
        &TestReviewRequest {
            review_repository: &review_repo,
            state_directory: state,
            input: TestReviewInput {
                spec_text: &spec_text,
                criteria: &context.specification.criteria,
                tests: &labelled,
                findings: &lint,
            },
            launch: request.test_reviewer,
            kaisa_executable: workflow.kaisa_executable,
            confine_agent: workflow.confine_agent && workflow.policy.policy().confine_agents,
            egress_hosts: workflow.egress_hosts,
            agent_writable: workflow.agent_writable,
            observed: false,
            reduced_mode: workflow.reduced_mode,
        },
        &AttemptContext {
            spec_run_id: Some(context.run_id),
            ..Default::default()
        },
    )?;
    let (review_status, review_detail) = if !successful(&domain.attempt) {
        (
            AcceptanceStatus::Inconclusive,
            "domain review process/transcript incomplete".into(),
        )
    } else {
        match &domain.review {
            Ok(review) if !review.blocks() => (
                AcceptanceStatus::Pass,
                "domain test review has no blocking finding".into(),
            ),
            Ok(review) => (AcceptanceStatus::Fail, review.blocking_reasons().join("; ")),
            Err(error) => (AcceptanceStatus::Inconclusive, error.to_string()),
        }
    };
    evidence.test_review = hook_receipt(
        context,
        "domain-test-review",
        review_status,
        &review_detail,
        Some(tree),
    )?;
    evidence.test_review.evidence_ids.push(artifact(
        state,
        context.run_id,
        &format!("test-review-transcript-{tree}.log"),
        transcript(state, &domain.attempt)?.as_bytes(),
    )?);
    *stage = EvaluationStage::StagedReview;
    let review_evidence = json!({"private_qa":results_json,"deterministic":evidence.deterministic,"traceability":trace,"domain_test_review":evidence.test_review});
    let decisions = json!({"spec_approval":evidence.approval,"policy":workflow.policy.canonical()});
    let developer_transcript = transcript(state, developer)?;
    let developer_answer = kaisa_agents::final_answer(&developer_transcript).unwrap_or_default();
    evidence.staged_review = request.staged_review.review(&StagedReviewInput {
        context: *context,
        workflow,
        diff: &kaisa_workspace::candidate_diff(context.repository, &context.base.commit, candidate)
            .map_err(at("reading staged review diff"))?,
        tests: &labelled,
        evidence: &review_evidence,
        decisions: &decisions,
        developer,
        developer_final_answer: &developer_answer,
    })?;
    *stage = EvaluationStage::Preservation;
    let ledger = EventStore::open(&state.join("kaisa.db"))
        .map_err(at("verifying preserved evidence ledger"))?;
    ledger
        .verify()
        .map_err(at("verifying specification ledger chain"))?;
    for suite in suites.iter() {
        suite
            .verified_tests()
            .map_err(at("verifying private tests after reviewers"))?;
    }
    evidence.preservation = hook_receipt(
        context,
        "preservation",
        AcceptanceStatus::Pass,
        "ledger chain, private manifests and final-tree evidence artifacts preserved",
        None,
    )?;
    let mut feedback = revision_feedback(&results)
        .iter()
        .filter(|failure| !failure.passed)
        .map(|failure| {
            format!(
                "criterion {} scenario {}: {}",
                failure.criterion_id, failure.scenario, failure.message
            )
        })
        .collect::<Vec<_>>();
    feedback.extend(public_deterministic_findings(deterministic_failures));
    if review_status == AcceptanceStatus::Fail {
        feedback.push(format!("domain test review: {review_detail}"));
    }
    if evidence.staged_review.status == AcceptanceStatus::Fail {
        feedback.push(format!("review: {}", evidence.staged_review.detail));
    }
    if evidence.traceability.status == AcceptanceStatus::Fail {
        feedback.extend(trace.gaps.clone());
    }
    Ok((trace, redact_revision_findings(feedback, &labelled)))
}
fn public_deterministic_findings(findings: Vec<String>) -> impl Iterator<Item = String> {
    findings
        .into_iter()
        .filter(|finding| !finding.contains("private/"))
}
// Reviewers see private tests, so their free-form findings need the same source boundary as
// QA diagnostics. Reject whole fields with source overlap instead of forwarding source fragments.
fn redact_revision_findings(findings: Vec<String>, tests: &[LabelledTest]) -> Vec<String> {
    const FIELD_CAP: usize = 256;
    const TOTAL_CAP: usize = 4096;
    const SHINGLE_WORDS: usize = 8;
    const EMBEDDED_SOURCE_CHARS: usize = 16;
    fn normalized(text: &str) -> String {
        text.chars()
            .filter(|character| !character.is_whitespace())
            .collect()
    }
    let sources: Vec<_> = tests
        .iter()
        .filter(|test| test.origin == TestOrigin::PrivateQa)
        .map(|test| normalized(&test.source))
        .collect();
    let mut remaining = TOTAL_CAP;
    findings
        .into_iter()
        .filter_map(|finding| {
            let normalized_finding = normalized(&finding);
            let words: Vec<_> = finding.split_whitespace().collect();
            let overlap = !normalized_finding.is_empty()
                && sources.iter().any(|source| {
                    source.contains(&normalized_finding)
                        || source
                            .as_bytes()
                            .windows(EMBEDDED_SOURCE_CHARS)
                            .any(|fragment| {
                                normalized_finding
                                    .as_bytes()
                                    .windows(EMBEDDED_SOURCE_CHARS)
                                    .any(|field| field == fragment)
                            })
                        || words
                            .windows(SHINGLE_WORDS)
                            .any(|words| source.contains(&normalized(&words.join(" "))))
                });
            let message = if overlap {
                "[private source redacted; see criterion and scenario failures]".into()
            } else if !kaisa_workspace::scan_for_secrets(&finding).is_empty() {
                "[finding omitted: secret detected]".into()
            } else {
                finding.chars().take(FIELD_CAP).collect::<String>()
            };
            if message.len() > remaining {
                None
            } else {
                remaining -= message.len();
                Some(message)
            }
        })
        .collect()
}

fn git_blob(repository: &Path, tree: &str, path: &str) -> std::io::Result<Vec<u8>> {
    let output = Command::new("git")
        .current_dir(repository)
        .args(["show", &format!("{tree}:{path}")])
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other("cannot read frozen blob"));
    }
    Ok(output.stdout)
}
fn tracked_paths(repository: &Path, tree: Option<&str>) -> std::io::Result<Vec<String>> {
    let mut command = Command::new("git");
    command.current_dir(repository);
    if let Some(tree) = tree {
        command.args(["ls-tree", "-r", "--name-only", "-z", tree]);
    } else {
        command.args(["ls-files", "-z"]);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(std::io::Error::other("cannot list frozen files"));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .filter_map(|path| String::from_utf8(path.to_vec()).ok())
        .collect())
}
fn spec_copies(
    repository: &Path,
    base: &Candidate,
    spec: &Specification,
) -> Result<Vec<(String, Vec<u8>)>, GoalError> {
    fn read_copies(
        repository: &Path,
        base: &Candidate,
        spec: &Specification,
    ) -> std::io::Result<Vec<(String, Vec<u8>)>> {
        use std::io::{BufRead, Read, Write};
        use std::process::Stdio;
        let tree = Command::new("git")
            .current_dir(repository)
            .args(["ls-tree", "-rz", "--full-tree", &base.tree])
            .output()?;
        if !tree.status.success() {
            return Err(std::io::Error::other("cannot walk specification tree"));
        }
        let mut blobs = Vec::new();
        for entry in tree
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let tab = entry
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(|| std::io::Error::other("invalid tree entry"))?;
            let path =
                String::from_utf8(entry[tab + 1..].to_vec()).map_err(std::io::Error::other)?;
            let header = std::str::from_utf8(&entry[..tab]).map_err(std::io::Error::other)?;
            let fields: Vec<_> = header.split_whitespace().collect();
            if fields.len() == 3 && fields[1] == "blob" {
                blobs.push((path, fields[2].to_owned()));
            }
        }
        if blobs.is_empty() {
            return Ok(Vec::new());
        }
        let mut child = Command::new("git")
            .current_dir(repository)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let requests = blobs
            .iter()
            .map(|(_, id)| format!("{id}\n"))
            .collect::<String>();
        // Drain stdout while feeding stdin so large trees cannot deadlock either pipe.
        let writer = std::thread::spawn(move || stdin.write_all(requests.as_bytes()));
        let mut cursor = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
        let scanned = (|| -> std::io::Result<Vec<(String, Vec<u8>)>> {
            let mut copies = Vec::new();
            for (path, id) in blobs {
                let mut header = String::new();
                cursor.read_line(&mut header)?;
                let fields: Vec<_> = header.split_whitespace().collect();
                if fields.len() != 3 || fields[0] != id || fields[1] != "blob" {
                    return Err(std::io::Error::other("invalid batch blob header"));
                }
                let size = fields[2].parse::<usize>().map_err(std::io::Error::other)?;
                let mut bytes = vec![0; size];
                cursor.read_exact(&mut bytes)?;
                let mut newline = [0];
                cursor.read_exact(&mut newline)?;
                if newline != *b"\n" {
                    return Err(std::io::Error::other("invalid batch blob terminator"));
                }
                if serde_json::from_slice::<Specification>(&bytes)
                    .is_ok_and(|copy| copy.id == spec.id)
                {
                    copies.push((path, bytes));
                }
            }
            Ok(copies)
        })();
        drop(cursor);
        if scanned.is_err() {
            let _ = child.kill();
        }
        let status = child.wait();
        let fed = writer
            .join()
            .map_err(|_| std::io::Error::other("batch input writer panicked"))?;
        let copies = scanned?;
        fed?;
        if !status?.success() {
            return Err(std::io::Error::other("cannot read specification blobs"));
        }
        Ok(copies)
    }
    read_copies(repository, base, spec).map_err(at("reading protected specification copies"))
}
fn test_sources(repository: &Path) -> Result<Vec<SourceFile>, GoalError> {
    let mut sources = Vec::new();
    for path in tracked_paths(repository, None).map_err(at("listing development test sources"))? {
        if kaisa_context::TestLanguage::of_path(&path).is_none() {
            continue;
        }
        let full = repository.join(&path);
        if !std::fs::symlink_metadata(&full)
            .map_err(at("checking test source type"))?
            .is_file()
        {
            return Err(at("checking development test source boundary")(
                std::io::Error::other(
                    "test source is not a regular file; deterministic lint is incomplete",
                ),
            ));
        }
        let source = std::fs::read_to_string(full)
            .map_err(at("reading development test source for lint"))?;
        sources.push(SourceFile { path, source });
    }
    Ok(sources)
}
fn finish(
    request: &SpecRequest<'_, '_>,
    run_id: String,
    evidence: AcceptanceEvidence,
    candidate: Option<Candidate>,
    attempts: Vec<GoalOutcome>,
    trace: Option<TraceabilityReport>,
) -> Result<SpecOutcome, GoalError> {
    let state = request.workflow.state_directory;
    let mut evidence = evidence;
    let receipts = [
        &evidence.approval,
        &evidence.spec_digest,
        &evidence.traceability,
        &evidence.deterministic,
        &evidence.private_qa,
        &evidence.test_review,
        &evidence.staged_review,
        &evidence.characterization,
        &evidence.preservation,
        &evidence.qa_isolation,
    ];
    let corrupt: Vec<_> = receipts
        .iter()
        .flat_map(|check| &check.evidence_ids)
        .filter(|id| {
            if let Some((name, digest)) = id.split_once(":blake3:") {
                let path = run_directory(state, &run_id).join(name);
                !std::fs::read(path)
                    .is_ok_and(|bytes| blake3::hash(&bytes).to_hex().as_str() == digest)
            } else {
                false
            }
        })
        .cloned()
        .collect();
    if !corrupt.is_empty() {
        let id = artifact(
            state,
            &run_id,
            "corrupt-evidence.json",
            &serde_json::to_vec(&corrupt).expect("corruption JSON"),
        )?;
        evidence.preservation = GateCheck::recorded(
            AcceptanceStatus::Fail,
            "preserved evidence digest mismatch",
            id,
            None,
        );
    }
    evidence.attempts = containment_receipts(state, &run_id)?;
    let decision = acceptance_gate(&evidence);
    let mut trace = trace.unwrap_or(TraceabilityReport {
        run_id: run_id.clone(),
        final_tree: evidence.final_tree.clone(),
        requirements: Vec::new(),
        gaps: vec!["specification could not be loaded and verified".into()],
    });
    if trace.final_tree != evidence.final_tree {
        trace.final_tree = evidence.final_tree.clone();
        trace
            .gaps
            .push("final candidate did not complete independent verification".into());
    }
    let mut markdown = format!("{}\n\n", decision.label());
    for line in &decision.lines {
        markdown.push_str(&format!(
            "- {:?}: {} — {} (evidence: {})\n",
            line.check.status,
            line.requirement,
            line.check.detail,
            line.check.evidence_ids.join(", ")
        ));
    }
    markdown.push('\n');
    markdown.push_str(&trace.markdown());
    artifact(
        state,
        &run_id,
        "traceability.json",
        &serde_json::to_vec_pretty(&trace).expect("trace JSON"),
    )?;
    // Record the pure PASS before publication; the final receipt below binds the report
    // and names only a branch that git actually created.
    event(
        state,
        &run_id,
        EventKind::GateAcceptance,
        json!({"decision":decision,"final_tree":evidence.final_tree,"spec_id":request.spec_id.as_str()}),
    )?;
    // This is the only delivery mutation. Internal attempt commits exist solely for recovery.
    let result_branch = if decision.status == AcceptanceStatus::Pass {
        let branch = format!("kaisa/result/{run_id}");
        let frozen = candidate.as_ref().expect("PASS requires frozen candidate");
        let status = Command::new("git")
            .current_dir(request.workflow.repository)
            .args(["branch", &branch, &frozen.commit])
            .status()
            .map_err(at("publishing accepted result branch"))?;
        if !status.success() {
            return Err(at("publishing accepted result branch")(
                std::io::Error::other("git branch failed"),
            ));
        }
        Some(branch)
    } else {
        None
    };
    if let Some(branch) = &result_branch {
        markdown.push_str(&format!(
            "\nResult: {branch}; retrieve with git merge --ff-only {branch}\n"
        ));
    }
    let report_id = artifact(state, &run_id, "report.md", markdown.as_bytes())?;
    let mut receipt = serde_json::to_value(&decision).expect("gate JSON");
    receipt["result_branch"] = json!(result_branch);
    artifact(
        state,
        &run_id,
        "acceptance.json",
        &serde_json::to_vec_pretty(&receipt).expect("receipt JSON"),
    )?;
    event(
        state,
        &run_id,
        EventKind::GateAcceptance,
        json!({"decision":decision,"final_tree":evidence.final_tree,"spec_id":request.spec_id.as_str(),"result_branch":result_branch,"report":"report.md","report_evidence_id":report_id}),
    )?;
    event(
        state,
        &run_id,
        EventKind::GateDecided,
        json!({"decision":match decision.status { AcceptanceStatus::Pass => "Deliver", AcceptanceStatus::Fail => "Refuse", AcceptanceStatus::Inconclusive => "NeedsPerson" },"acceptance_status":decision.status,"result_branch":result_branch,"goal":request.workflow.goal,"reason":format!("{}; {}", decision.label(), decision.lines.iter().filter(|line| line.check.status != AcceptanceStatus::Pass).map(|line| format!("{}: {}", line.requirement, line.check.detail)).collect::<Vec<_>>().join("; "))}),
    )?;
    Ok(SpecOutcome {
        report_path: run_directory(state, &run_id).join("report.md"),
        run_id,
        acceptance: decision,
        candidate,
        result_branch,
        attempts,
    })
}
/// Reads a preserved run receipt for `kaisa spec report <run>`.
/// # Errors
/// Rejects unsafe run identifiers and missing reports.
pub fn spec_report(state: &Path, run_id: &str) -> std::io::Result<String> {
    kaisa_domain::RunId::try_new(run_id).map_err(std::io::Error::other)?;
    let bytes = std::fs::read(run_directory(state, run_id).join("report.md"))?;
    let expected = format!("report.md:blake3:{}", blake3::hash(&bytes).to_hex());
    let ledger = EventStore::open(&state.join("kaisa.db")).map_err(std::io::Error::other)?;
    ledger.verify().map_err(std::io::Error::other)?;
    let verified = ledger
        .read_from(kaisa_domain::Sequence::FIRST)
        .map_err(std::io::Error::other)?
        .into_iter()
        .any(|event| {
            event.recorded.event.kind == EventKind::GateAcceptance
                && event.recorded.event.subject == run_id
                && serde_json::from_str::<Value>(&event.recorded.event.payload)
                    .is_ok_and(|payload| payload["report_evidence_id"] == expected)
        });
    if !verified {
        return Err(std::io::Error::other(
            "specification report has no matching digest-verified gate receipt",
        ));
    }
    String::from_utf8(bytes).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::redact_revision_findings;
    use crate::{LabelledTest, TestOrigin};
    #[test]
    fn batched_spec_copy_scan_preserves_extensionless_and_unusual_path_copies() {
        let root =
            std::env::temp_dir().join(format!("kaisa-spec-batch-{}", crate::ids::fresh_suffix()));
        crate::prepare_review_repository(&root).expect("test repository");
        let spec: kaisa_domain::Specification = serde_json::from_value(serde_json::json!({
            "id":"spec_batch", "version":1, "title":"contract", "requirements":[], "criteria":[], "ambiguities":[], "invariants":[], "error_behaviour":[]
        })).expect("spec");
        let unusual = if cfg!(windows) {
            "copy with spaces.json"
        } else {
            "copy\twith\nnewlines.json"
        };
        for path in ["contract", unusual] {
            std::fs::write(root.join(path), spec.canonical_json()).expect("copy");
        }
        std::fs::write(root.join("binary"), vec![0xff; 100_000]).expect("non-JSON blob");
        for args in [
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Kaisa",
                "-c",
                "user.email=kaisa@localhost.invalid",
                "commit",
                "-qm",
                "copies",
            ],
        ] {
            assert!(
                std::process::Command::new("git")
                    .current_dir(&root)
                    .args(args)
                    .status()
                    .expect("git")
                    .success()
            );
        }
        let base = kaisa_workspace::freeze_candidate(&root, "HEAD").expect("frozen tree");
        let copies = super::spec_copies(&root, &base, &spec).expect("batch scan");
        let _ = std::fs::remove_dir_all(root);
        assert_eq!(copies.len(), 2);
        for path in ["contract", unusual] {
            assert!(
                copies
                    .iter()
                    .any(|(name, bytes)| name == path && *bytes == spec.canonical_json()),
                "batch framing preserves {path:?} and its exact bytes"
            );
        }
    }
    #[test]
    fn deterministic_revision_feedback_drops_private_paths_before_source_redaction() {
        let findings = vec![
            "private/acceptance/empty.rs: a weak assertion".into(),
            "src/lib.rs: missing rejection".into(),
            "copy private/adversarial/edge.rs".into(),
        ];
        assert_eq!(
            super::public_deterministic_findings(findings).collect::<Vec<_>>(),
            vec!["src/lib.rs: missing rejection"],
            "private paths are removed even without source overlap"
        );
    }
    #[test]
    fn qa_isolation_keeps_middle_attempt_gaps_after_a_full_final_attempt() {
        use crate::qa::QaIsolation::{Full, Reduced};
        assert_eq!(
            super::worst_qa_isolation([Full, Reduced("unconfined developer".into()), Full]),
            Reduced("unconfined developer".into())
        );
        assert_eq!(
            super::worst_qa_isolation([Reduced("observed developer".into()), Full]),
            Reduced("observed developer".into())
        );
        assert_eq!(super::worst_qa_isolation([Full, Full]), Full);
        let combined = super::worst_qa_isolation([
            Reduced("observed developer".into()),
            Reduced("unconfined developer".into()),
        ])
        .label();
        assert!(
            combined.contains("observed developer") && combined.contains("unconfined developer"),
            "every distinct gap remains named"
        );
    }
    #[test]
    fn reduced_human_approval_requires_policy_and_keeps_its_assurance_label() {
        let stored = kaisa_ledger::spec::StoredSpec {
            specification: serde_json::from_value(serde_json::json!({
                "id":"spec_reduced", "version":1, "title":"contract",
                "requirements":[], "criteria":[], "ambiguities":[],
                "invariants":[], "error_behaviour":[]
            }))
            .expect("spec fixture"),
            digest: "digest".into(),
            frozen: true,
            approval: Some(kaisa_ledger::spec::ApprovalRecord {
                kind: "human-approved".into(),
                user: Some("person".into()),
                receipt: serde_json::json!({"attempts_unconfined_since_draft":true}),
            }),
        };
        let policy = kaisa_domain::Policy::personal_default();
        let check = super::approval_check(&stored, &policy.snapshot().expect("policy"), "receipt");
        assert_eq!(
            check.status,
            crate::acceptance::AcceptanceStatus::Inconclusive,
            "reduced human approval requires explicit policy acceptance"
        );
        assert!(check.detail.contains("human-approved (reduced assurance)"));
        let mut accepted = policy;
        accepted.accept_reduced_assurance_approval = true;
        let check = super::approval_check(
            &stored,
            &accepted.snapshot().expect("accepted policy"),
            "receipt",
        );
        assert_eq!(check.status, crate::acceptance::AcceptanceStatus::Pass);
        assert!(check.detail.contains("human-approved (reduced assurance)"));
        let mut evidence = super::missing_evidence();
        evidence.approval = check;
        evidence.reduced_assurance_approval = true;
        let decision = crate::acceptance::acceptance_gate(&evidence);
        assert!(
            decision
                .label()
                .contains("human-approved (reduced assurance)")
        );
        assert_eq!(
            serde_json::to_value(&decision).expect("receipt")["reduced_assurance_approval"],
            true
        );
    }
    #[test]
    fn review_findings_cannot_copy_private_test_source_into_revision_feedback() {
        let private_source = "#[test] fn an_empty_input_is_rejected() { assert_eq!(accept(EMPTY_INPUT), REJECTED); }";
        let tests = [LabelledTest {
            origin: TestOrigin::PrivateQa,
            path: "private/empty.rs".into(),
            source: private_source.into(),
        }];
        let findings = vec![
            format!("Please implement this exact private case: {private_source}"),
            "AC-1: rejection is missing".into(),
        ];
        let sanitized = redact_revision_findings(findings, &tests).join("\n");
        assert!(
            !sanitized.contains(private_source) && !sanitized.contains("assert_eq!"),
            "source-bearing semantic review findings are redacted before revision"
        );
        assert!(
            sanitized.contains("AC-1: rejection is missing"),
            "observable domain guidance survives source redaction"
        );
    }
    #[test]
    fn a_nontext_test_source_cannot_silently_escape_deterministic_lint() {
        const INVALID_UTF8_SOURCE: &[u8] = &[0xff];
        let root =
            std::env::temp_dir().join(format!("kaisa-spec-source-{}", crate::ids::fresh_suffix()));
        crate::prepare_review_repository(&root).expect("fresh test repository");
        std::fs::write(root.join("contract.rs"), INVALID_UTF8_SOURCE).expect("nontext fixture");
        let status = std::process::Command::new("git")
            .current_dir(&root)
            .args(["add", "contract.rs"])
            .status()
            .expect("track fixture");
        assert!(
            status.success(),
            "fixture is tracked for the same scan as a candidate"
        );
        let result = super::test_sources(&root);
        let _ = std::fs::remove_dir_all(root);
        assert!(
            result.is_err(),
            "unreadable test source makes lint incomplete instead of disappearing"
        );
    }
}
