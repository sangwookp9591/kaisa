//! The orchestrator's acceptance decision. Agents supply evidence, never authority to deliver.

use serde::{Deserialize, Serialize};

/// A recorded requirement's result, or the aggregate decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AcceptanceStatus {
    /// Every required check has positive evidence.
    Pass,
    /// At least one check has evidence of failure.
    Fail,
    /// A required check is missing or could not run.
    Inconclusive,
}
impl AcceptanceStatus {
    /// CI contract for specification builds.
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Fail => 4,
            Self::Inconclusive => 3,
        }
    }
}

/// A trusted receipt for one requirement. Unknown evidence defaults to inconclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateCheck {
    /// Positive result, negative evidence, or missing evidence.
    pub status: AcceptanceStatus,
    /// Human-readable observation, including any reduced guarantees.
    pub detail: String,
    /// Ledger sequence IDs or content-addressed artifact IDs.
    pub evidence_ids: Vec<String>,
    /// Candidate content to which the observation applies, when tree-dependent.
    pub candidate_tree: Option<String>,
    /// Explicitly admitted containment gaps retained through delivery and report labels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub admitted_reductions: Vec<String>,
}
impl GateCheck {
    /// A check that has not produced usable evidence.
    pub fn missing(detail: impl Into<String>) -> Self {
        Self {
            status: AcceptanceStatus::Inconclusive,
            detail: detail.into(),
            evidence_ids: Vec::new(),
            candidate_tree: None,
            admitted_reductions: Vec::new(),
        }
    }
    /// Constructs a trusted observation bound to preserved evidence.
    pub fn recorded(
        status: AcceptanceStatus,
        detail: impl Into<String>,
        evidence: impl Into<String>,
        tree: Option<&str>,
    ) -> Self {
        Self {
            status,
            detail: detail.into(),
            evidence_ids: vec![evidence.into()],
            candidate_tree: tree.map(str::to_owned),
            admitted_reductions: Vec::new(),
        }
    }
}

/// The complete recorded evidence considered by §5, kept separate from any agent verdict.
#[derive(Debug, Clone)]
pub struct AcceptanceEvidence {
    /// Final content being considered for delivery.
    pub final_tree: Option<String>,
    /// Whether approval was policy-based, never a claim of human confirmation.
    pub provisional: bool,
    /// Human approval followed unconfined attempts and retains its reduced assurance.
    pub reduced_assurance_approval: bool,
    /// Verified approval/freeze receipt under the run policy.
    pub approval: GateCheck,
    /// Approved bytes and receipt remained unchanged after every agent.
    pub spec_digest: GateCheck,
    /// Every requirement and criterion has final-tree acceptance coverage.
    pub traceability: GateCheck,
    /// Trusted checks, protected assets and deterministic test lint.
    pub deterministic: GateCheck,
    /// Both independent private suites passed on the final content.
    pub private_qa: GateCheck,
    /// Domain test review has no blocking finding.
    pub test_review: GateCheck,
    /// Context review approved with no unresolved blocking finding.
    pub staged_review: GateCheck,
    /// Baseline evidence and permitted corrections, or explicit non-applicability.
    pub characterization: GateCheck,
    /// All referenced evidence is preserved and digest-verified.
    pub preservation: GateCheck,
    /// Full isolation, or explicit policy acceptance of named gaps.
    pub qa_isolation: GateCheck,
}

