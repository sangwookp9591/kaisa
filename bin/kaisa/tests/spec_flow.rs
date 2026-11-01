//! Specification builds use fake agents and private state; no provider is contacted.
mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_context::{Confirmation, DetectedCommand, Language, Purpose};
use kaisa_daemon::acceptance::AcceptanceStatus;
use kaisa_daemon::qa::QaToolchain;
use kaisa_daemon::spec_flow::{
    AgentStagedReview, ExistingReviewer, LedgerCharacterize, NoCharacterization, SpecOutcome,
    SpecRequest, run_spec_goal, spec_report,
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
    characterize: Box<dyn kaisa_daemon::spec_flow::Characterize + Sync>,
    blind_json: Option<String>,
    author_json: Option<String>,
    accept_reduced_approval: bool,
    production_hooks: bool,
    characterization_json: Option<String>,
    staged_mode: String,
    poison_admission: bool,
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
            characterize: Box::new(NoCharacterization),
            blind_json: None,
            author_json: None,
            accept_reduced_approval: false,
            production_hooks: false,
            characterization_json: None,
            staged_mode: "approve".into(),
            poison_admission: false,
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
            accept_reduced_assurance_approval: self.accept_reduced_approval,
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
                    "--fake-final-answer".into(),
                    "REVIEW_ONLY_DEVELOPER_DECISIONS".into(),
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
            args: if self.production_hooks {
                vec![
                    "--mcp-config".into(),
                    config.display().to_string(),
                    "--fake-staged-review".into(),
                    self.staged_mode.clone(),
                ]
            } else {
                vec![
                    "--mcp-config".into(),
                    config.display().to_string(),
                    "--fake-review-require".into(),
                    LIBRARY.into(),
                ]
            },
            working_directory: std::env::temp_dir(),
            env: Vec::new(),
        };
        let review_file = self.bench.base.join("domain-answer.txt");
        std::fs::write(&review_file, format!("```kaisa-test-review\n{}\n```", json!({"criteria":self.spec.criteria.iter().map(|criterion| json!({"id":criterion.id,"coverage":"covered","note":"contract is asserted"})).collect::<Vec<_>>(),"missing_business_cases":[],"missing_state_transitions":[],"weak_assertions":[],"spec_test_mismatches":[]}))).expect("review answer");
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
        let mut author = qa.clone();
        if let Some(json) = &self.author_json {
            author.env = vec![("KAISA_FAKE_QA_JSON".into(), json.clone())];
        }
        if let Some(json) = &self.characterization_json {
            author
                .env
                .push(("KAISA_FAKE_CHARACTERIZATION_JSON".into(), json.clone()));
        }
        let mut blind = qa.clone();
        if let Some(json) = &self.blind_json {
            blind.env = vec![("KAISA_FAKE_QA_JSON".into(), json.clone())];
        }
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
        let toolchain = QaToolchain::Rust { cargo };
        let characterize = LedgerCharacterize::new(&author, &toolchain);
        let out_of_order = AdmissionBeforeBaseline(&characterize);
        run_spec_goal(&SpecRequest {
            workflow: &workflow,
            spec_id: &self.spec.id,
            version: None,
            qa_author: &author,
            qa_blind: &blind,
            qa_toolchain: &toolchain,
            test_reviewer: &domain,
            characterize: if self.production_hooks && self.poison_admission {
                &out_of_order
            } else if self.production_hooks {
                &characterize
            } else {
                self.characterize.as_ref()
            },
            staged_review: if self.production_hooks {
                &AgentStagedReview
            } else {
                &ExistingReviewer
            },
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

// A hostile verification adapter runs after successful development; this reaches the
// rehash guard independently of MCP's refusal of out-of-worktree developer writes.
struct RewriteFrozenSpec;
impl kaisa_daemon::spec_flow::Characterize for RewriteFrozenSpec {
    fn baseline(
        &self,
        context: &kaisa_daemon::spec_flow::HookContext<'_>,
    ) -> Result<kaisa_daemon::acceptance::GateCheck, kaisa_daemon::GoalError> {
        kaisa_daemon::spec_flow::Characterize::baseline(&NoCharacterization, context)
    }
    fn evaluate(
        &self,
        context: &kaisa_daemon::spec_flow::HookContext<'_>,
        baseline: &kaisa_daemon::acceptance::GateCheck,
    ) -> Result<kaisa_daemon::acceptance::GateCheck, kaisa_daemon::GoalError> {
        let path = context
            .state_directory
            .join("specs")
            .join(context.specification.id.as_str())
            .join("v1.json");
        let mut permissions = std::fs::metadata(&path).expect("frozen file").permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions.clone())
            .expect("hostile adapter makes file writable");
        let mut forged = context.specification.clone();
        forged.criteria[0].then = "accept anything".into();
        std::fs::write(&path, forged.canonical_json()).expect("hostile adapter rewrites bytes");
        permissions.set_readonly(true);
        std::fs::set_permissions(path, permissions).expect("restore readonly marker");
        kaisa_daemon::spec_flow::Characterize::evaluate(&NoCharacterization, context, baseline)
    }
}
fn platform_isolation_acceptance() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        None
    } else {
        Some("reduced(platform state-read QA boundary not verified)")
    }
}
#[test]
fn a_post_development_spec_store_rewrite_fails_the_integrity_gate() {
    let mut bench = BuildBench::new("spec-store-post-development-tamper");
    bench.characterize = Box::new(RewriteFrozenSpec);
    bench.freeze();
    let outcome = bench.run(
        LIBRARY,
        VALID_IMPLEMENTATION,
        true,
        platform_isolation_acceptance(),
    );
    assert_eq!(
        outcome.attempts.len(),
        1,
        "developer completed before tampering"
    );
    assert_eq!(outcome.acceptance.status, AcceptanceStatus::Fail);
    let integrity = outcome
        .acceptance
        .lines
        .iter()
        .find(|line| line.requirement == "spec digest unchanged")
        .expect("integrity line");
    assert_eq!(
        integrity.check.status,
        AcceptanceStatus::Fail,
        "store rehash catches modified private bytes even with readonly restored"
    );
    bench.assert_not_delivered(&outcome);
}
#[test]
fn adversarial_coverage_cannot_fill_an_acceptance_criterion_gap() {
    let mut bench = BuildBench::new("spec-adversarial-only-criterion");
    let mut criterion = bench.spec.criteria[0].clone();
    criterion.id = "AC-2".into();
    bench.spec.criteria.push(criterion);
    bench.blind_json = Some(json!({"tests":[{"test_id":"empty-input","criterion_ids":["AC-1","AC-2"],"file":"empty.rs","contents":"#[test] fn an_empty_input_is_rejected() { assert_eq!(spec_fixture::accept(\"\"), false); }"}]}).to_string());
    bench.freeze();
    let outcome = bench.run(
        LIBRARY,
        VALID_IMPLEMENTATION,
        true,
        platform_isolation_acceptance(),
    );
    assert_eq!(
        outcome.acceptance.status,
        AcceptanceStatus::Fail,
        "even approving domain and staged reviewers cannot fill an acceptance gap"
    );
    let trace = outcome
        .acceptance
        .lines
        .iter()
        .find(|line| line.requirement == "traceability complete")
        .expect("trace line");
    assert_eq!(trace.check.status, AcceptanceStatus::Fail);
    assert!(
        trace
            .check
            .detail
            .contains("AC-2 has no private acceptance test")
    );
    assert!(
        outcome
            .acceptance
            .lines
            .iter()
            .filter(|line| line.requirement != "traceability complete")
            .all(|line| line.check.status == AcceptanceStatus::Pass),
        "the acceptance-kind coverage guard alone blocks delivery: {:?}",
        outcome.acceptance
    );
    bench.assert_not_delivered(&outcome);
}
#[test]
fn malformed_private_qa_is_attributed_to_the_private_qa_gate_line() {
    let mut bench = BuildBench::new("spec-qa-stage-error");
    bench.author_json = Some(json!({"tests":[{"test_id":"unmapped","criterion_ids":[],"file":"empty.rs","contents":"#[test] fn reject_empty() { assert_eq!(spec_fixture::accept(\"\"), false); }"}]}).to_string());
    bench.freeze();
    let outcome = bench.run(
        LIBRARY,
        VALID_IMPLEMENTATION,
        true,
        platform_isolation_acceptance(),
    );
    let qa = outcome
        .acceptance
        .lines
        .iter()
        .find(|line| line.requirement.starts_with("private QA"))
        .expect("QA line");
    assert_eq!(qa.check.status, AcceptanceStatus::Inconclusive);
    assert!(
        qa.check.detail.contains("no criterion mapping"),
        "specific QA failure stays on QA line: {}",
        qa.check.detail
    );
    let preservation = outcome
        .acceptance
        .lines
        .iter()
        .find(|line| line.requirement == "evidence preserved")
        .expect("preservation line");
    assert!(!preservation.check.detail.contains("no criterion mapping"));
    bench.assert_not_delivered(&outcome);
}

