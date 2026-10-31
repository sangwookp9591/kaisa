//! The delivery gate (T22a, ADR-0030): the one place that decides whether a candidate reaches
//! the user. Nothing is delivered without fresh, passing evidence and an approving verdict, both
//! bound to this candidate's tree.

use kaisa_domain::{Finding, ReviewVerdict};
use kaisa_workspace::Candidate;

use crate::verify::Evidence;

/// What happens to a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Delivered; `reduced` is true if a check ran without OS confinement.
    Deliver {
        /// Whether any evidence came from an unconfined check.
        reduced: bool,
    },
    /// Sent back for a revision with what it must fix.
    Revise(Vec<RevisionReason>),
    /// A person must decide, for the given reason.
    NeedsPerson(String),
    /// Never delivered as it stands, for the given reason.
    Refuse(String),
}

/// Why a revision is needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionReason {
    /// A trusted check failed.
    FailingCheck(Vec<String>),
    /// A reviewer found something blocking.
    Finding(Finding),
}

/// Decides what happens to `candidate` given its `evidence` and its review `verdict` (`None`
/// when no valid verdict exists: a malformed verdict counts as none).
#[must_use]
pub fn decide_delivery(
    candidate: &Candidate,
    evidence: &[Evidence],
    verdict: Option<&ReviewVerdict>,
) -> Delivery {
    if evidence.is_empty() {
        return Delivery::Refuse("no trusted check produced evidence".to_owned());
    }
    if let Some(reason) = evidence.iter().find_map(Evidence::bounded_failure_reason) {
        return Delivery::Refuse(reason);
    }
    if evidence.iter().any(|piece| !piece.applies_to(candidate)) {
        return Delivery::Refuse("evidence was produced on other content".to_owned());
    }
    let weakened: Vec<_> = evidence
        .iter()
        .filter(|piece| piece.require_person_when_tests_weaken)
        .flat_map(|piece| &piece.weakened_tests)
        .collect();
    if !weakened.is_empty() {
        let reason = format!(
            "weakened_tests: {}",
            weakened
                .iter()
                .map(|reason| reason.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
        return needs_person_with_context(reason, candidate, evidence, verdict);
    }
    let Some(verdict) = verdict else {
        return Delivery::Refuse("there is no valid review verdict".to_owned());
    };
    if verdict.candidate_tree != candidate.tree {
        return Delivery::Refuse("the review looked at other content".to_owned());
    }
    let changed: Vec<&String> = evidence
        .iter()
        .flat_map(|piece| piece.definitions_changed.iter())
        .collect();
    if !changed.is_empty() {
        return needs_person_with_context(
            format!(
                "the candidate changed what its checks run: {}",
                changed
                    .iter()
                    .map(|path| path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            candidate,
            evidence,
            Some(verdict),
        );
    }
    let mut reasons: Vec<RevisionReason> = evidence
        .iter()
        .filter(|piece| !piece.passed())
        .map(|piece| RevisionReason::FailingCheck(piece.check.clone()))
        .collect();
    reasons.extend(
        verdict
            .blocking()
            .into_iter()
            .cloned()
            .map(RevisionReason::Finding),
    );
    if reasons.is_empty() {
        Delivery::Deliver {
            reduced: evidence.iter().any(|piece| !piece.confined),
        }
    } else {
        Delivery::Revise(reasons)
    }
}

fn needs_person_with_context(
    mut reason: String,
    candidate: &Candidate,
    evidence: &[Evidence],
    verdict: Option<&ReviewVerdict>,
) -> Delivery {
    for piece in evidence.iter().filter(|piece| !piece.passed()) {
        reason.push_str(&format!(
            "\nfailing check: {} ({:?})",
            piece.check.join(" "),
            piece.exit
        ));
    }
    if let Some(verdict) = verdict.filter(|verdict| verdict.candidate_tree == candidate.tree) {
        for finding in verdict.blocking() {
            reason.push_str(&format!(
                "\nblocking finding: {}: {}",
                finding.path, finding.message
            ));
        }
    }
    Delivery::NeedsPerson(reason)
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{Finding, RepoPath, ReviewVerdict, Severity};
    use kaisa_sys::ProcessExit;
    use kaisa_workspace::Candidate;

    use super::{Delivery, RevisionReason, decide_delivery};
    use crate::verify::Evidence;

    /// The tree of the candidate at the gate.
    const CANDIDATE_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    /// The tree of the same work after a later change.
    const CHANGED_TREE: &str = "9daeafb9864cf43055ae93beb0afd6c7d144bfa4";
    /// The exit of a passing test command.
    const PASS: ProcessExit = ProcessExit::Code(0);
    /// The exit of a failing test command.
    const FAIL: ProcessExit = ProcessExit::Code(1);

    fn the_candidate() -> Candidate {
        Candidate {
            commit: "1f2e3d4c5b6a79881726354453627180918a7b6c".to_owned(),
            tree: CANDIDATE_TREE.to_owned(),
        }
    }

    fn evidence_on(tree: &str, exit: ProcessExit) -> Evidence {
        Evidence {
            failure_reason: None,
            candidate_tree: tree.to_owned(),
            check: vec!["cargo".to_owned(), "test".to_owned()],
            exit: Some(exit),
            output_digest: "digest-of-test-output".to_owned(),
            confined: true,
            definitions_changed: Vec::new(),
            weakened_tests: Vec::new(),
            require_person_when_tests_weaken: true,
        }
    }

    fn a_review(tree: &str, severity: Option<Severity>) -> ReviewVerdict {
        ReviewVerdict {
            candidate_tree: tree.to_owned(),
            findings: severity
                .into_iter()
                .map(|severity| Finding {
                    severity,
                    path: RepoPath::try_new("src/auth/session.rs").expect("valid"),
                    line: None,
                    message: "the refresh token is never rotated".to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn nothing_is_delivered_without_fresh_passing_evidence() {
        let approval = a_review(CANDIDATE_TREE, None);

        let without_evidence = decide_delivery(&the_candidate(), &[], Some(&approval));
        let with_failing = decide_delivery(
            &the_candidate(),
            &[evidence_on(CANDIDATE_TREE, FAIL)],
            Some(&approval),
        );

        assert!(
            matches!(without_evidence, Delivery::Refuse(_)),
            "an approval alone never delivers: {without_evidence:?}"
        );
        assert!(
            matches!(with_failing, Delivery::Revise(_)),
            "a failing check sends the work back: {with_failing:?}"
        );
    }

    #[test]
    fn a_tampered_candidate_is_never_delivered() {
        let stale_evidence = [evidence_on(CHANGED_TREE, PASS)];
        let stale_review = a_review(CHANGED_TREE, None);

        let with_stale_evidence = decide_delivery(
            &the_candidate(),
            &stale_evidence,
            Some(&a_review(CANDIDATE_TREE, None)),
        );
        let with_stale_review = decide_delivery(
            &the_candidate(),
            &[evidence_on(CANDIDATE_TREE, PASS)],
            Some(&stale_review),
        );

        assert!(
            matches!(with_stale_evidence, Delivery::Refuse(_))
                && matches!(with_stale_review, Delivery::Refuse(_)),
            "evidence or a review of other content never delivers this content"
        );
    }

    #[test]
    fn passing_evidence_and_an_approval_deliver() {
        let delivery = decide_delivery(
            &the_candidate(),
            &[evidence_on(CANDIDATE_TREE, PASS)],
            Some(&a_review(CANDIDATE_TREE, Some(Severity::Advisory))),
        );

        assert_eq!(
            delivery,
            Delivery::Deliver { reduced: false },
            "advisory findings never stop a verified candidate"
        );
    }

    #[test]
    fn a_blocking_finding_sends_the_work_back_with_the_finding() {
        let delivery = decide_delivery(
            &the_candidate(),
            &[evidence_on(CANDIDATE_TREE, PASS)],
            Some(&a_review(CANDIDATE_TREE, Some(Severity::Blocking))),
        );

        assert!(
            matches!(&delivery, Delivery::Revise(reasons)
                if matches!(reasons.as_slice(), [RevisionReason::Finding(_)])),
            "the revision carries the blocking finding: {delivery:?}"
        );
    }

    #[test]
    fn a_candidate_that_changed_its_checks_needs_a_person() {
        let mut evidence = evidence_on(CANDIDATE_TREE, PASS);
        evidence.definitions_changed = vec!["package.json".to_owned()];

        let delivery = decide_delivery(
            &the_candidate(),
            &[evidence],
            Some(&a_review(CANDIDATE_TREE, None)),
        );

        assert!(
            matches!(delivery, Delivery::NeedsPerson(_)),
            "passing checks the agent rewrote prove nothing on their own: {delivery:?}"
        );
    }

    #[test]
    fn weakened_tests_keep_failed_checks_and_current_blocking_findings_visible() {
        let mut evidence = evidence_on(CANDIDATE_TREE, FAIL);
        evidence.weakened_tests = vec!["src/lib.rs: tests commented out".into()];
        let review = a_review(CANDIDATE_TREE, Some(Severity::Blocking));
        let delivery = decide_delivery(&the_candidate(), &[evidence], Some(&review));
        assert!(
            matches!(delivery, Delivery::NeedsPerson(ref reason) if reason.contains("failing check: cargo test") && reason.contains("blocking finding: src/auth/session.rs")),
            "a human sees all reasons the same candidate is unsafe: {delivery:?}"
        );
    }

    #[test]
    fn an_unconfined_check_delivers_as_reduced() {
        let mut evidence = evidence_on(CANDIDATE_TREE, PASS);
        evidence.confined = false;

        let delivery = decide_delivery(
            &the_candidate(),
            &[evidence],
            Some(&a_review(CANDIDATE_TREE, None)),
        );

        assert_eq!(
            delivery,
            Delivery::Deliver { reduced: true },
            "the user is told the checks ran without OS confinement"
        );
    }
}
