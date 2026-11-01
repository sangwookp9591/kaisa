//! Inspectable requirement → criterion → final-tree private test → development-test matrix.
use kaisa_domain::Specification;
use kaisa_domain::spec::TestMapping;
use kaisa_workspace::QaKind;
use serde::{Deserialize, Serialize};

use crate::qa::QaResult;

/// A private scenario result on the content under consideration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivateTestTrace {
    /// Independent scenario identity.
    pub test_id: String,
    /// Suite purpose; only acceptance scenarios fill acceptance coverage.
    pub kind: String,
    /// Positive harness evidence on the final tree.
    pub passed: bool,
    /// Exact content exercised.
    pub candidate_tree: String,
    /// Preserved output receipt.
    pub output_digest: String,
}
/// One criterion and its test mappings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionTrace {
    /// Frozen criterion id.
    pub criterion_id: String,
    /// Every private scenario mapped to this criterion, including failures and stale results.
    pub private_tests: Vec<PrivateTestTrace>,
    /// Development test declarations verified by deterministic lint.
    pub development_tests: Vec<String>,
}
/// One original requirement and its acceptance coverage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementTrace {
    /// Original requirement id, retained even when coverage is missing.
    pub requirement_id: String,
    /// Verbatim request text.
    pub text: String,
    /// Criteria linking to this requirement.
    pub criteria: Vec<CriterionTrace>,
}
/// Durable traceability artifact stored alongside the gate receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceabilityReport {
    /// Run identity.
    pub run_id: String,
    /// Final tree, absent before development.
    pub final_tree: Option<String>,
    /// Every original requirement.
    pub requirements: Vec<RequirementTrace>,
    /// Gaps use original requirement/criterion identifiers.
    pub gaps: Vec<String>,
}
impl TraceabilityReport {
    /// Renders the same evidence as the JSON artifact, without private test source.
    pub fn markdown(&self) -> String {
        let mut text = format!(
            "# Traceability: {}\n\nFinal tree: {}\n\n| Requirement | Criterion | Private tests (final-tree result) | Development tests (lint-mapped) |\n|---|---|---|---|\n",
            self.run_id,
            self.final_tree.as_deref().unwrap_or("not produced")
        );
        for requirement in &self.requirements {
            if requirement.criteria.is_empty() {
                text.push_str(&format!(
                    "| {} | MISSING | — | — |\n",
                    cell(&requirement.requirement_id)
                ));
            }
            for criterion in &requirement.criteria {
                let tests: Vec<_> = criterion
                    .private_tests
                    .iter()
                    .map(|test| {
                        format!(
                            "{} [{}]: {} ({})",
                            test.test_id,
                            test.kind,
                            if test.passed { "PASS" } else { "FAIL/stale" },
                            test.candidate_tree
                        )
                    })
                    .collect();
                text.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    cell(&requirement.requirement_id),
                    cell(&criterion.criterion_id),
                    cell(&tests.join("; ")),
                    cell(&criterion.development_tests.join("; "))
                ));
            }
        }
        for gap in &self.gaps {
            text.push_str(&format!("\n- Gap: {}", cell(gap)));
        }
        text.push('\n');
        text
    }
}
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\r', '\n'], " ")
}

/// Builds coverage from actual results; adversarial tests cannot fill an acceptance gap.
pub fn traceability_report(
    run_id: &str,
    spec: &Specification,
    final_tree: Option<&str>,
    private: &[QaResult],
    development: &[TestMapping],
) -> TraceabilityReport {
    let mut gaps: Vec<String> = spec
        .validate()
        .iter()
        .map(|finding| format!("{finding:?}"))
        .collect();
    for criterion in &spec.criteria {
        if !private.iter().any(|test| {
            test.kind == QaKind::Acceptance
                && test.criterion_ids.contains(&criterion.id)
                && Some(test.candidate_tree.as_str()) == final_tree
        }) {
            gaps.push(format!(
                "{} has no private acceptance test on the final tree",
                criterion.id
            ));
        }
    }
    let requirements = spec
        .requirements
        .iter()
        .map(|requirement| RequirementTrace {
            requirement_id: requirement.id.clone(),
            text: requirement.text.clone(),
            criteria: spec
                .criteria
                .iter()
                .filter(|criterion| criterion.requirement_ids.contains(&requirement.id))
                .map(|criterion| CriterionTrace {
                    criterion_id: criterion.id.clone(),
                    private_tests: private
                        .iter()
                        .filter(|test| test.criterion_ids.contains(&criterion.id))
                        .map(|test| PrivateTestTrace {
                            test_id: test.test_id.clone(),
                            kind: test.kind.code().into(),
                            passed: test.passed && Some(test.candidate_tree.as_str()) == final_tree,
                            candidate_tree: test.candidate_tree.clone(),
                            output_digest: test.output_digest.clone(),
                        })
                        .collect(),
                    development_tests: development
                        .iter()
                        .filter(|mapping| mapping.criterion_ids.contains(&criterion.id))
                        .map(|mapping| mapping.test_id.clone())
                        .collect(),
                })
                .collect(),
        })
        .collect();
    TraceabilityReport {
        run_id: run_id.into(),
        final_tree: final_tree.map(str::to_owned),
        requirements,
        gaps,
    }
}