/// Fixture for the already-verified WP1 ledger receipt. Approval minting itself is
/// tested by WP1; these tests exercise WP4's consumption of its assurance flag.
fn freeze_reduced_human_receipt(bench: &BuildBench) {
    let state = bench.bench.state();
    kaisa_sys::ensure_private_directory(&state).expect("private state");
    let draft = SpecStore::new(&state)
        .draft(bench.spec.clone())
        .expect("draft");
    let approval = json!({"kind":"human-approved", "user":"test-person", "receipt":{
        "kind":"human-approved", "user":"test-person", "method":"terminal-confirmation",
        "digest":draft.digest, "at":now_ms(), "attempts_unconfined_since_draft":true
    }});
    let payload = json!({"digest":draft.digest,"version":1,"approval_kind":"human-approved","user":"test-person","approval":approval,"attempts_unconfined_since_draft":true});
    let mut ledger = kaisa_ledger::EventStore::open(&state.join("kaisa.db")).expect("ledger");
    for kind in [
        kaisa_domain::EventKind::SpecApproved,
        kaisa_domain::EventKind::SpecFrozen,
    ] {
        ledger
            .append(kaisa_domain::Event {
                kind,
                subject: bench.spec.id.to_string(),
                payload_version: 1,
                payload: payload.to_string(),
            })
            .expect("WP1 receipt fixture");
    }
    let path = state
        .join("specs")
        .join(bench.spec.id.as_str())
        .join("v1.json");
    let mut permissions = std::fs::metadata(&path).expect("spec bytes").permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(path, permissions).expect("freeze bytes");
    assert!(
        SpecStore::new(&state)
            .read(&bench.spec.id, None)
            .expect("verified fixture")
            .frozen
    );
}
#[test]
fn reduced_human_approval_waits_by_default_and_the_cli_retains_the_label() {
    let bench = BuildBench::new("spec-reduced-human-default");
    freeze_reduced_human_receipt(&bench);
    let output = Command::new(BINARY)
        .current_dir(&bench.repository)
        .env("KAISA_STATE_DIR", bench.bench.state())
        .args(["build", "--spec", bench.spec.id.as_str(), "--agent-program"])
        .arg(fake_agent_beside(Path::new(BINARY)))
        .output()
        .expect("build CLI");
    assert_eq!(output.status.code(), Some(3));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("INCONCLUSIVE")
            && text.contains("human-approved (reduced assurance)")
            && text.contains("accept_reduced_assurance_approval=false"),
        "CLI explains reduced approval: {text}"
    );
    assert!(!text.contains("Result:"));
    let run = text
        .lines()
        .find_map(|line| line.strip_prefix("Run: "))
        .expect("run");
    let report = spec_report(&bench.bench.state(), run).expect("report");
    assert!(report.contains("human-approved (reduced assurance)"));
    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            bench
                .bench
                .state()
                .join("spec-runs")
                .join(run)
                .join("acceptance.json"),
        )
        .expect("receipt bytes"),
    )
    .expect("receipt JSON");
    assert_eq!(receipt["reduced_assurance_approval"], true);
    assert!(receipt["result_branch"].is_null());
    assert!(git(&bench.repository, &["branch", "--list", "kaisa/result/*"]).is_empty());
}
#[test]
fn policy_accepted_reduced_human_approval_keeps_its_label_after_delivery() {
    let mut bench = BuildBench::new("spec-reduced-human-accepted");
    bench.accept_reduced_approval = true;
    freeze_reduced_human_receipt(&bench);
    let outcome = bench.run(
        LIBRARY,
        VALID_IMPLEMENTATION,
        false,
        platform_isolation_acceptance(),
    );
    assert_eq!(outcome.acceptance.status, AcceptanceStatus::Pass);
    assert!(
        outcome
            .acceptance
            .label()
            .contains("human-approved (reduced assurance)")
    );
    let report = spec_report(&bench.bench.state(), &outcome.run_id).expect("report");
    assert!(report.contains("human-approved (reduced assurance)"));
    let branch = outcome.result_branch.as_deref().expect("published branch");
    assert!(
        report.contains(branch),
        "report names the actual published result"
    );
    let status = Command::new(BINARY)
        .current_dir(&bench.repository)
        .env("KAISA_STATE_DIR", bench.bench.state())
        .arg("status")
        .output()
        .expect("status CLI");
    assert!(
        status.status.success() && String::from_utf8_lossy(&status.stdout).contains(branch),
        "status makes the accepted branch discoverable: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let ledger =
        kaisa_ledger::EventStore::open(&bench.bench.state().join("kaisa.db")).expect("ledger");
    let events = ledger
        .read_from(kaisa_domain::Sequence::FIRST)
        .expect("events");
    let gate: serde_json::Value = events
        .iter()
        .rev()
        .find(|event| event.recorded.event.kind == kaisa_domain::EventKind::GateAcceptance)
        .map(|event| serde_json::from_str(&event.recorded.event.payload).expect("gate JSON"))
        .expect("gate");
    assert_eq!(gate["decision"]["reduced_assurance_approval"], true);
    assert_eq!(gate["result_branch"], branch);
}

fn production_bench(name: &str, correction: bool) -> BuildBench {
    let mut bench = BuildBench::new(name);
    bench.production_hooks = true;
    bench.spec.criteria[0].correction = correction;
    bench.characterization_json = Some(json!({
        "tests":[{"test_id":"legacy-empty","file":"legacy.rs","contents":"#[test] fn current_empty_input_is_accepted() { assert!(spec_fixture::accept(\"\")); }"}],
        "behaviours":[{"test_id":"legacy-empty","path":LIBRARY,"description":"empty input is currently accepted","edge_case":true,
            "disposition":if correction {json!({"kind":"correct","criterion_id":"AC-1"})} else {json!({"kind":"preserve"})}}]
    }).to_string());
    bench
}
fn gate_line<'a>(
    outcome: &'a SpecOutcome,
    requirement: &str,
) -> &'a kaisa_daemon::acceptance::GateCheck {
    &outcome
        .acceptance
        .lines
        .iter()
        .find(|line| line.requirement == requirement)
        .unwrap_or_else(|| panic!("missing {requirement}: {:?}", outcome.acceptance))
        .check
}
fn recorded_events(bench: &BuildBench) -> Vec<kaisa_ledger::ChainedEvent> {
    kaisa_ledger::EventStore::open(&bench.bench.state().join("kaisa.db"))
        .expect("ledger")
        .read_from(kaisa_domain::Sequence::FIRST)
        .expect("events")
}
#[test]
fn production_legacy_baseline_precedes_real_developer_admission_and_blocks_unexplained_change() {
    let bench = production_bench("production-unexplained", false);
    bench.freeze();
    let outcome = bench.run(
        LIBRARY,
        VALID_IMPLEMENTATION,
        true,
        platform_isolation_acceptance(),
    );
    assert_eq!(outcome.acceptance.status, AcceptanceStatus::Fail);
    let check = gate_line(&outcome, "characterization honoured");
    assert_eq!(check.status, AcceptanceStatus::Fail);
    assert!(
        check
            .detail
            .contains("unexplained behaviour change: legacy-empty"),
        "{}",
        check.detail
    );
    let events = recorded_events(&bench);
    let baseline = events
        .iter()
        .find(|e| e.recorded.event.kind == kaisa_domain::EventKind::CharacterizationBaseline)
        .expect("real baseline");
    let admissions: Vec<_> = events
        .iter()
        .filter(|e| {
            let payload: serde_json::Value =
                serde_json::from_str(&e.recorded.event.payload).expect("payload");
            e.recorded.event.kind == kaisa_domain::EventKind::AttemptAdmitted
                && payload["run_id"] == outcome.run_id
                && payload["role"] == "developer"
        })
        .collect();
    assert_eq!(admissions.len(), 1);
    assert_eq!(
        admissions[0].recorded.event.subject,
        outcome.attempts[0].attempt.as_str(),
        "actual admission, not a synthetic post-execution event"
    );
    assert!(baseline.recorded.sequence < admissions[0].recorded.sequence);
    bench.assert_not_delivered(&outcome);
}
#[test]
fn production_flags_a_required_correction_that_preserves_the_old_behaviour() {
    let bench = production_bench("production-correction-preserved", true);
    bench.freeze();
    let outcome = bench.run(
        LIBRARY,
        BUGGY_IMPLEMENTATION,
        true,
        platform_isolation_acceptance(),
    );
    assert_eq!(outcome.acceptance.status, AcceptanceStatus::Fail);
    let check = gate_line(&outcome, "characterization honoured");
    assert_eq!(check.status, AcceptanceStatus::Fail);
    assert!(
        check
            .detail
            .contains("required correction AC-1 preserved current behaviour"),
        "{}",
        check.detail
    );
    bench.assert_not_delivered(&outcome);
}
#[test]
fn production_staged_reviewer_escalates_with_ledger_evidence_and_blocks_delivery() {
    let mut bench = production_bench("production-staged-escalation", true);
    bench.staged_mode = "escalate".into();
    bench.freeze();
    let outcome = bench.run(
        LIBRARY,
        VALID_IMPLEMENTATION,
        true,
        platform_isolation_acceptance(),
    );
    assert_eq!(outcome.acceptance.status, AcceptanceStatus::Fail);
    let check = gate_line(&outcome, "staged review approved");
    assert_eq!(check.status, AcceptanceStatus::Fail, "{}", check.detail);
    assert!(check.detail.contains("unsafe input transition"));
    let review: serde_json::Value = recorded_events(&bench)
        .iter()
        .rev()
        .find(|e| e.recorded.event.kind == kaisa_domain::EventKind::ReviewVerdict)
        .map(|e| serde_json::from_str(&e.recorded.event.payload).expect("review JSON"))
        .expect("review receipt");
    assert!(
        review["stages"][4]["changes"][0]["evidence_ids"][0]
            .as_str()
            .expect("evidence id")
            .starts_with("ledger:")
    );
    bench.assert_not_delivered(&outcome);
}
#[test]
fn production_rejects_an_explanation_only_blocking_downgrade() {
    let mut bench = production_bench("production-explanation-downgrade", true);
    bench.staged_mode = "explanation".into();
    bench.freeze();
    let outcome = bench.run(
        LIBRARY,
        VALID_IMPLEMENTATION,
        true,
        platform_isolation_acceptance(),
    );
    assert_eq!(outcome.acceptance.status, AcceptanceStatus::Inconclusive);
    let check = gate_line(&outcome, "staged review approved");
    assert_eq!(check.status, AcceptanceStatus::Inconclusive);
    assert!(
        check.detail.contains("requires passing check/QA evidence"),
        "{}",
        check.detail
    );
    bench.assert_not_delivered(&outcome);
}
#[test]
fn production_staged_approval_cannot_override_a_failed_deterministic_line() {
    let bench = production_bench("production-staged-vs-deterministic", true);
    bench.freeze();
    let outcome = bench.run(
        LIBRARY,
        WEAK_IMPLEMENTATION,
        true,
        platform_isolation_acceptance(),
    );
    assert_eq!(
        gate_line(&outcome, "staged review approved").status,
        AcceptanceStatus::Pass
    );
    assert_eq!(
        gate_line(&outcome, "deterministic checks").status,
        AcceptanceStatus::Fail
    );
    assert_eq!(outcome.acceptance.status, AcceptanceStatus::Fail);
    bench.assert_not_delivered(&outcome);
}
#[test]
fn production_happy_path_passes_with_traceability_and_review_only_stage_three_context() {
    let mut bench = production_bench("production-happy-path", true);
    bench.staged_mode = "resolve".into();
    bench.revision_limit = 1;
    bench.freeze();
    let outcome = bench.run(
        LIBRARY,
        BUGGY_IMPLEMENTATION,
        true,
        platform_isolation_acceptance(),
    );
    assert_eq!(
        outcome.acceptance.status,
        AcceptanceStatus::Pass,
        "{:?}",
        outcome.acceptance
    );
    assert_eq!(outcome.attempts.len(), 2);
    assert_eq!(outcome.acceptance.lines.len(), 10);
    if !kaisa_daemon::expected_sandbox_missing(true).is_empty() {
        assert!(outcome.acceptance.label().contains("reduced:"));
        assert!(
            !gate_line(&outcome, "staged review approved")
                .admitted_reductions
                .is_empty()
        );
    }

    assert!(outcome.result_branch.is_some());
    let report = spec_report(&bench.bench.state(), &outcome.run_id).expect("traceability report");
    assert!(
        report.contains("AC-1")
            && report.contains("R1")
            && report.contains(&outcome.acceptance.label())
    );
    let context_path = bench
        .bench
        .state()
        .join("spec-runs")
        .join(&outcome.run_id)
        .join(format!(
            "staged-review-context-{}.json",
            outcome.candidate.as_ref().expect("candidate").tree
        ));
    let context: serde_json::Value =
        serde_json::from_slice(&std::fs::read(context_path).expect("review context"))
            .expect("context JSON");
    assert!(
        context[2]["developer_final_answer"]
            .as_str()
            .expect("final answer")
            .contains("REVIEW_ONLY_DEVELOPER_DECISIONS")
    );
    assert!(
        context[2]["revision_history"]
            .as_str()
            .expect("history")
            .contains("revision.started")
    );
    assert!(
        context[2]["human_interventions"]
            .as_str()
            .expect("interventions")
            .contains("spec_approval")
    );
    let ledger =
        kaisa_ledger::EventStore::open(&bench.bench.state().join("kaisa.db")).expect("ledger");
    let events = recorded_events(&bench);
    let qa_attempts: Vec<_> =
        events
            .iter()
            .filter(|e| e.recorded.event.kind == kaisa_domain::EventKind::AttemptAdmitted)
            .filter(|e| {
                serde_json::from_str::<serde_json::Value>(&e.recorded.event.payload)
                    .expect("payload")["role"]
                    == "qa"
            })
            .map(|e| e.recorded.event.subject.as_str())
            .collect();
    assert_eq!(qa_attempts.len(), 2);
    for attempt in qa_attempts {
        let pack: serde_json::Value = events
            .iter()
            .find(|e| {
                e.recorded.event.kind == kaisa_domain::EventKind::ContextPackRecorded
                    && e.recorded.event.subject == attempt
            })
            .map(|e| serde_json::from_str(&e.recorded.event.payload).expect("pack event"))
            .expect("recorded QA context");
        let digest = kaisa_domain::ContentDigest::try_new(pack["digest"].as_str().expect("digest"))
            .expect("valid digest");
        let qa_context = String::from_utf8(
            ledger
                .context_pack(&digest)
                .expect("QA pack")
                .expect("QA bytes"),
        )
        .expect("UTF-8");
        for forbidden in [
            "REVIEW_ONLY_DEVELOPER_DECISIONS",
            "developer_final_answer",
            "revision_history",
            "human_interventions",
            "attempt.admitted",
            "spec_approval",
        ] {
            assert!(
                !qa_context.contains(forbidden),
                "QA context leaked {forbidden}: {qa_context}"
            );
        }
    }
}