/// One line in the acceptance ledger record and receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceLine {
    /// Stable requirement name.
    pub requirement: String,
    /// Recorded observation after freshness verification.
    pub check: GateCheck,
}
/// Immutable gate receipt, suitable for ledger and report serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceDecision {
    /// PASS, FAIL, or INCONCLUSIVE. Failure always dominates approval or missing evidence.
    pub status: AcceptanceStatus,
    /// Explicitly retained provenance label.
    pub provisional: bool,
    /// Retains WP1 approval assurance in receipts and every display surface.
    #[serde(default)]
    pub reduced_assurance_approval: bool,
    /// All §5 requirements, including successful lines.
    pub lines: Vec<AcceptanceLine>,
}
impl AcceptanceDecision {
    /// Display status without manufacturing human approval.
    pub fn label(&self) -> String {
        let status = format!("{:?}", self.status).to_uppercase();
        let mut label = if self.provisional {
            format!("{status} (PROVISIONAL SPEC)")
        } else if self.reduced_assurance_approval {
            format!("{status} — human-approved (reduced assurance)")
        } else {
            status
        };
        let reductions: std::collections::BTreeSet<_> = self
            .lines
            .iter()
            .flat_map(|line| &line.check.admitted_reductions)
            .collect();
        if !reductions.is_empty() {
            label.push_str(&format!(
                " (reduced: {})",
                reductions
                    .into_iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        label
    }
}

/// Pure gate: a reviewer cannot repair a failing deterministic line or stale QA evidence.
pub fn acceptance_gate(evidence: &AcceptanceEvidence) -> AcceptanceDecision {
    let checks = [
        ("spec approval status", &evidence.approval, false),
        ("spec digest unchanged", &evidence.spec_digest, false),
        ("traceability complete", &evidence.traceability, true),
        ("deterministic checks", &evidence.deterministic, true),
        (
            "private QA all passed on FINAL tree",
            &evidence.private_qa,
            true,
        ),
        (
            "domain test review no blocking",
            &evidence.test_review,
            true,
        ),
        ("staged review approved", &evidence.staged_review, true),
        (
            "characterization honoured",
            &evidence.characterization,
            true,
        ),
        ("evidence preserved", &evidence.preservation, false),
        (
            "qa_isolation full or policy-accepted",
            &evidence.qa_isolation,
            true,
        ),
    ];
    let lines: Vec<_> = checks
        .into_iter()
        .map(|(name, receipt, tree_dependent)| {
            let mut check = receipt.clone();
            if check.status == AcceptanceStatus::Pass {
                if check.evidence_ids.is_empty() {
                    check.status = AcceptanceStatus::Inconclusive;
                    check.detail.push_str("; missing preserved evidence IDs");
                }
                if tree_dependent
                    && (evidence.final_tree.is_none()
                        || check.candidate_tree != evidence.final_tree)
                {
                    check.status = AcceptanceStatus::Fail;
                    check
                        .detail
                        .push_str("; evidence does not apply to the FINAL tree");
                }
            }
            AcceptanceLine {
                requirement: name.into(),
                check,
            }
        })
        .collect();
    let status = if lines
        .iter()
        .any(|line| line.check.status == AcceptanceStatus::Fail)
    {
        AcceptanceStatus::Fail
    } else if lines
        .iter()
        .any(|line| line.check.status == AcceptanceStatus::Inconclusive)
    {
        AcceptanceStatus::Inconclusive
    } else {
        AcceptanceStatus::Pass
    };
    AcceptanceDecision {
        status,
        provisional: evidence.provisional,
        reduced_assurance_approval: evidence.reduced_assurance_approval,
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::{AcceptanceEvidence, AcceptanceStatus, GateCheck, acceptance_gate};
    const ACCEPTANCE_REQUIREMENTS: usize = 10;
    fn complete_evidence() -> AcceptanceEvidence {
        let receipt = GateCheck::recorded(
            AcceptanceStatus::Pass,
            "verified",
            "ledger:#1",
            Some("final-tree"),
        );
        AcceptanceEvidence {
            final_tree: Some("final-tree".into()),
            provisional: false,
            reduced_assurance_approval: false,
            approval: receipt.clone(),
            spec_digest: receipt.clone(),
            traceability: receipt.clone(),
            deterministic: receipt.clone(),
            private_qa: receipt.clone(),
            test_review: receipt.clone(),
            staged_review: receipt.clone(),
            characterization: receipt.clone(),
            preservation: receipt.clone(),
            qa_isolation: receipt,
        }
    }
    #[test]
    fn reviewer_approval_cannot_override_a_failed_deterministic_requirement() {
        for requirement in [
            "weakened existing test",
            "blocking lint",
            "traceability gap",
        ] {
            let mut evidence = complete_evidence();
            evidence.deterministic.status = AcceptanceStatus::Fail;
            evidence.deterministic.detail = requirement.into();
            assert_eq!(
                acceptance_gate(&evidence).status,
                AcceptanceStatus::Fail,
                "approving review cannot override {requirement}"
            );
        }
    }
    #[test]
    fn private_qa_from_an_earlier_tree_cannot_accept_the_final_candidate() {
        let mut evidence = complete_evidence();
        evidence.private_qa.candidate_tree = Some("earlier-tree".into());
        assert_eq!(
            acceptance_gate(&evidence).status,
            AcceptanceStatus::Fail,
            "all QA must be rerun after revision"
        );
    }
    #[test]
    fn missing_isolation_acceptance_waits_and_failed_qa_still_dominates() {
        let mut evidence = complete_evidence();
        evidence.qa_isolation = GateCheck::missing("reduced without policy");
        assert_eq!(
            acceptance_gate(&evidence).status,
            AcceptanceStatus::Inconclusive,
            "a containment gap is never silently accepted"
        );
        evidence.private_qa.status = AcceptanceStatus::Fail;
        assert_eq!(
            acceptance_gate(&evidence).status,
            AcceptanceStatus::Fail,
            "independent failure dominates missing isolation evidence"
        );
    }
    #[test]
    fn provisional_acceptance_keeps_its_label_and_all_requirement_lines() {
        let mut evidence = complete_evidence();
        evidence.provisional = true;
        let decision = acceptance_gate(&evidence);
        assert_eq!(
            decision.status,
            AcceptanceStatus::Pass,
            "allowed provisional approval can pass"
        );
        assert!(
            decision.label().contains("PROVISIONAL") && !decision.label().contains("HUMAN"),
            "provisional receipt never claims human approval"
        );
        assert_eq!(
            decision.lines.len(),
            ACCEPTANCE_REQUIREMENTS,
            "every design requirement has a line"
        );
    }
}
