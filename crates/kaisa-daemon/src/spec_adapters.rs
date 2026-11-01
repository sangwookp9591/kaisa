//! Production adapters keep the workflow hooks small and the evidence authorities explicit.
use std::collections::BTreeMap;
use std::sync::Mutex;

use kaisa_domain::{Admission, EventKind, Role, Sequence, WriteSet, admit_on, codes};
use kaisa_ledger::EventStore;
use kaisa_sys::ProcessSpec;
use serde_json::{Value, json};

use crate::GoalRequest;
use crate::acceptance::{AcceptanceStatus, GateCheck};
use crate::characterize::{Baseline, CharacterizationInput, LegacyCharacterizer};
use crate::goal::{GoalError, at};
use crate::qa::QaToolchain;
use crate::spec_flow::{
    Characterize, HookContext, StagedReview, StagedReviewInput, artifact, event, hook_receipt,
};
use crate::staged_review::{EvidenceKind, ReviewEvidence, run_staged_review_with_context};

/// Runs the staged agent protocol; only ledger-authenticated execution can close a blocker.
pub struct AgentStagedReview;
impl StagedReview for AgentStagedReview {
    fn review(&self, input: &StagedReviewInput<'_, '_>) -> Result<GateCheck, GoalError> {
        let context = &input.context;
        let candidate = context.candidate.expect("review follows candidate freeze");
        let ledger = EventStore::open(&context.state_directory.join("kaisa.db"))
            .map_err(at("opening staged review ledger"))?;
        ledger
            .verify()
            .map_err(at("verifying staged review ledger"))?;
        let events = ledger
            .read_from(Sequence::FIRST)
            .map_err(at("reading review history"))?;
        let mut evidence = Vec::new();
        let mut history = Vec::new();
        let mut interventions = Vec::new();
        for entry in events {
            let recorded = entry.recorded;
            let payload: Value = serde_json::from_str(&recorded.event.payload)
                .map_err(at("reading review event"))?;
            if recorded.event.kind == EventKind::SpecApproved
                && recorded.event.subject == context.specification.id.as_str()
                && payload["version"] == context.specification.version
            {
                interventions.push(payload.clone());
            }
            if payload["run_id"] != context.run_id {
                continue;
            }
            if matches!(
                recorded.event.kind,
                EventKind::RevisionStarted
                    | EventKind::GateAcceptance
                    | EventKind::AttemptAdmitted
                    | EventKind::CandidateFrozen
            ) {
                history.push(json!({"sequence":recorded.sequence.value(),"kind":recorded.event.kind.code(),"payload":payload}));
            }
            if payload["candidate_tree"] != candidate.tree {
                continue;
            }
            let (kind, passed) = match recorded.event.kind {
                EventKind::CheckEvidence => (
                    EvidenceKind::Check,
                    payload["passed"] == true
                        && payload["confined"] == true
                        && payload["weakened_tests"]
                            .as_array()
                            .is_some_and(Vec::is_empty)
                        && payload["definitions_changed"]
                            .as_array()
                            .is_some_and(Vec::is_empty),
                ),
                EventKind::QaResult => (
                    EvidenceKind::Qa,
                    payload["passed"] == true
                        && payload["confined"] == true
                        && payload["source_unchanged"] == true
                        && payload["private_test_unchanged"] == true
                        && payload["positive_evidence"] == true
                        && payload["blocking_findings"]
                            .as_array()
                            .is_some_and(Vec::is_empty),
                ),
                _ => continue,
            };
            evidence.push(ReviewEvidence {
                id: format!("ledger:{}", recorded.sequence.value()),
                kind,
                candidate_tree: payload["candidate_tree"]
                    .as_str()
                    .expect("filtered candidate tree")
                    .to_owned(),
                passed,
            });
        }
        // Explanations have known ids, but can never act as execution evidence.
        evidence.push(ReviewEvidence {
            id: "developer-final-answer".into(),
            kind: EvidenceKind::Explanation,
            candidate_tree: candidate.tree.clone(),
            passed: false,
        });
        let mut diff = input.diff.to_owned();
        let mut end = diff.len().min(24 * 1024);
        while !diff.is_char_boundary(end) {
            end -= 1;
        }
        diff.truncate(end);
        let bundle = crate::staged_review::StagedReviewInput {
            candidate_tree: candidate.tree.clone(), bounded_diff: diff,
            tests_and_spec: json!({"specification":context.specification,"tests":input.tests.iter().map(|test| json!({"origin":format!("{:?}",test.origin),"path":test.path,"source":test.source})).collect::<Vec<_>>(),"recorded_results":input.evidence}).to_string(),
            developer_final_answer: input.developer_final_answer.into(),
            revision_history: json!(history).to_string(),
            human_interventions: json!({"approvals":interventions,"decisions":input.decisions}).to_string(),
            evaluation_context: json!({"invariants":context.specification.invariants,"error_behaviour":context.specification.error_behaviour}).to_string(),
            evidence,
        };
        let workflow = input.workflow;
        let no_paths = WriteSet::default();
        let admission_time = crate::ids::now_ms();
        let outcome = run_staged_review_with_context(
            GoalRequest {
                repository: context.repository,
                state_directory: context.state_directory,
                goal: "staged review",
                base: &candidate.commit,
                write_set: &no_paths,
                role: Role::Reviewer,
                launch: workflow.reviewer,
                kaisa_executable: workflow.kaisa_executable,
                confine_agent: workflow.confine_agent && workflow.policy.policy().confine_agents,
                egress_hosts: workflow.egress_hosts,
                agent_writable: workflow.agent_writable,
                local_model_port: None,
                observed: false,
                reduced_mode: workflow.reduced_mode,
            },
            &bundle,
            &crate::goal::AttemptContext {
                spec_run_id: Some(context.run_id),
                ..Default::default()
            },
        )?;
        let (status, detail) = staged_status(&outcome, &candidate.tree, admission_time);
        let stages = outcome.review.as_ref().ok().map(|review| review.stages());
        event(
            context.state_directory,
            context.run_id,
            EventKind::ReviewVerdict,
            json!({"candidate_tree":candidate.tree,"attempt":outcome.attempt.attempt.as_str(),"status":status,"detail":detail,"stages":stages,"containment_missing":outcome.attempt.missing.iter().map(|c| c.code()).collect::<Vec<_>>(),"admitted_on":outcome.attempt.admitted_on.code()}),
        )?;
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
            &format!("staged-review-context-{}.json", candidate.tree),
            &serde_json::to_vec(&crate::staged_review::context_bundle(&bundle))
                .expect("review context JSON"),
        )?);
        let transcript = std::fs::read(
            context
                .state_directory
                .join("attempts")
                .join(outcome.attempt.attempt.as_str())
                .join("transcript.log"),
        )
        .map_err(at("preserving staged review transcript"))?;
        check.evidence_ids.push(artifact(
            context.state_directory,
            context.run_id,
            &format!("review-transcript-{}.log", candidate.tree),
            &transcript,
        )?);
        Ok(check)
    }
}