// Simulate a pre-existing run-scoped admission; production must refuse its comparison.
struct AdmissionBeforeBaseline<'a>(&'a dyn kaisa_daemon::spec_flow::Characterize);
impl kaisa_daemon::spec_flow::Characterize for AdmissionBeforeBaseline<'_> {
    fn baseline(
        &self,
        context: &kaisa_daemon::spec_flow::HookContext<'_>,
    ) -> Result<kaisa_daemon::acceptance::GateCheck, kaisa_daemon::GoalError> {
        kaisa_ledger::EventStore::open(&context.state_directory.join("kaisa.db"))
            .expect("ledger")
            .append(kaisa_domain::Event {
                kind: kaisa_domain::EventKind::AttemptAdmitted,
                subject: "att_prebaseline".into(),
                payload_version: 1,
                payload:
                    json!({"run_id":context.run_id,"role":"developer","task":"task_prebaseline"})
                        .to_string(),
            })
            .expect("out-of-order fixture");
        self.0.baseline(context)
    }
    fn evaluate(
        &self,
        context: &kaisa_daemon::spec_flow::HookContext<'_>,
        baseline: &kaisa_daemon::acceptance::GateCheck,
    ) -> Result<kaisa_daemon::acceptance::GateCheck, kaisa_daemon::GoalError> {
        self.0.evaluate(context, baseline)
    }
}
#[test]
fn production_refused_comparison_is_a_fail_with_an_explicit_ordering_reason() {
    let mut bench = production_bench("production-refused-comparison", true);
    bench.poison_admission = true;
    bench.freeze();
    let outcome = bench.run(
        LIBRARY,
        VALID_IMPLEMENTATION,
        true,
        platform_isolation_acceptance(),
    );
    let check = gate_line(&outcome, "characterization honoured");
    assert_eq!(
        check.status,
        AcceptanceStatus::Fail,
        "{:?}",
        outcome.acceptance
    );
    assert!(
        check
            .detail
            .contains("comparison refused: DeveloperAdmittedBeforeBaseline"),
        "{}",
        check.detail
    );
    assert_eq!(outcome.acceptance.status, AcceptanceStatus::Fail);
    bench.assert_not_delivered(&outcome);
}
#[test]
fn production_does_not_admit_a_developer_when_legacy_baseline_authoring_fails() {
    let mut bench = production_bench("production-invalid-baseline", false);
    bench.characterization_json = Some(json!({"tests":[],"behaviours":[]}).to_string());
    bench.freeze();
    let outcome = bench.run(
        LIBRARY,
        VALID_IMPLEMENTATION,
        true,
        platform_isolation_acceptance(),
    );
    assert_eq!(outcome.acceptance.status, AcceptanceStatus::Inconclusive);
    assert!(
        gate_line(&outcome, "characterization honoured")
            .detail
            .contains("every legacy area needs characterization")
    );
    assert!(outcome.attempts.is_empty());
    assert!(recorded_events(&bench).iter().all(|e| {
        let payload: serde_json::Value =
            serde_json::from_str(&e.recorded.event.payload).expect("payload");
        e.recorded.event.kind != kaisa_domain::EventKind::AttemptAdmitted
            || payload["role"] != "developer"
    }));
    bench.assert_not_delivered(&outcome);
}

