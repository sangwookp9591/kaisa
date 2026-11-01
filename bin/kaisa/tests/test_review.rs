//! `run_test_review` end to end with a scripted reviewer: what the attempt is given, that it cannot
//! write, and that a repository other than the fresh README-only one is refused before any agent
//! starts. No real model runs.

use std::path::{Path, PathBuf};

use kaisa_context::{CriterionKind, CriterionRef, LintPolicy, SourceFile, lint_tests};
use kaisa_daemon::{
    LabelledTest, TestOrigin, TestReviewInput, TestReviewRequest, prepare_review_repository,
    run_test_review,
};
use kaisa_domain::{ContainmentCapability, ReducedModeBasis, ReducedModeDecision};
use kaisa_sys::{ProcessExit, ProcessSpec};

/// The frozen specification the reviewer is given.
const SPEC_TEXT: &str = "{\"title\":\"cart pricing\",\"marker\":\"SPEC-MARKER\"}";
/// Text that exists only in the project's implementation and in the developer's reasoning; the
/// reviewer must never receive it.
const IMPLEMENTATION_MARKER: &str = "IMPLEMENTATION-MARKER";
const REASONING_MARKER: &str = "DEVELOPER-REASONING-MARKER";
const DIFF_MARKER: &str = "diff --git";
/// Where the reviewer tries to write, relative to its worktree.
const FORBIDDEN_WRITE: &str = "notes/reviewer-was-here.md";
/// How long the reduced-mode permission lasts; ample for one run.
const DECISION_MS: u64 = 600_000;
/// The outcome the scripted reviewer records when the bridge refuses its write.
const WRITE_REFUSED: &str = "refused";
/// The lint findings the weak QA test below must produce at least once.
const AT_LEAST_ONE_FINDING: usize = 1;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).expect("a date this century"))
        .expect("the clock is after 1970")
}

fn the_fake_agent() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX))
}

/// A scratch directory removed on drop; the review repository is made inside it, fresh.
struct Bench {
    base: PathBuf,
}

impl Bench {
    fn new(purpose: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "kaisa-test-review-{purpose}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("writable temp dir");
        Self { base }
    }

    fn review_repository(&self) -> PathBuf {
        self.base.join("review")
    }