fn staged_status(
    outcome: &crate::staged_review::StagedReviewOutcome,
    tree: &str,
    admission_time: u64,
) -> (AcceptanceStatus, String) {
    match &outcome.review {
        Err(reason) => (
            AcceptanceStatus::Inconclusive,
            format!("staged review unavailable: {reason}"),
        ),
        Ok(review) if !review.verdict().approves(tree) => (
            AcceptanceStatus::Fail,
            format!(
                "staged review blocked: {}",
                review
                    .verdict()
                    .findings
                    .iter()
                    .map(|f| f.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        ),

        Ok(_)
            if outcome.attempt.confined != outcome.attempt.missing.is_empty()
                || matches!(
                    admit_on(
                        &outcome.attempt.missing,
                        &outcome.attempt.admitted_on,
                        admission_time
                    ),
                    Admission::Refused { .. }
                ) =>
        {
            (
                AcceptanceStatus::Inconclusive,
                format!(
                    "staged reviewer containment reduction not admitted: {}",
                    codes(&outcome.attempt.missing)
                ),
            )
        }
        Ok(_) if !outcome.attempt.missing.is_empty() => (
            AcceptanceStatus::Pass,
            format!(
                "PASS (reduced: {}); staged review approved with no unresolved blocking finding; admitted on {}",
                codes(&outcome.attempt.missing),
                outcome.attempt.admitted_on.code()
            ),
        ),
        Ok(_) => (
            AcceptanceStatus::Pass,
            "staged review approved with no unresolved blocking finding".into(),
        ),
    }
}

/// Owns opaque, ledger-sequenced WP6 tokens across all revisions of a specification run.
pub struct LedgerCharacterize<'a> {
    author: &'a ProcessSpec,
    toolchain: &'a QaToolchain,
    baselines: Mutex<BTreeMap<String, Baseline>>,
}
impl<'a> LedgerCharacterize<'a> {
    /// Uses a base-only author and a trusted toolchain; no candidate context is supplied.
    pub fn new(author: &'a ProcessSpec, toolchain: &'a QaToolchain) -> Self {
        Self {
            author,
            toolchain,
            baselines: Mutex::new(BTreeMap::new()),
        }
    }
    fn service<'b>(&'b self, context: &'b HookContext<'_>) -> LegacyCharacterizer<'b> {
        LegacyCharacterizer {
            repository: context.repository,
            base: &context.base.commit,
            state: context.state_directory,
            run_id: context.run_id,
            author: self.author,
            toolchain: self.toolchain,
        }
    }
}
impl Characterize for LedgerCharacterize<'_> {
    fn baseline(&self, context: &HookContext<'_>) -> Result<GateCheck, GoalError> {
        let spec = serde_json::to_value(context.specification).expect("spec JSON");
        // Specification currently has no path-hint fields; WP6 selects public entry points.
        let baseline = self.service(context).baseline(&CharacterizationInput {
            frozen_spec: &spec,
            spec_paths: &[],
            agent_paths: &[],
        })?;
        let mut receipt = hook_receipt(
            context,
            "characterization-baseline",
            if baseline.questions().is_empty() {
                AcceptanceStatus::Pass
            } else {
                AcceptanceStatus::Inconclusive
            },
            &format!(
                "baseline recorded before development; {} legacy areas; {}",
                baseline.areas().len(),
                baseline.questions().join("; ")
            ),
            None,
        )?;
        receipt
            .evidence_ids
            .push(format!("ledger:{}", baseline.event_sequence().value()));
        self.baselines
            .lock()
            .expect("baseline mutex")
            .insert(context.run_id.into(), baseline);
        Ok(receipt)
    }
    fn evaluate(
        &self,
        context: &HookContext<'_>,
        receipt: &GateCheck,
    ) -> Result<GateCheck, GoalError> {
        let candidate = context
            .candidate
            .expect("comparison follows candidate freeze");
        let baselines = self.baselines.lock().expect("baseline mutex");
        let Some(baseline) = baselines.get(context.run_id).filter(|baseline| {
            receipt.status == AcceptanceStatus::Pass
                && receipt
                    .evidence_ids
                    .contains(&format!("ledger:{}", baseline.event_sequence().value()))
        }) else {
            return hook_receipt(
                context,
                "characterization",
                AcceptanceStatus::Fail,
                "characterization comparison refused: missing or mismatched baseline token",
                Some(&candidate.tree),
            );
        };
        let ledger = EventStore::open(&context.state_directory.join("kaisa.db"))
            .map_err(at("opening characterization comparison ledger"))?;
        let (status, detail) = match self.service(context).compare(baseline, candidate, &ledger) {
            Err(reason) => (AcceptanceStatus::Fail, reason.to_string()),
            Ok(findings) if findings.is_empty() && baseline.areas().is_empty() => (
                AcceptanceStatus::Pass,
                "no untested legacy area detected; characterization not applicable".into(),
            ),
            Ok(findings) if findings.is_empty() => (
                AcceptanceStatus::Pass,
                "legacy characterization honoured on final tree".into(),
            ),
            Ok(findings) => (
                AcceptanceStatus::Fail,
                findings
                    .iter()
                    .map(|f| f.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
        };
        hook_receipt(
            context,
            "characterization",
            status,
            &detail,
            Some(&candidate.tree),
        )
    }
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{AttemptId, ContainmentCapability, ReducedModeBasis, ReducedModeDecision};
    use kaisa_sys::ProcessExit;
    use serde_json::json;

    use super::staged_status;
    use crate::GoalOutcome;
    use crate::acceptance::AcceptanceStatus;
    use crate::staged_review::{StagedReviewInput, StagedReviewOutcome, parse_staged_review};

    const ADMISSION_TIME: u64 = 1_000;
    fn approval(
        missing: Vec<ContainmentCapability>,
        basis: ReducedModeBasis,
    ) -> StagedReviewOutcome {
        let input = StagedReviewInput {
            candidate_tree: "tree".into(),
            ..Default::default()
        };
        let answer = json!({"stages":[
            {"stage":"stage1_diff_concerns","concerns":[]},
            {"stage":"stage2_tests_and_expected_behaviour","assessment":"reviewed"},
            {"stage":"stage3_decisions_and_history","assessment":"reviewed"},
            {"stage":"stage4_evaluation","architecture":"reviewed","domain_invariants":"reviewed","error_handling":"reviewed","security":"reviewed","external_integrations":"reviewed"},
            {"stage":"stage5_judgment_changes","changes":[]}
        ],"candidate_tree":"tree","approve":true,"findings":[]});
        StagedReviewOutcome {
            review: parse_staged_review(&answer.to_string(), &input),
            attempt: GoalOutcome {
                attempt: AttemptId::try_new("att_staged").expect("attempt id"),
                exit: Some(ProcessExit::Code(0)),
                truncated: false,
                permission_denials: vec![],
                branch: "review".into(),
                commit: None,
                kept_back: vec![],
                context_pack: String::new(),
                confined: missing.is_empty(),
                missing,
                admitted_on: basis,
                observed: false,
            },
        }
    }
    #[test]
    fn fully_confined_staged_approval_passes() {
        let outcome = approval(vec![], ReducedModeBasis::None);
        let (status, detail) = staged_status(&outcome, "tree", ADMISSION_TIME);
        assert_eq!(status, AcceptanceStatus::Pass);
        assert!(!detail.contains("reduced"));
    }
    #[test]
    fn an_admitted_descendant_limit_passes_with_its_reduction_label() {
        let outcome = approval(
            vec![ContainmentCapability::DescendantContainment],
            ReducedModeBasis::Decision(ReducedModeDecision {
                accepts_missing: vec![ContainmentCapability::DescendantContainment],
                expires_at_ms: ADMISSION_TIME + 1,
            }),
        );
        let (status, detail) = staged_status(&outcome, "tree", ADMISSION_TIME);
        assert_eq!(status, AcceptanceStatus::Pass);
        assert!(
            detail.contains("PASS (reduced: descendants)")
                && detail.contains("admitted on decision")
        );
    }
    #[test]
    fn an_unadmitted_reduction_is_inconclusive_despite_approval() {
        let outcome = approval(
            vec![ContainmentCapability::WriteConfinement],
            ReducedModeBasis::None,
        );
        let (status, detail) = staged_status(&outcome, "tree", ADMISSION_TIME);
        assert_eq!(status, AcceptanceStatus::Inconclusive);
        assert!(detail.contains("not admitted: write"));
    }
}