#[test]
fn build_spec_cli_uses_production_adapters_and_reports_the_admitted_reduction() {
    let bench = production_bench("production-cli-defaults", true);
    bench.freeze();
    let state = bench.bench.state();
    let policy = Command::new(BINARY)
        .current_dir(&bench.repository)
        .env("KAISA_STATE_DIR", &state)
        .args(["policy", "set", "allow_provisional_spec", "true"])
        .output()
        .expect("policy CLI");
    assert!(policy.status.success());
    let domain = bench.bench.base.join("cli-domain.txt");
    std::fs::write(&domain, format!("```kaisa-test-review\n{}\n```", json!({"criteria":[{"id":"AC-1","coverage":"covered","note":"contract asserted"}],"missing_business_cases":[],"missing_state_transitions":[],"weak_assertions":[],"spec_test_mismatches":[]}))).expect("domain fixture");
    let qa = json!({"tests":[{"test_id":"empty-input","criterion_ids":["AC-1"],"file":"empty.rs","contents":"#[test] fn empty_input_is_rejected() { assert_eq!(spec_fixture::accept(\"\"), false); }"}]}).to_string();
    let qa_file = bench.bench.base.join("cli-qa.json");
    let characterization_file = bench.bench.base.join("cli-characterization.json");
    std::fs::write(&qa_file, qa).expect("QA fixture");
    std::fs::write(
        &characterization_file,
        bench
            .characterization_json
            .as_ref()
            .expect("baseline fixture"),
    )
    .expect("characterization fixture");
    let mut build = Command::new(BINARY);
    build
        .current_dir(&bench.repository)
        .env("KAISA_STATE_DIR", &state)
        .args(["build", "--spec", bench.spec.id.as_str(), "--agent-program"])
        .arg(fake_agent_beside(Path::new(BINARY)))
        .args([
            "--agent-arg",
            "--fake-write",
            "--agent-arg",
            LIBRARY,
            "--agent-arg",
            "--fake-content",
            "--agent-arg",
            VALID_IMPLEMENTATION,
            "--reviewer-arg",
            "--fake-staged-review",
            "--reviewer-arg",
            "resolve",
        ])
        .args(["--qa-author-arg", "--fake-qa", "--qa-author-arg"])
        .arg(&qa_file)
        .args([
            "--qa-author-arg",
            "--fake-characterization-answer",
            "--qa-author-arg",
        ])
        .arg(&characterization_file)
        .args(["--qa-blind-arg", "--fake-qa", "--qa-blind-arg"])
        .arg(&qa_file)
        .args([
            "--test-reviewer-arg",
            "--fake-test-review-answer",
            "--test-reviewer-arg",
        ])
        .arg(&domain)
        .arg("--check-program")
        .arg(program("cargo"))
        .args(["--check-arg", "test", "--check-arg", "--offline"]);
    if let Some(isolation) = platform_isolation_acceptance() {
        build.args(["--accept-qa-isolation", isolation]);
    }
    let output = build.output().expect("build CLI");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("PASS") && text.contains("Result:"));
    if !kaisa_daemon::expected_sandbox_missing(true).is_empty() {
        assert!(
            text.contains("reduced:"),
            "CLI must retain the reduction: {text}"
        );
    }
    let run = text
        .lines()
        .find_map(|line| line.strip_prefix("Run: "))
        .expect("run id");
    let report = spec_report(&state, run).expect("traceability report");
    assert!(report.contains("AC-1") && report.contains("legacy characterization honoured"));
    assert!(
        recorded_events(&bench)
            .iter()
            .any(|e| e.recorded.event.kind == kaisa_domain::EventKind::CharacterizationBaseline)
    );
    assert!(recorded_events(&bench).iter().any(|e| e.recorded.event.kind
        == kaisa_domain::EventKind::ReviewVerdict
        && e.recorded.event.payload.contains("stage5_judgment_changes")));
}