    fn state(&self) -> PathBuf {
        self.base.join("state")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.base.join(name)
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn criteria() -> Vec<CriterionRef> {
    vec![
        CriterionRef {
            id: "AC-1".to_owned(),
            kind: CriterionKind::Normal,
            requirement_ids: vec!["R1".into()],
            given: "fixture".into(),
            when: "exercised".into(),
            then: "expected outcome".into(),
            correction: false,
        },
        CriterionRef {
            id: "AC-2".to_owned(),
            kind: CriterionKind::Error,
            requirement_ids: vec!["R1".into()],
            given: "fixture".into(),
            when: "exercised".into(),
            then: "expected outcome".into(),
            correction: false,
        },
    ]
}

fn a_valid_review_answer() -> String {
    let review = serde_json::json!({
        "criteria": [
            {"id": "AC-1", "coverage": "covered", "note": "the totals test compares the value"},
            {"id": "AC-2", "coverage": "covered", "note": "the empty cart is rejected"}
        ],
        "missing_business_cases": [],
        "missing_state_transitions": [],
        "weak_assertions": [],
        "spec_test_mismatches": []
    });
    format!("The tests cover the specification.\n```kaisa-test-review\n{review}\n```\n")
}

fn a_decision() -> ReducedModeBasis {
    ReducedModeBasis::Decision(ReducedModeDecision {
        accepts_missing: ContainmentCapability::ALL.to_vec(),
        expires_at_ms: now_ms() + DECISION_MS,
    })
}

/// Runs the scripted reviewer over `review_repository`; the files it leaves are the bench's.
fn review_with_the_scripted_reviewer(
    bench: &Bench,
    review_repository: &Path,
    tests: &[LabelledTest],
) -> Result<kaisa_daemon::TestReviewOutcome, kaisa_daemon::GoalError> {
    let answer = bench.file("answer.txt");
    std::fs::write(&answer, a_valid_review_answer()).expect("written");
    let (context, outcome) = (bench.file("context.txt"), bench.file("write-outcome.txt"));
    let launch = |mcp_config: &Path| ProcessSpec {
        program: the_fake_agent(),
        args: vec![
            "--mcp-config".to_owned(),
            mcp_config.to_string_lossy().into_owned(),
            "--fake-test-review-answer".to_owned(),
            answer.to_string_lossy().into_owned(),
            "--fake-dump-context".to_owned(),
            context.to_string_lossy().into_owned(),
            "--fake-try-write".to_owned(),
            FORBIDDEN_WRITE.to_owned(),
            "--fake-write-outcome".to_owned(),
            outcome.to_string_lossy().into_owned(),
        ],
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    };
    let criteria = criteria();
    let findings = lint_tests(
        &tests
            .iter()
            .map(|test| SourceFile {
                path: test.path.clone(),
                source: test.source.clone(),
            })
            .collect::<Vec<_>>(),
        &[],
        &[],
        &LintPolicy::default(),
    );
    assert!(
        findings.len() >= AT_LEAST_ONE_FINDING,
        "the fixture's weak QA test must give the prompt a lint finding to carry: {findings:#?}"
    );
    run_test_review(&TestReviewRequest {
        review_repository,
        state_directory: &bench.state(),
        input: TestReviewInput {
            spec_text: SPEC_TEXT,
            criteria: &criteria,
            tests,
            findings: &findings,
        },
        launch: &launch,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: false,
        egress_hosts: &[],
        agent_writable: &[],
        observed: false,
        reduced_mode: &a_decision(),
    })
}

fn the_labelled_tests() -> Vec<LabelledTest> {
    vec![
        LabelledTest {
            origin: TestOrigin::PrivateQa,
            path: "qa/cart.rs".to_owned(),
            source: "#[test]\nfn test_case_1() { assert!(true); }\n".to_owned(),
        },
        LabelledTest {
            origin: TestOrigin::Development,
            path: "tests/dev_cart.rs".to_owned(),
            source: "#[test]\nfn an_empty_cart_is_rejected() { assert!(checkout(&empty_cart()).is_err()); }\n".to_owned(),
        },
    ]
}

#[test]
fn the_reviewer_is_given_the_spec_the_tests_and_the_findings_and_cannot_write() {
    let bench = Bench::new("attempt");
    // The project the tests are about, with an implementation and the developer's reasoning,
    // sits beside the review repository. None of it may reach the reviewer.
    let project = bench.file("project");
    std::fs::create_dir_all(&project).expect("writable temp dir");
    std::fs::write(
        project.join("implementation.rs"),
        format!("// {IMPLEMENTATION_MARKER}\npub fn checkout() {{}}\n"),
    )
    .expect("written");
    std::fs::write(project.join("reasoning.md"), REASONING_MARKER).expect("written");
    prepare_review_repository(&bench.review_repository()).expect("a fresh review repository");

    let outcome = review_with_the_scripted_reviewer(
        &bench,
        &bench.review_repository(),
        &the_labelled_tests(),
    )
    .expect("the reviewer attempt runs");

    let pack =
        std::fs::read_to_string(bench.file("context.txt")).expect("the reviewer saved its pack");
    for (needed, why) in [
        ("SPEC-MARKER", "the frozen specification is delivered"),
        ("qa/cart.rs", "the private QA test is delivered"),
        (
            "origin: private-qa",
            "QA tests are labelled with their origin",
        ),
        ("tests/dev_cart.rs", "the development test is delivered"),
        (
            "origin: development",
            "development tests are labelled with their origin",
        ),
        (
            "generic-name",
            "the deterministic lint findings are delivered",
        ),
        ("AC-2", "the criteria to judge are delivered"),
        ("README.md", "the reviewer's whole repository is the README"),
    ] {
        assert!(
            pack.contains(needed),
            "{why}: `{needed}` is missing from the delivered context:\n{pack}"
        );
    }
    for (forbidden, why) in [
        (IMPLEMENTATION_MARKER, "the implementation"),
        (REASONING_MARKER, "the developer's reasoning"),
        (DIFF_MARKER, "a diff"),
        ("implementation.rs", "the implementation's file name"),
    ] {
        assert!(
            !pack.contains(forbidden),
            "{why} must never reach the domain-test reviewer, but `{forbidden}` is in the delivered context:\n{pack}"
        );
    }

    assert_eq!(
        std::fs::read_to_string(bench.file("write-outcome.txt")).unwrap_or_default(),
        WRITE_REFUSED,
        "a reviewer's write through the bridge is refused: its write set is empty"
    );
    assert_eq!(
        outcome.attempt.exit,
        Some(ProcessExit::Code(0)),
        "the reviewer finished normally"
    );
    assert!(
        outcome.attempt.commit.is_none() && outcome.attempt.kept_back.is_empty(),
        "a read-only attempt leaves no work to preserve: {:?}",
        outcome.attempt
    );
    let review = outcome
        .review
        .expect("the scripted answer is a valid review");
    assert!(
        !review.blocks(),
        "the scripted review covers every criterion, so it does not block: {:?}",
        review.blocking_reasons()
    );
}

#[test]
fn a_review_repository_that_holds_anything_but_the_readme_is_refused_before_an_agent_starts() {
    let bench = Bench::new("reused");
    prepare_review_repository(&bench.review_repository()).expect("a fresh review repository");
    // An implementation committed into the reviewer's repository after preparation.
    let review_repository = bench.review_repository();
    std::fs::write(
        review_repository.join("implementation.rs"),
        format!("// {IMPLEMENTATION_MARKER}\n"),
    )
    .expect("written");
    for args in [
        ["add", "."].as_slice(),
        ["commit", "--quiet", "-m", "implementation"].as_slice(),
    ] {
        let status = std::process::Command::new("git")
            .current_dir(&review_repository)
            .args([
                "-c",
                "user.name=Kaisa Test",
                "-c",
                "user.email=t@example.invalid",
            ])
            .args(args)
            .status()
            .expect("git is installed on every CI runner");
        assert!(status.success(), "git {args:?} must succeed in the fixture");
    }

    let refused =
        review_with_the_scripted_reviewer(&bench, &review_repository, &the_labelled_tests());

    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.to_string().contains("README-only")),
        "a repository holding an implementation must be refused at the attempt, naming why; got ok={}",
        refused.is_ok()
    );
    assert!(
        !bench.file("context.txt").exists(),
        "the refusal comes before the agent starts, so the agent never saw any context"
    );
}
