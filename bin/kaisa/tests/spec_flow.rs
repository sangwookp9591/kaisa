//! Specification builds use fake agents and private state; no provider is contacted.
mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_context::{Confirmation, DetectedCommand, Language, Purpose};
use kaisa_daemon::acceptance::AcceptanceStatus;
use kaisa_daemon::qa::QaToolchain;
use kaisa_daemon::spec_flow::{
    ExistingReviewer, NoCharacterization, SpecOutcome, SpecRequest, run_spec_goal, spec_report,
};
use kaisa_daemon::{VerifiedRequest, whole_repository};
use kaisa_domain::{
    AcceptanceCriterion, ContainmentCapability, CriterionKind, Policy, ReducedModeBasis,
    ReducedModeDecision, Requirement, SpecApproval, SpecId, Specification,
};
use kaisa_ledger::spec::SpecStore;
use kaisa_sys::ProcessSpec;
use serde_json::json;
use support::{Workbench, fake_agent_beside, git};

const BINARY: &str = env!("CARGO_BIN_EXE_kaisa");
const LIBRARY: &str = "src/lib.rs";
const SPEC_COPY: &str = "specification.json";
const VALID_IMPLEMENTATION: &str = "pub fn accept(input: &str) -> bool { !input.is_empty() }\n#[cfg(test)] mod tests { #[test] fn a_nonempty_input_is_accepted() { assert!(super::accept(\"value\")); } }\n";
const BUGGY_IMPLEMENTATION: &str = "pub fn accept(_: &str) -> bool { true }\n#[cfg(test)] mod tests { #[test] fn a_nonempty_input_is_accepted() { assert!(super::accept(\"value\")); } }\n";
const WEAK_IMPLEMENTATION: &str = "pub fn accept(input: &str) -> bool { !input.is_empty() }\n#[cfg(test)] mod tests { #[test] fn a_nonempty_input_is_accepted() { assert!(true); } }\n";
const DECISION_LIFETIME_MS: u64 = 600_000;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis()
        .try_into()
        .expect("timestamp")
}
fn spec() -> Specification {
    Specification {
        id: SpecId::try_new("spec_build").expect("spec id"),
        version: 1,
        title: "Reject empty inputs".into(),
        requirements: vec![Requirement {
            id: "R1".into(),
            text: "Reject empty inputs".into(),
        }],
        criteria: vec![AcceptanceCriterion {
            id: "AC-1".into(),
            requirement_ids: vec!["R1".into()],
            given: "empty input".into(),
            when: "accept is called".into(),
            then: "false".into(),
            kind: CriterionKind::Normal,
            correction: false,
        }],
        ambiguities: Vec::new(),
        invariants: Vec::new(),
        error_behaviour: Vec::new(),
    }
}
fn program(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
        .map(|root| {
            root.join(if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.into()
            })
        })
        .find(|path| path.is_file())
        .expect("trusted toolchain")
}
struct BuildBench {
    bench: Workbench,
    repository: PathBuf,
    spec: Specification,
    original_head: String,
    revision_limit: u32,
    development_mappings: Vec<kaisa_domain::spec::TestMapping>,
}
impl BuildBench {
    fn new(name: &str) -> Self {
        let bench = Workbench::new(name);
        let repository = bench.repository("project");
        let spec = spec();
        std::fs::write(
            repository.join("Cargo.toml"),
            "[package]\nname = \"spec-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("fixture manifest");
        std::fs::write(
            repository.join(LIBRARY),
            "pub fn accept(_: &str) -> bool { true }\n",
        )
        .expect("fixture implementation");
        std::fs::write(repository.join(SPEC_COPY), spec.canonical_json()).expect("repo spec copy");
        std::fs::write(repository.join("README.md"), "accept rejects empty input")
            .expect("public contract");
        git(
            &repository,
            &["add", "Cargo.toml", LIBRARY, SPEC_COPY, "README.md"],
        );
        git(
            &repository,
            &[
                "-c",
                "user.name=Kaisa Test",
                "-c",
                "user.email=kaisa@example.invalid",
                "commit",
                "-qm",
                "fixture contract",
            ],
        );
        let original_head = git(&repository, &["rev-parse", "HEAD"]);
        Self {
            bench,
            repository,
            spec,
            original_head,
            revision_limit: 0,
            development_mappings: Vec::new(),
        }
    }
    fn freeze(&self) {
        let state = self.bench.state();
        kaisa_sys::ensure_private_directory(&state).expect("private state");
        let store = SpecStore::new(&state);
        let draft = store.draft(self.spec.clone()).expect("draft");
        let policy = Policy {
            allow_provisional_spec: true,
            ..Policy::personal_default()
        };
        store
            .approve(
                &draft.specification.id,
                draft.specification.version,
                &SpecApproval::Provisional {
                    policy: "explicit test policy".into(),
                },
                &policy,
            )
            .expect("provisional freeze");
    }
    fn run(
        &self,
        target: &str,
        content: &str,
        allow_provisional: bool,
        accept_isolation: Option<&str>,
    ) -> SpecOutcome {
        self.run_mode(target, content, allow_provisional, accept_isolation, false)
    }
    fn run_mode(
        &self,
        target: &str,
        content: &str,
        allow_provisional: bool,
        accept_isolation: Option<&str>,
        observed: bool,
    ) -> SpecOutcome {
        let state = self.bench.state();
        let policy = Policy {
            allow_provisional_spec: allow_provisional,
            revision_limit: self.revision_limit,
            ..Policy::personal_default()
        }
        .snapshot()
        .expect("policy");
        let fake = fake_agent_beside(Path::new(BINARY));
        let launches = std::sync::atomic::AtomicUsize::new(0);
        let developer = |config: &Path| ProcessSpec {
            program: fake.clone(),
            args: if observed {
                vec!["--fake-observed-write".into(), target.into()]
            } else {
                vec![
                    "--mcp-config".into(),
                    config.display().to_string(),
                    "--fake-write".into(),
                    target.into(),
                    "--fake-content".into(),
                    if self.revision_limit > 0
                        && launches.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 0
                    {
                        VALID_IMPLEMENTATION.into()
                    } else {
                        content.into()
                    },
                ]
            },
            working_directory: std::env::temp_dir(),
            env: Vec::new(),
        };
        let reviewer = |config: &Path| ProcessSpec {
            program: fake.clone(),
            args: vec![
                "--mcp-config".into(),
                config.display().to_string(),
                "--fake-review-require".into(),
                LIBRARY.into(),
            ],
            working_directory: std::env::temp_dir(),
            env: Vec::new(),
        };
        let review_file = self.bench.base.join("domain-answer.txt");
        std::fs::write(&review_file, format!("```kaisa-test-review\n{}\n```", json!({"criteria":[{"id":"AC-1","coverage":"covered","note":"empty input is asserted"}],"missing_business_cases":[],"missing_state_transitions":[],"weak_assertions":[],"spec_test_mismatches":[]}))).expect("review answer");
        let domain = |config: &Path| ProcessSpec {
            program: fake.clone(),
            args: vec![
                "--mcp-config".into(),
                config.display().to_string(),
                "--fake-test-review-answer".into(),
                review_file.display().to_string(),
            ],
            working_directory: std::env::temp_dir(),
            env: Vec::new(),
        };
        let qa_json = json!({"tests":[{"test_id":"empty-input","criterion_ids":["AC-1"],"file":"empty.rs","contents":"#[test] fn an_empty_input_is_rejected() { assert_eq!(spec_fixture::accept(\"\"), false); }"}]}).to_string();
        let qa = ProcessSpec {
            program: fake.clone(),
            args: vec!["--fake-qa".into(), "-".into()],
            working_directory: std::env::temp_dir(),
            env: vec![("KAISA_FAKE_QA_JSON".into(), qa_json)],
        };
        let cargo = program("cargo");
        let checks = vec![DetectedCommand {
            language: Language::Rust,
            purpose: Purpose::Test,
            argv: vec![
                cargo.display().to_string(),
                "test".into(),
                "--offline".into(),
            ],
            directory: None,
            confirmation: Confirmation::Confirmed,
        }];
        let basis = ReducedModeBasis::Decision(ReducedModeDecision {
            accepts_missing: ContainmentCapability::ALL.to_vec(),
            expires_at_ms: now_ms() + DECISION_LIFETIME_MS,
        });
        let write_set = whole_repository(&self.repository).expect("write set");
        let workflow = VerifiedRequest {
            repository: &self.repository,
            state_directory: &state,
            goal: "Implement approved spec",
            write_set: &write_set,
            developer: &developer,
            reviewer: &reviewer,
            kaisa_executable: Path::new(BINARY),
            confine_agent: true,
            checks: &checks,
            policy: &policy,
            observed,
            first_pass: None,
            egress_hosts: &[],
            agent_writable: &[],
            reduced_mode: &basis,
        };
        run_spec_goal(&SpecRequest {
            workflow: &workflow,
            spec_id: &self.spec.id,
            version: None,
            qa_author: &qa,
            qa_blind: &qa,
            qa_toolchain: &QaToolchain::Rust { cargo },
            test_reviewer: &domain,
            characterize: &NoCharacterization,
            staged_review: &ExistingReviewer,
            accepted_qa_isolation: accept_isolation,
            development_mappings: &self.development_mappings,
        })
        .expect("workflow records an outcome")
    }
    fn assert_not_delivered(&self, outcome: &SpecOutcome) {
        assert!(
            outcome.result_branch.is_none(),
            "failed/inconclusive gate cannot publish a result"
        );
        assert!(
            git(&self.repository, &["branch", "--list", "kaisa/result/*"]).is_empty(),
            "no delivery branch exists"
        );
        assert_eq!(
            git(&self.repository, &["rev-parse", "HEAD"]),
            self.original_head,
            "no delivery auto-commit changes the user's branch"
        );
    }
}
#[test]
fn failing_private_qa_blocks_delivery_even_when_developer_tests_and_reviewer_pass() {
    let bench = BuildBench::new("spec-private-failure");
    bench.freeze();
    let outcome = bench.run(LIBRARY, BUGGY_IMPLEMENTATION, true, None);
    assert_eq!(
        outcome.acceptance.status,
        AcceptanceStatus::Fail,
        "independent private rejection test must block the buggy candidate"
    );
    assert!(
        outcome
            .acceptance
            .lines
            .iter()
            .any(|line| line.requirement.starts_with("private QA")
                && line.check.status == AcceptanceStatus::Fail),
        "failure is attributed to private QA"
    );
    bench.assert_not_delivered(&outcome);
}
#[test]
fn an_approving_reviewer_cannot_override_a_lint_blocking_test() {
    let bench = BuildBench::new("spec-lint-failure");
    bench.freeze();
    let outcome = bench.run(LIBRARY, WEAK_IMPLEMENTATION, true, None);
    assert_eq!(
        outcome.acceptance.status,
        AcceptanceStatus::Fail,
        "assert true must fail deterministic lint regardless of approval"
    );
    assert!(
        outcome
            .acceptance
            .lines
            .iter()
            .any(|line| line.requirement == "deterministic checks"
                && line.check.status == AcceptanceStatus::Fail),
        "deterministic failure remains visible"
    );
    bench.assert_not_delivered(&outcome);
}
#[test]
fn a_developer_edit_to_a_repo_criterion_fails_and_preserves_the_approved_spec() {
    let bench = BuildBench::new("spec-copy-tamper");
    bench.freeze();
    let mut edited = bench.spec.clone();
    edited.criteria[0].then = "accept empty input".into();
    let outcome = bench.run(
        SPEC_COPY,
        &String::from_utf8(edited.canonical_json()).expect("JSON text"),
        true,
        None,
    );
    assert_eq!(
        outcome.acceptance.status,
        AcceptanceStatus::Fail,
        "repo criterion edits cannot change the approved contract"
    );
    assert_eq!(
        SpecStore::new(&bench.bench.state())
            .read(&bench.spec.id, None)
            .expect("approved bytes remain valid")
            .specification,
        bench.spec,
        "approved spec never changes"
    );
    assert!(
        outcome
            .acceptance
            .lines
            .iter()
            .any(|line| line.requirement == "spec digest unchanged"
                && line.check.status == AcceptanceStatus::Fail),
        "tampering fails the protected-spec line independently of QA"
    );
    bench.assert_not_delivered(&outcome);
}
#[test]
fn provisional_specs_without_policy_refuse_before_development_and_never_claim_human_approval() {
    let bench = BuildBench::new("spec-provisional-refusal");
    bench.freeze();
    let outcome = bench.run(LIBRARY, VALID_IMPLEMENTATION, false, None);
    assert_eq!(
        outcome.acceptance.status,
        AcceptanceStatus::Inconclusive,
        "policy must explicitly permit provisional specs"
    );
    assert!(
        outcome.attempts.is_empty(),
        "developer never starts under disallowed approval"
    );
    assert!(
        outcome.acceptance.label().contains("PROVISIONAL")
            && !outcome.acceptance.label().contains("HUMAN"),
        "receipt cannot manufacture human approval"
    );
    bench.assert_not_delivered(&outcome);
}
#[test]
fn a_requirement_without_criteria_is_reported_before_any_developer_attempt() {
    let mut bench = BuildBench::new("spec-missing-criterion");
    bench.spec.criteria.clear();
    kaisa_sys::ensure_private_directory(&bench.bench.state()).expect("state");
    SpecStore::new(&bench.bench.state())
        .draft(bench.spec.clone())
        .expect("incomplete draft");
    let outcome = bench.run(LIBRARY, VALID_IMPLEMENTATION, true, None);
    assert_eq!(
        outcome.acceptance.status,
        AcceptanceStatus::Fail,
        "uncovered requirement blocks admission"
    );
    assert!(
        outcome.attempts.is_empty(),
        "no developer runs before criteria exist"
    );
    assert!(
        spec_report(&bench.bench.state(), &outcome.run_id)
            .expect("report")
            .contains("R1"),
        "report identifies the original missing requirement"
    );
    bench.assert_not_delivered(&outcome);
}
#[test]
fn build_and_report_cli_reuse_goal_options_and_show_provisional_policy_refusal() {
    let bench = BuildBench::new("spec-build-cli");
    bench.freeze();
    let output = Command::new(BINARY)
        .current_dir(&bench.repository)
        .env("KAISA_STATE_DIR", bench.bench.state())
        .args([
            "build",
            "--spec",
            bench.spec.id.as_str(),
            "--version",
            "1",
            "--agent-program",
        ])
        .arg(fake_agent_beside(Path::new(BINARY)))
        .output()
        .expect("build CLI");
    assert_eq!(
        output.status.code(),
        Some(AcceptanceStatus::Inconclusive.exit_code().into()),
        "build has the spec-gate CI contract"
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("PROVISIONAL") && text.contains("allow_provisional_spec"),
        "receipt states why policy refuses: {text}"
    );
    let run = text
        .lines()
        .find_map(|line| line.strip_prefix("Run: "))
        .expect("run receipt");
    let report = Command::new(BINARY)
        .current_dir(&bench.repository)
        .env("KAISA_STATE_DIR", bench.bench.state())
        .args(["spec", "report", run])
        .output()
        .expect("report CLI");
    assert!(
        report.status.success()
            && String::from_utf8_lossy(&report.stdout).contains("spec approval status"),
        "spec report prints all gate lines"
    );
}

#[test]
fn reduced_qa_isolation_without_policy_waits_and_never_publishes_a_result() {
    let mut bench = BuildBench::new("spec-reduced-isolation");
    std::fs::write(bench.repository.join(LIBRARY), VALID_IMPLEMENTATION).expect("correct baseline");
    git(&bench.repository, &["add", LIBRARY]);
    git(
        &bench.repository,
        &[
            "-c",
            "user.name=Kaisa Test",
            "-c",
            "user.email=kaisa@example.invalid",
            "commit",
            "-qm",
            "correct baseline",
        ],
    );
    bench.original_head = git(&bench.repository, &["rev-parse", "HEAD"]);
    bench.freeze();
    let outcome = bench.run_mode("README.md", "", true, None, true);
    assert_eq!(
        outcome.acceptance.status,
        AcceptanceStatus::Inconclusive,
        "observed developers have reduced QA state-read isolation: {}",
        std::fs::read_to_string(&outcome.report_path).expect("report")
    );
    assert!(
        outcome
            .acceptance
            .lines
            .iter()
            .any(|line| line.requirement.starts_with("qa_isolation")
                && line.check.detail.contains("observed developer")),
        "the actual isolation gap is named"
    );
    bench.assert_not_delivered(&outcome);
}
#[test]
fn passing_final_evidence_publishes_only_a_provisional_result_after_the_gate() {
    let bench = BuildBench::new("spec-pass-delivery");
    bench.freeze();
    let isolation_policy = if cfg!(target_os = "macos") {
        None
    } else {
        Some("reduced(platform state-read QA boundary not verified)")
    };
    let outcome = bench.run(LIBRARY, VALID_IMPLEMENTATION, true, isolation_policy);
    assert_eq!(
        outcome.acceptance.status,
        AcceptanceStatus::Pass,
        "all independent final-tree evidence permits delivery: {}",
        std::fs::read_to_string(&outcome.report_path).expect("gate report")
    );
    assert!(
        outcome.result_branch.is_some(),
        "PASS publishes the result branch"
    );
    assert_eq!(
        git(&bench.repository, &["rev-parse", "HEAD"]),
        bench.original_head,
        "publication never auto-commits the user branch"
    );
    assert!(
        spec_report(&bench.bench.state(), &outcome.run_id)
            .expect("report")
            .contains("PROVISIONAL"),
        "provenance survives delivery"
    );
}
#[test]
fn a_developer_attempt_to_write_private_spec_state_is_refused_without_changing_it() {
    let bench = BuildBench::new("spec-state-write");
    bench.freeze();
    let path = bench
        .bench
        .state()
        .join("specs")
        .join(bench.spec.id.as_str())
        .join("v1.json");
    let original = std::fs::read(&path).expect("approved spec");
    let outcome = bench.run(&path.display().to_string(), "forged spec", true, None);
    assert_eq!(
        outcome.acceptance.status,
        AcceptanceStatus::Fail,
        "private state writes cannot become accepted development"
    );
    assert_eq!(
        std::fs::read(&path).expect("spec still readable"),
        original,
        "approved bytes remain unchanged"
    );
    bench.assert_not_delivered(&outcome);
}

#[test]
fn a_revision_reruns_both_sealed_private_suites_on_the_new_final_tree() {
    let mut bench = BuildBench::new("spec-bounded-revision");
    bench.revision_limit = 1;
    bench.freeze();
    let accepted = if cfg!(target_os = "macos") {
        None
    } else {
        Some("reduced(platform state-read QA boundary not verified)")
    };
    let outcome = bench.run(LIBRARY, BUGGY_IMPLEMENTATION, true, accepted);
    assert_eq!(
        outcome.acceptance.status,
        AcceptanceStatus::Pass,
        "observable private failure guides a successful bounded revision: {}",
        std::fs::read_to_string(&outcome.report_path).expect("report")
    );
    assert_eq!(
        outcome.attempts.len(),
        (bench.revision_limit + 1) as usize,
        "exactly the allowed developer revision ran"
    );
    let report = spec_report(&bench.bench.state(), &outcome.run_id).expect("traceability report");
    let final_tree = &outcome.candidate.as_ref().expect("candidate").tree;
    assert!(
        report.contains(final_tree) && !report.contains("FAIL/stale"),
        "only final-tree QA contributes to the delivered matrix"
    );
    let ledger =
        kaisa_ledger::EventStore::open(&bench.bench.state().join("kaisa.db")).expect("ledger");
    let events = ledger
        .read_from(kaisa_domain::Sequence::FIRST)
        .expect("events");
    let suite_events: Vec<_> = events
        .iter()
        .filter(|event| event.recorded.event.kind == kaisa_domain::EventKind::QaSuiteRecorded)
        .collect();
    let revision_attempt = outcome.attempts.last().expect("revision attempt");
    let digest = kaisa_domain::ContentDigest::try_new(&revision_attempt.context_pack)
        .expect("context digest");
    let context_bytes = ledger
        .context_pack(&digest)
        .expect("recorded context")
        .expect("context bytes");
    let context = String::from_utf8_lossy(&context_bytes);
    assert!(
        context.contains("empty-input")
            && !context.contains("spec_fixture::accept")
            && !context.contains("#[test] fn an_empty_input_is_rejected"),
        "revision receives observable failure summaries without private test source"
    );
    const INDEPENDENT_SUITES: usize = 2;
    assert_eq!(
        suite_events.len(),
        INDEPENDENT_SUITES,
        "QA authors run once, while sealed suites rerun after revision"
    );
}

#[test]
fn editing_a_preserved_report_is_rejected_by_its_gate_ledger_digest() {
    let bench = BuildBench::new("spec-report-integrity");
    bench.freeze();
    let outcome = bench.run(LIBRARY, VALID_IMPLEMENTATION, false, None);
    std::fs::write(&outcome.report_path, "PASS human-approved").expect("tampered report");
    assert!(
        spec_report(&bench.bench.state(), &outcome.run_id).is_err(),
        "modified artifacts cannot fabricate approval or acceptance"
    );
}

#[test]
fn a_development_mapping_to_an_unknown_criterion_blocks_acceptance() {
    let mut bench = BuildBench::new("spec-development-mapping");
    bench
        .development_mappings
        .push(kaisa_domain::spec::TestMapping {
            test_id: "src/lib.rs::a_nonempty_input_is_accepted".into(),
            criterion_ids: vec!["AC-unknown".into()],
        });
    bench.freeze();
    let accepted = if cfg!(target_os = "macos") {
        None
    } else {
        Some("reduced(platform state-read QA boundary not verified)")
    };
    let outcome = bench.run(LIBRARY, VALID_IMPLEMENTATION, true, accepted);
    assert_eq!(
        outcome.acceptance.status,
        AcceptanceStatus::Fail,
        "development coverage references must use frozen criterion IDs"
    );
    assert!(
        outcome
            .acceptance
            .lines
            .iter()
            .any(|line| line.requirement == "deterministic checks"
                && line.check.detail.contains("AC-unknown")),
        "invalid development mapping is identified in the deterministic line"
    );
    bench.assert_not_delivered(&outcome);
}
