//! A verified team run end to end with scripted agents (T31): developer → trusted checks →
//! reviewer → delivery gate → bounded revision.

use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_context::{Confirmation, DetectedCommand, Language, Purpose};
use kaisa_daemon::{
    DEFAULT_REVISION_LIMIT, Delivery, VerifiedOutcome, VerifiedRequest, run_verified_goal,
};
use kaisa_domain::{RepoPath, WriteSet};
use kaisa_sys::ProcessSpec;

/// What the user asks for.
const GOAL: &str = "Write the session notes in two steps.";
/// Where the scripted developer writes its step files.
const STEP_PREFIX: &str = "src/step";
/// The file the first developer attempt writes.
const FIRST_STEP: &str = "src/step-1.md";
/// The file only a revision writes; the reviewer requires it in T31b.
const SECOND_STEP: &str = "src/step-2.md";

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .expect("git is installed on every CI runner");
    assert!(output.status.success(), "git {args:?} failed: {output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repository with one commit, removed on drop.
struct Workbench {
    base: PathBuf,
}

impl Workbench {
    fn new(name: &str) -> Self {
        let base: PathBuf =
            std::env::temp_dir().join(format!("kaisa-team-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("repo/src")).expect("writable temp dir");
        std::fs::write(base.join("repo/src/lib.rs"), "pub fn login() {}\n").expect("written");
        let repository: PathBuf = base.join("repo");
        git(&repository, &["init", "--quiet", "--initial-branch=main"]);
        git(&repository, &["add", "."]);
        git(
            &repository,
            &[
                "-c",
                "user.name=Kaisa Test",
                "-c",
                "user.email=kaisa-test@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "initial",
            ],
        );
        Self { base }
    }

    fn repository(&self) -> PathBuf {
        self.base.join("repo")
    }
}

impl Drop for Workbench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn the_fake_agent() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX))
}

fn scripted(mcp_config: &Path, role_args: &[&str]) -> ProcessSpec {
    let mut args: Vec<String> = vec![
        "--mcp-config".to_owned(),
        mcp_config.to_string_lossy().into_owned(),
    ];
    args.extend(role_args.iter().map(|arg| (*arg).to_owned()));
    ProcessSpec {
        program: the_fake_agent(),
        args,
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    }
}

/// The project's test command: passes once the first step exists.
fn the_projects_test_command() -> DetectedCommand {
    let argv: Vec<String> = if cfg!(windows) {
        vec![
            "cmd".to_owned(),
            "/c".to_owned(),
            r"if exist src\step-1.md (exit 0) else (exit 1)".to_owned(),
        ]
    } else {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("test -f {FIRST_STEP}"),
        ]
    };
    DetectedCommand {
        language: Language::Rust,
        purpose: Purpose::Test,
        argv,
        directory: None,
        confirmation: Confirmation::Confirmed,
    }
}

/// The policy a test run adopts.
fn a_policy(confine_agents: bool) -> kaisa_domain::PolicySnapshot {
    kaisa_domain::Policy {
        accept_reduced_assurance_approval: false,
        allow_provisional_spec: false,
        routing: kaisa_domain::RoutingPolicy::default(),
        revision_limit: DEFAULT_REVISION_LIMIT,
        allowed_egress_hosts: Vec::new(),
        confine_agents,
        min_grade_for_security_review: kaisa_domain::Grade::T3,
        require_person_when_check_definitions_change: true,
        require_person_when_tests_weaken: true,
        ..kaisa_domain::Policy::personal_default()
    }
    .snapshot()
    .expect("a valid policy")
}

fn a_team_run(bench: &Workbench, reviewer_requires: &str) -> VerifiedOutcome {
    a_team_run_with_a_reviewer(bench, &["--fake-review-require", reviewer_requires])
}

/// A team run whose scripted reviewer is started with `reviewer_args`.
fn a_team_run_with_a_reviewer(bench: &Workbench, reviewer_args: &[&str]) -> VerifiedOutcome {
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let developer = |mcp_config: &Path| scripted(mcp_config, &["--fake-write-next", STEP_PREFIX]);
    let reviewer = |mcp_config: &Path| scripted(mcp_config, reviewer_args);
    run_verified_goal(&VerifiedRequest {
        repository: &bench.repository(),
        state_directory: &bench.base.join("state"),
        goal: GOAL,
        write_set: &write_set,
        developer: &developer,
        reviewer: &reviewer,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        checks: &[the_projects_test_command()],
        policy: &a_policy(true),
        observed: false,
        first_pass: None,
        egress_hosts: &[],
        agent_writable: &[],
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("the team run completes")
}

#[test]
fn a_team_run_delivers_only_a_verified_candidate() {
    let bench = Workbench::new("deliver");

    let outcome: VerifiedOutcome = a_team_run(&bench, FIRST_STEP);

    assert!(
        matches!(outcome.delivery, Delivery::Deliver { .. }) && outcome.revisions == 0,
        "checks pass and the reviewer approves the first candidate: {:?}",
        outcome.delivery
    );
    let delivered = outcome.candidate.expect("a delivered candidate exists");
    let files: String = git(
        &bench.repository(),
        &["ls-tree", "-r", "--name-only", &delivered.commit],
    );
    assert!(
        files.contains(FIRST_STEP),
        "the delivered commit is the one that was checked and reviewed: {files:?}"
    );
}

#[test]
fn a_blocking_finding_is_fixed_by_revision_without_a_person() {
    let bench = Workbench::new("revise");

    let outcome: VerifiedOutcome = a_team_run(&bench, SECOND_STEP);

    assert!(
        matches!(outcome.delivery, Delivery::Deliver { .. }),
        "the revision fixed the blocking finding and the gate delivered: {:?}",
        outcome.delivery
    );
    assert_eq!(
        outcome.revisions, ONE_REVISION,
        "exactly one revision was needed and no person was asked"
    );
    let delivered = outcome.candidate.expect("a delivered candidate exists");
    let files: String = git(
        &bench.repository(),
        &["ls-tree", "-r", "--name-only", &delivered.commit],
    );
    assert!(
        files.contains(FIRST_STEP) && files.contains(SECOND_STEP),
        "the revision built on the first candidate and added what the review asked for: {files:?}"
    );
}

/// The revisions T31b's scripted run needs: one.
const ONE_REVISION: u32 = 1;

#[test]
fn a_repository_that_widens_agent_authority_needs_a_person() {
    let bench = Workbench::new("risky-settings");
    std::fs::create_dir_all(bench.repository().join(".claude")).expect("writable");
    std::fs::write(
        bench.repository().join(".claude/settings.json"),
        r#"{"permissions":{"defaultMode":"bypassPermissions"}}"#,
    )
    .expect("written");

    let outcome: VerifiedOutcome = a_team_run(&bench, FIRST_STEP);

    assert!(
        matches!(&outcome.delivery, Delivery::NeedsPerson(reason) if reason.contains(".claude/settings.json")),
        "the risky settings are named and a person decides: {:?}",
        outcome.delivery
    );
    assert!(
        outcome.attempts.is_empty(),
        "no agent starts in a repository whose settings would widen its authority"
    );
}

#[test]
fn a_run_records_the_policy_it_ran_under() {
    let bench = Workbench::new("policy");
    let policy = a_policy(true);

    a_team_run(&bench, FIRST_STEP);

    let adopted = kaisa_ledger::EventStore::open(&bench.base.join("state").join("kaisa.db"))
        .and_then(|ledger| ledger.adopted_policies())
        .expect("the ledger reads");
    assert_eq!(
        adopted
            .iter()
            .map(|recorded| recorded.canonical.as_str())
            .collect::<Vec<_>>(),
        vec![policy.canonical()],
        "the run's ledger names exactly the policy it ran under"
    );
}

/// A path the write set does not cover, which an observed developer writes anyway.
const OUTSIDE_THE_WRITE_SET: &str = "docs/outside.md";

#[test]
fn an_observed_change_outside_the_write_set_needs_a_person() {
    let bench = Workbench::new("outside");
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let developer = |_mcp_config: &Path| ProcessSpec {
        program: the_fake_agent(),
        args: vec![
            "--fake-observed-write".to_owned(),
            OUTSIDE_THE_WRITE_SET.to_owned(),
        ],
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    };
    let reviewer = |mcp_config: &Path| {
        scripted(
            mcp_config,
            &["--fake-review-require", OUTSIDE_THE_WRITE_SET],
        )
    };

    let outcome: VerifiedOutcome = run_verified_goal(&VerifiedRequest {
        repository: &bench.repository(),
        state_directory: &bench.base.join("state"),
        goal: GOAL,
        write_set: &write_set,
        developer: &developer,
        reviewer: &reviewer,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        checks: &[the_projects_test_command()],
        policy: &a_policy(true),
        observed: true,
        first_pass: None,
        egress_hosts: &[],
        agent_writable: &[],
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("the team run completes");

    assert!(
        matches!(&outcome.delivery, Delivery::NeedsPerson(reason) if reason.contains(OUTSIDE_THE_WRITE_SET)),
        "a write the broker never saw is caught before delivery and named: {:?}",
        outcome.delivery
    );
}

/// A team run with a first-pass reviewer requiring `first_pass_requires` and a full reviewer
/// requiring `reviewer_requires`.
fn a_team_run_with_a_first_pass(
    bench: &Workbench,
    first_pass_requires: &str,
    reviewer_requires: &str,
) -> VerifiedOutcome {
    a_team_run_with_a_first_pass_format(bench, first_pass_requires, reviewer_requires, "raw")
}

fn a_team_run_with_a_first_pass_format(
    bench: &Workbench,
    first_pass_requires: &str,
    reviewer_requires: &str,
    format: &str,
) -> VerifiedOutcome {
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let developer = |mcp_config: &Path| scripted(mcp_config, &["--fake-write-next", STEP_PREFIX]);
    let first_pass = |mcp_config: &Path| {
        scripted(
            mcp_config,
            &[
                "--fake-review-require",
                first_pass_requires,
                "--fake-review-format",
                format,
            ],
        )
    };
    let reviewer =
        |mcp_config: &Path| scripted(mcp_config, &["--fake-review-require", reviewer_requires]);
    run_verified_goal(&VerifiedRequest {
        repository: &bench.repository(),
        state_directory: &bench.base.join("state"),
        goal: GOAL,
        write_set: &write_set,
        developer: &developer,
        reviewer: &reviewer,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        checks: &[the_projects_test_command()],
        policy: &a_policy(true),
        observed: false,
        first_pass: Some(kaisa_daemon::FirstPass {
            launch: &first_pass,
            model_port: None,
        }),
        egress_hosts: &[],
        agent_writable: &[],
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("the team run completes")
}

/// Attempts in a first-pass run: developer, first pass, developer, first pass, full review.
const ATTEMPTS_WITH_ONE_FIRST_PASS_SEND_BACK: usize = 5;
/// Attempts when the first pass approves but the full review sends back once: developer,
/// first pass and full review, twice.
const ATTEMPTS_WHEN_THE_FULL_REVIEW_OVERRULES: usize = 6;

#[test]
fn a_first_pass_blocking_finding_revises_without_the_full_review() {
    let bench = Workbench::new("first-pass-revise");

    let outcome: VerifiedOutcome = a_team_run_with_a_first_pass(&bench, SECOND_STEP, FIRST_STEP);

    assert!(
        matches!(outcome.delivery, Delivery::Deliver { .. }) && outcome.revisions == 1,
        "the first pass sent the candidate back once and the revised one was delivered: {:?}",
        outcome.delivery
    );
    assert_eq!(
        outcome.attempts.len(),
        ATTEMPTS_WITH_ONE_FIRST_PASS_SEND_BACK,
        "the full review ran only on the revised candidate, never on the one sent back"
    );
}

#[test]
fn a_first_pass_approval_still_needs_the_full_review() {
    let bench = Workbench::new("first-pass-advisory");

    let outcome: VerifiedOutcome = a_team_run_with_a_first_pass(&bench, FIRST_STEP, SECOND_STEP);

    assert!(
        matches!(outcome.delivery, Delivery::Deliver { .. }) && outcome.revisions == 1,
        "the full reviewer's blocking finding overruled the first pass's approval once: {:?}",
        outcome.delivery
    );
    assert_eq!(
        outcome.attempts.len(),
        ATTEMPTS_WHEN_THE_FULL_REVIEW_OVERRULES,
        "the full review ran after every first-pass approval"
    );
}

/// A host the policy allows the agents to reach.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const ALLOWED_HOST: &str = "allowed.example";
/// A host nobody allowed; the egress broker must refuse it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const UNLISTED_HOST: &str = "unlisted.example";

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_team_run_gives_its_agents_the_egress_broker() {
    let bench = Workbench::new("team-egress");
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let developer = |mcp_config: &Path| {
        scripted(
            mcp_config,
            &[
                "--fake-write-next",
                STEP_PREFIX,
                "--expect-proxy-refuses",
                UNLISTED_HOST,
            ],
        )
    };
    let reviewer = |mcp_config: &Path| scripted(mcp_config, &["--fake-review-require", FIRST_STEP]);
    let egress_hosts: [&str; 1] = [ALLOWED_HOST];

    let outcome: VerifiedOutcome = run_verified_goal(&VerifiedRequest {
        repository: &bench.repository(),
        state_directory: &bench.base.join("state"),
        goal: GOAL,
        write_set: &write_set,
        developer: &developer,
        reviewer: &reviewer,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        checks: &[the_projects_test_command()],
        policy: &a_policy(true),
        observed: false,
        first_pass: None,
        egress_hosts: &egress_hosts,
        agent_writable: &[],
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("the team run completes");

    assert!(
        matches!(outcome.delivery, Delivery::Deliver { .. }),
        "the developer found the egress broker in front of it, refusing an unlisted host: {:?}",
        outcome.delivery
    );
}

#[test]
fn a_reviewer_answering_in_claude_stream_json_gets_its_approval_delivered() {
    let bench = Workbench::new("claude-format");

    let outcome: VerifiedOutcome = a_team_run_with_a_reviewer(
        &bench,
        &[
            "--fake-review-require",
            FIRST_STEP,
            "--fake-review-format",
            "claude",
        ],
    );

    assert!(
        matches!(outcome.delivery, Delivery::Deliver { .. }),
        "the verdict in Claude's result text is read, not the result event on the last line: {:?}",
        outcome.delivery
    );
}

#[test]
fn a_claude_result_without_a_verdict_is_never_delivered() {
    let bench = Workbench::new("claude-prose");

    let outcome: VerifiedOutcome = a_team_run_with_a_reviewer(
        &bench,
        &[
            "--fake-review-require",
            FIRST_STEP,
            "--fake-review-format",
            "claude-prose",
        ],
    );

    assert!(
        matches!(outcome.delivery, Delivery::Refuse(_)) && outcome.candidate.is_some(),
        "a review that answered in prose only gave no verdict, so nothing is delivered: {:?}",
        outcome.delivery
    );
}

/// A reviewer prints a successful Claude result before either overflowing capture or exiting 1.
fn a_team_with_an_unreliable_reviewer(bench: &Workbench, format: &str) -> VerifiedOutcome {
    a_team_run_with_a_reviewer(
        bench,
        &[
            "--fake-review-require",
            FIRST_STEP,
            "--fake-review-format",
            format,
        ],
    )
}

#[test]
fn a_truncated_reviewer_transcript_is_refused_and_its_metadata_preserves_the_loss() {
    let bench = Workbench::new("truncated-reviewer");
    let outcome = a_team_with_an_unreliable_reviewer(&bench, "claude-truncated");
    let review = outcome.attempts.last().expect("the reviewer ran");
    let metadata: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            bench
                .base
                .join("state/attempts")
                .join(review.attempt.as_str())
                .join("transcript-meta.json"),
        )
        .expect("capture metadata was saved"),
    )
    .expect("metadata is JSON");
    assert!(
        review.truncated && metadata["truncated"] == true,
        "both the outcome and saved metadata must preserve the capture loss"
    );
    assert!(
        matches!(outcome.delivery, Delivery::Refuse(ref reason) if reason.contains("truncated")),
        "a success result before a lost failing result cannot approve: {:?}",
        outcome.delivery
    );
}

#[test]
fn a_reviewer_that_exits_unsuccessfully_after_success_is_refused() {
    let bench = Workbench::new("failed-reviewer");
    let outcome = a_team_with_an_unreliable_reviewer(&bench, "claude-failed");
    assert!(
        matches!(outcome.delivery, Delivery::Refuse(ref reason) if reason.contains("exit")),
        "a success event cannot override the actual process exit: {:?}",
        outcome.delivery
    );
}

/// Where Claude Code reads a repository's agent settings.
const CLAUDE_SETTINGS: &str = ".claude/settings.json";
/// Claude settings that let an agent act without asking.
const BYPASS_PERMISSIONS: &str = r#"{"permissions":{"defaultMode":"bypassPermissions"}}"#;
/// Attempts when the run stops right after the first developer: that developer alone.
const ONLY_THE_DEVELOPER: usize = 1;

/// A test command that always passes, so only the settings decide the outcome.
fn a_check_that_always_passes() -> DetectedCommand {
    let argv: Vec<String> = if cfg!(windows) {
        vec!["cmd".to_owned(), "/c".to_owned(), "exit 0".to_owned()]
    } else {
        vec!["sh".to_owned(), "-c".to_owned(), "true".to_owned()]
    };
    DetectedCommand {
        argv,
        ..the_projects_test_command()
    }
}

#[test]
fn a_candidate_that_widens_agent_settings_needs_a_person_before_any_reviewer_starts() {
    let bench = Workbench::new("candidate-settings");
    let write_set = WriteSet::new(vec![
        RepoPath::try_new("src").expect("valid"),
        RepoPath::try_new(".claude").expect("valid"),
    ]);
    let developer = |mcp_config: &Path| {
        scripted(
            mcp_config,
            &[
                "--fake-write",
                CLAUDE_SETTINGS,
                "--fake-content",
                BYPASS_PERMISSIONS,
            ],
        )
    };
    let reviewer =
        |mcp_config: &Path| scripted(mcp_config, &["--fake-review-require", CLAUDE_SETTINGS]);

    let outcome: VerifiedOutcome = run_verified_goal(&VerifiedRequest {
        repository: &bench.repository(),
        state_directory: &bench.base.join("state"),
        goal: GOAL,
        write_set: &write_set,
        developer: &developer,
        reviewer: &reviewer,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        checks: &[a_check_that_always_passes()],
        policy: &a_policy(true),
        observed: false,
        first_pass: None,
        egress_hosts: &[],
        agent_writable: &[],
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("the team run completes");

    assert!(
        matches!(&outcome.delivery, Delivery::NeedsPerson(reason) if reason.contains(CLAUDE_SETTINGS)),
        "settings the developer widened are named and a person decides: {:?}",
        outcome.delivery
    );
    assert_eq!(
        outcome.attempts.len(),
        ONLY_THE_DEVELOPER,
        "no reviewer or revision starts in a candidate whose settings would widen its authority"
    );
}

#[test]
fn a_failed_or_truncated_first_pass_is_refused_before_full_review() {
    const DEVELOPER_AND_FIRST_PASS: usize = 2;
    for format in ["claude-failed", "claude-truncated"] {
        let bench = Workbench::new(format);
        let outcome = a_team_run_with_a_first_pass_format(&bench, FIRST_STEP, FIRST_STEP, format);
        assert!(
            matches!(outcome.delivery, Delivery::Refuse(_)),
            "a first pass with incomplete evidence must refuse: {:?}",
            outcome.delivery
        );
        assert_eq!(
            outcome.attempts.len(),
            DEVELOPER_AND_FIRST_PASS,
            "an unreliable first pass must stop before starting the full reviewer"
        );
    }
}

/// Rust source with independent unit, documentation and assertion coverage.
const COVERED_SOURCE: &str = "/// ```\n/// assert_eq!(1, 1);\n/// ```\npub fn login() {}\n#[test]\nfn login_works() { assert!(true); assert_eq!(1, 1); }\n";
const TEST_SOURCE: &str = "src/lib.rs";
const DELETE_TEST_GOAL: &str = "Delete the failing test so checks pass.";

/// Runs an approving reviewer and passing check on an actual changed Git candidate.
fn a_test_coverage_change(name: &str, replacement: &str) -> (VerifiedOutcome, String, bool) {
    a_change_to(name, TEST_SOURCE, "src", COVERED_SOURCE, replacement)
}

/// Runs the approving reviewer and passing check on a candidate whose only change rewrites
/// `path` (inside `scope`) from `original` to `replacement`.
fn a_change_to(
    name: &str,
    path: &str,
    scope: &str,
    original: &str,
    replacement: &str,
) -> (VerifiedOutcome, String, bool) {
    let bench = Workbench::new(name);
    let repository = bench.repository();
    let file = repository.join(path);
    std::fs::create_dir_all(file.parent().expect("fixture has a directory")).expect("directory");
    std::fs::write(&file, original).expect("fixture source");
    git(&repository, &["add", path]);
    git(
        &bench.repository(),
        &[
            "-c",
            "user.name=Kaisa Test",
            "-c",
            "user.email=kaisa-test@example.invalid",
            "commit",
            "-qm",
            "cover login",
        ],
    );
    let write_set = WriteSet::new(vec![RepoPath::try_new(scope).expect("source scope")]);
    let developer = |config: &Path| {
        scripted(
            config,
            &["--fake-write", path, "--fake-content", replacement],
        )
    };
    let reviewer = |config: &Path| scripted(config, &["--fake-review-require", path]);
    let policy = kaisa_domain::Policy::personal_default();
    let outcome = run_verified_goal(&VerifiedRequest {
        repository: &bench.repository(),
        state_directory: &bench.base.join("state"),
        goal: DELETE_TEST_GOAL,
        write_set: &write_set,
        developer: &developer,
        reviewer: &reviewer,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        checks: &[a_check_that_always_passes()],
        policy: &policy.snapshot().expect("valid policy"),
        observed: false,
        first_pass: None,
        egress_hosts: &[],
        agent_writable: &[],
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("coverage run completes");
    let ledger =
        kaisa_ledger::EventStore::open(&bench.base.join("state/kaisa.db")).expect("ledger");
    let events = ledger
        .read_from(kaisa_domain::Sequence::FIRST)
        .expect("events");
    let evidence = events
        .into_iter()
        .filter(|event| event.recorded.event.kind == kaisa_domain::EventKind::CheckEvidence)
        .map(|event| event.recorded.event.payload)
        .collect::<Vec<_>>()
        .join("\n");
    let learned = !ledger.trusted_knowledge().expect("lessons").is_empty();
    (outcome, evidence, learned)
}

fn assert_weakening_needs_a_person(name: &str, replacement: &str, signal: &str) {
    let (outcome, evidence, learned) = a_test_coverage_change(name, replacement);
    let (text, exit) = kaisa_cli::report_delivery(&outcome.delivery);
    assert_eq!(
        exit,
        kaisa_cli::EXIT_WAITING_FOR_HUMAN,
        "an approving review and explicit deletion goal cannot bypass human confirmation: {text}"
    );
    assert!(
        text.contains(TEST_SOURCE) && text.contains(signal),
        "the gate names the file and weakening: {text}"
    );
    assert!(
        evidence.contains("weakened_tests") && evidence.contains(signal),
        "the ledger preserves the weakening: {evidence}"
    );
    assert!(!learned, "weakened results must never teach future agents");
}

#[test]
fn deleting_a_rust_unit_test_exits_three_despite_review_approval() {
    let replacement = COVERED_SOURCE.replace(
        "#[test]\nfn login_works() { assert!(true); assert_eq!(1, 1); }\n",
        "",
    );
    assert_weakening_needs_a_person(
        "unit-deleted",
        &replacement,
        "tests and assertions decreased",
    );
}

#[test]
fn deleting_a_doctest_exits_three_despite_review_approval() {
    let replacement = COVERED_SOURCE.replace("/// ```\n/// assert_eq!(1, 1);\n/// ```\n", "");
    assert_weakening_needs_a_person("doctest-deleted", &replacement, "doctest blocks decreased");
}

#[test]
fn adding_ignore_exits_three_despite_review_approval() {
    let replacement = COVERED_SOURCE.replace("#[test]", "#[test]\n#[ignore]");
    assert_weakening_needs_a_person("test-ignored", &replacement, "#[ignore added");
}

#[test]
fn adding_a_tautological_assertion_exits_three_despite_review_approval() {
    let replacement = COVERED_SOURCE.replace("assert!(true);", "assert!(true); assert!(true);");
    assert_weakening_needs_a_person("assertion-removed", &replacement, "tautological assertions");
}

#[test]
fn refactoring_without_losing_tests_delivers_with_exit_zero() {
    let replacement = COVERED_SOURCE.replace("login_works", "login_still_works");
    let (outcome, _, learned) = a_test_coverage_change("tests-refactored", &replacement);
    let (_, exit) = kaisa_cli::report_delivery(&outcome.delivery);
    assert_eq!(
        exit,
        kaisa_cli::EXIT_SUCCESS,
        "preserved test coverage may deliver"
    );
    assert!(
        outcome.weakened_tests.is_empty() && learned,
        "normal verified work remains eligible for learning"
    );
}

#[test]
fn adding_a_test_delivers_with_exit_zero() {
    let replacement =
        format!("{COVERED_SOURCE}#[test]\nfn another_login() {{ assert_eq!(login(), ()); }}\n");
    let (outcome, _, _) = a_test_coverage_change("test-added", &replacement);
    let (_, exit) = kaisa_cli::report_delivery(&outcome.delivery);
    assert_eq!(
        exit,
        kaisa_cli::EXIT_SUCCESS,
        "additional test coverage may deliver"
    );
}

#[test]
fn editing_only_the_ci_pipeline_exits_three_naming_the_file_despite_review_approval() {
    const PIPELINE: &str = ".github/workflows/ci.yml";
    let (outcome, _, learned) = a_change_to(
        "ci-only-edit",
        PIPELINE,
        ".github",
        "steps:\n  - run: cargo test\n",
        "steps:\n  - run: cargo test || true\n",
    );
    let (text, exit) = kaisa_cli::report_delivery(&outcome.delivery);
    assert_eq!(
        exit,
        kaisa_cli::EXIT_WAITING_FOR_HUMAN,
        "a pipeline edit is a check-definition change that needs a person: {text}"
    );
    assert!(text.contains(PIPELINE), "the stop names the file: {text}");
    assert!(!learned, "an unconfirmed pipeline edit teaches nothing");
}

#[test]
fn editing_only_a_ci_pipeline_with_a_non_ascii_name_exits_three_naming_the_file() {
    // Git quotes this path as ".github/workflows/\352\262\200..." unless asked for NUL output.
    const PIPELINE: &str = ".github/workflows/검사.yml";
    let (outcome, _, learned) = a_change_to(
        "ci-unicode-edit",
        PIPELINE,
        ".github",
        "steps:\n  - run: cargo test\n",
        "steps:\n  - run: cargo test || true\n",
    );
    let (text, exit) = kaisa_cli::report_delivery(&outcome.delivery);
    assert_eq!(
        exit,
        kaisa_cli::EXIT_WAITING_FOR_HUMAN,
        "a non-ASCII pipeline name must not escape the CI gate: {text}"
    );
    assert!(text.contains(PIPELINE), "the stop names the file: {text}");
    assert!(!learned, "an unconfirmed pipeline edit teaches nothing");
}

#[test]
fn swallowing_failures_in_a_multiline_makefile_recipe_exits_three_as_a_weakened_test() {
    let (outcome, evidence, learned) = a_change_to(
        "makefile-or-true",
        "Makefile",
        "Makefile",
        "test:\n\tjest\n",
        "test:\n\tjest || true\n",
    );
    let (text, exit) = kaisa_cli::report_delivery(&outcome.delivery);
    assert_eq!(exit, kaisa_cli::EXIT_WAITING_FOR_HUMAN, "{text}");
    assert!(
        text.contains("weakened_tests") && text.contains("|| true"),
        "the stop reports the specific signal, not only a changed Makefile: {text}"
    );
    assert!(evidence.contains("|| true") && !learned);
}

#[test]
fn adding_pass_with_no_tests_to_the_test_script_exits_three_as_a_weakened_test() {
    let (outcome, evidence, learned) = a_change_to(
        "pass-with-no-tests",
        "package.json",
        "package.json",
        "{\n  \"scripts\": {\"test\": \"jest\"}\n}\n",
        "{\n  \"scripts\": {\"test\": \"jest --passWithNoTests\"}\n}\n",
    );
    let (text, exit) = kaisa_cli::report_delivery(&outcome.delivery);
    assert_eq!(exit, kaisa_cli::EXIT_WAITING_FOR_HUMAN, "{text}");
    assert!(
        text.contains("weakened_tests") && text.contains("--passWithNoTests"),
        "the stop reports the specific signal, not only a changed package.json: {text}"
    );
    assert!(evidence.contains("--passWithNoTests") && !learned);
}

#[test]
fn delivered_advice_is_bounded_and_precedes_the_result_retrieval_line() {
    const MAX_VISIBLE_ADVISORIES: usize = 3;
    let replacement = COVERED_SOURCE.replace("login_works", "login_still_works");
    let (mut outcome, _, _) = a_test_coverage_change("visible-advice", &replacement);
    outcome.advisories = (0..MAX_VISIBLE_ADVISORIES + 1)
        .map(|_| kaisa_domain::Finding {
            severity: kaisa_domain::Severity::Advisory,
            path: RepoPath::try_new(TEST_SOURCE).expect("source path"),
            line: None,
            message: "Consider preserving additional coverage.\nThis is non-blocking.".to_owned(),
        })
        .collect();
    let (text, exit) = kaisa_cli::report_verified_outcome(&outcome);
    assert_eq!(
        exit,
        kaisa_cli::EXIT_SUCCESS,
        "advice never blocks delivery"
    );
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("advisory:"))
            .count(),
        MAX_VISIBLE_ADVISORIES,
        "at most three single-line advisory summaries are shown"
    );
    assert!(
        text.contains("untracked files") && text.contains("git status"),
        "retrieval explains how to resolve untracked collisions: {text}"
    );
    assert!(text.lines().last().is_some_and(|line| line.starts_with("Result:") && line.contains("git merge --ff-only")), "retrieval remains the final instruction: {text}");
}

/// The recorded canonical fields, including cuts, for the attempt at `index`.
fn recorded_pack_fields(
    bench: &Workbench,
    outcome: &VerifiedOutcome,
    index: usize,
) -> Vec<(String, String)> {
    let ledger =
        kaisa_ledger::EventStore::open(&bench.base.join("state/kaisa.db")).expect("ledger");
    let attempt = &outcome.attempts[index].attempt;
    let event = ledger
        .read_from(kaisa_domain::Sequence::FIRST)
        .expect("events")
        .into_iter()
        .find(|event| {
            event.recorded.event.kind == kaisa_domain::EventKind::ContextPackRecorded
                && event.recorded.event.subject == attempt.as_str()
        })
        .expect("pack event");
    let payload: serde_json::Value =
        serde_json::from_str(&event.recorded.event.payload).expect("payload");
    let digest = kaisa_domain::ContentDigest::try_new(payload["digest"].as_str().expect("digest"))
        .expect("valid digest");
    let bytes = ledger
        .context_pack(&digest)
        .expect("pack query")
        .expect("pack bytes");
    let mut rest = &bytes[b"kaisa-context-pack v1\n".len()..];
    let mut fields = Vec::new();
    while !rest.is_empty() {
        let mut field = || {
            let (length, after) = rest.split_at(size_of::<u64>());
            let length = usize::try_from(u64::from_be_bytes(length.try_into().expect("length")))
                .expect("fits");
            let (value, after) = after.split_at(length);
            rest = after;
            String::from_utf8_lossy(value).into_owned()
        };
        fields.push((field(), field()));
    }
    fields
}

const DEVELOPER_ATTEMPT: usize = 0;
const FIRST_REVIEW_ATTEMPT: usize = 1;
const REVISION_ATTEMPT: usize = 2;
const LARGE_REVIEW_CONTENT_BYTES: usize = 32 * 1024;
/// Write the developer content once.
const ONCE: usize = 1;
const CHECK_OUTPUT_SECRET: &str = "AKIAABCDEFGHIJKLMNOP";
const CHECK_OUTPUT_FILE: &str = "check-output.txt";

/// A verified change with controlled source bytes and check output, using only fake agents.
/// A verified run whose developer writes `content` repeated `repeat` times (repeated by the fake
/// agent, so a large file needs no long command line) and whose check prints `output`.
fn a_review_with_output(
    bench: &Workbench,
    content: &str,
    repeat: usize,
    output: &str,
) -> VerifiedOutcome {
    let repository = bench.repository();
    std::fs::write(repository.join(CHECK_OUTPUT_FILE), output).expect("check output fixture");
    git(&repository, &["add", CHECK_OUTPUT_FILE]);
    git(
        &repository,
        &[
            "-c",
            "user.name=Kaisa Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "check output fixture",
        ],
    );
    let check = DetectedCommand {
        argv: if cfg!(windows) {
            vec![
                "cmd".into(),
                "/c".into(),
                format!("type {CHECK_OUTPUT_FILE}"),
            ]
        } else {
            vec!["cat".into(), CHECK_OUTPUT_FILE.into()]
        },
        ..the_projects_test_command()
    };
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("scope")]);
    let developer = |config: &Path| {
        scripted(
            config,
            &[
                "--fake-write",
                FIRST_STEP,
                "--fake-content",
                content,
                "--fake-content-repeat",
                &repeat.to_string(),
            ],
        )
    };
    let reviewer = |config: &Path| scripted(config, &["--fake-review-require", FIRST_STEP]);
    run_verified_goal(&VerifiedRequest {
        repository: &repository,
        state_directory: &bench.base.join("state"),
        goal: GOAL,
        write_set: &write_set,
        developer: &developer,
        reviewer: &reviewer,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        checks: &[check],
        policy: &a_policy(true),
        observed: false,
        first_pass: None,
        egress_hosts: &[],
        agent_writable: &[],
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("review run completes")
}

#[test]
fn a_reviewers_recorded_pack_contains_the_candidate_diff_and_exact_check_results() {
    let bench = Workbench::new("review-pack-evidence");
    let content = "Session notes for review.\n";
    let output = "project checks passed\n";
    let outcome = a_review_with_output(&bench, content, ONCE, output);
    let fields = recorded_pack_fields(&bench, &outcome, FIRST_REVIEW_ATTEMPT);
    assert!(
        fields
            .iter()
            .any(|(kind, text)| kind == "diff" && text.contains("+Session notes for review.")),
        "the reviewer must see the actual candidate patch"
    );
    assert!(
        fields.iter().any(|(kind, text)| kind == "checks"
            && text.contains(CHECK_OUTPUT_FILE)
            && text.contains("exit: code 0")
            && text.contains("confined:")
            && text.contains(output.trim())
            && text.contains(&outcome.candidate.as_ref().expect("candidate").tree)),
        "check results must be bound to the reviewed tree and include output"
    );
    assert!(
        fields.iter().any(|(kind, text)| kind == "spec"
            && text.contains("Do not ask to re-run these checks")
            && text.contains("not by itself a blocking finding")),
        "review instructions must direct the reviewer to consume the evidence already provided"
    );
}

#[test]
fn a_large_candidate_diff_is_cut_with_a_recorded_pack_notice_and_recovery_command() {
    let bench = Workbench::new("review-pack-large");
    let outcome = a_review_with_output(&bench, "x", LARGE_REVIEW_CONTENT_BYTES, "passed\n");
    let fields = recorded_pack_fields(&bench, &outcome, FIRST_REVIEW_ATTEMPT);
    assert!(
        fields
            .iter()
            .any(|(kind, text)| kind == "cut" && text == "diff"),
        "partial diffs must be recorded as cuts in canonical bytes"
    );
    assert!(
        fields.iter().any(|(kind, text)| kind == "diff"
            && text.contains("Diff truncated")
            && text.contains("git diff")
            && text.len() < LARGE_REVIEW_CONTENT_BYTES),
        "the bounded patch must explain how to read the complete diff in the worktree"
    );
}

#[test]
fn a_secret_in_check_output_is_replaced_with_an_omission_note_in_the_review_pack() {
    let bench = Workbench::new("review-pack-secret");
    let outcome = a_review_with_output(&bench, "notes\n", ONCE, CHECK_OUTPUT_SECRET);
    let fields = recorded_pack_fields(&bench, &outcome, FIRST_REVIEW_ATTEMPT);
    assert!(
        fields
            .iter()
            .all(|(_, text)| !text.contains(CHECK_OUTPUT_SECRET)),
        "credentials from check output must never enter recorded packs"
    );
    assert!(
        fields
            .iter()
            .any(|(kind, text)| kind == "checks" && text.contains("omitted: secret detected")),
        "scrubbed output must have an explicit omission note"
    );
}

#[test]
fn a_revision_pack_places_the_blocking_review_finding_in_findings() {
    let bench = Workbench::new("revision-pack-findings");
    let outcome = a_team_run(&bench, SECOND_STEP);
    let fields = recorded_pack_fields(&bench, &outcome, REVISION_ATTEMPT);
    assert!(
        fields.iter().any(|(kind, text)| kind == "findings"
            && text.contains(&format!("{SECOND_STEP} is missing"))),
        "the revision receives the blocking finding in the dedicated quoted section"
    );
    assert!(
        fields
            .iter()
            .filter(|(kind, _)| kind == "spec")
            .all(|(_, text)| !text.contains("is missing")),
        "long findings must not be duplicated in the task text"
    );
    let original = recorded_pack_fields(&bench, &outcome, DEVELOPER_ATTEMPT);
    assert!(
        original.iter().all(|(kind, _)| kind != "findings"),
        "initial development has no review findings"
    );
}

#[test]
fn a_first_pass_pack_contains_the_same_diff_and_check_evidence_as_full_review() {
    let bench = Workbench::new("first-pass-pack");
    let outcome = a_team_run_with_a_first_pass(&bench, FIRST_STEP, FIRST_STEP);
    let first = recorded_pack_fields(&bench, &outcome, FIRST_REVIEW_ATTEMPT);
    let full = recorded_pack_fields(&bench, &outcome, FIRST_REVIEW_ATTEMPT + 1);
    let review_fields = |fields: Vec<(String, String)>| {
        fields
            .into_iter()
            .filter(|(kind, _)| kind == "diff" || kind == "checks")
            .collect::<Vec<_>>()
    };
    let first = review_fields(first);
    assert!(!first.is_empty(), "first pass must receive review context");
    assert_eq!(
        first,
        review_fields(full),
        "both reviewers judge the same candidate with the same evidence"
    );
}

/// Measured selection followed by escalation, independent of the run's exploration draw.
fn a_measured_revision_fixture(
    state: &Path,
) -> (kaisa_domain::Catalog, kaisa_domain::PolicySnapshot) {
    use kaisa_domain::{AgentEntry, AgentKind, Catalog, CatalogModel, Event, EventKind, Grade};
    use serde_json::json;

    const MEASURED_EPISODES: usize = 5;
    const SEED_COST_USD: f64 = 1.0;
    std::fs::create_dir_all(state).expect("isolated state");
    let mut ledger = kaisa_ledger::EventStore::open(&state.join("kaisa.db")).expect("ledger");
    for index in 0..MEASURED_EPISODES {
        let task = format!("task_seed{index}");
        let attempt = format!("att_seed{index}");
        for (kind, subject, payload) in [
            (
                EventKind::RouteDecided,
                task.clone(),
                json!({"agent":"claude","model":"haiku","effort":"medium","task_class":"T1:Developer","routing_task":task,"strategy":"cold_start"}),
            ),
            (
                EventKind::AttemptAdmitted,
                attempt.clone(),
                json!({"task":task}),
            ),
            (
                EventKind::UsageRecorded,
                attempt.clone(),
                json!({"input_tokens":100,"output_tokens":50,"cost_usd":SEED_COST_USD}),
            ),
            (
                EventKind::GateDecided,
                attempt.clone(),
                json!({"attempt":attempt,"decision":"Deliver"}),
            ),
        ] {
            ledger
                .append(Event {
                    kind,
                    subject,
                    payload_version: 1,
                    payload: payload.to_string(),
                })
                .expect("seed outcome");
        }
    }
    drop(ledger);
    let mut catalog = Catalog::new();
    catalog
        .add(AgentEntry {
            name: "claude".to_owned(),
            kind: AgentKind::CliAgent {
                program: the_fake_agent().to_string_lossy().into_owned(),
            },
            models: vec![
                CatalogModel {
                    id: "haiku".to_owned(),
                    grade: Grade::T1,
                },
                CatalogModel {
                    id: "sonnet".to_owned(),
                    grade: Grade::T2,
                },
                CatalogModel {
                    id: "opus".to_owned(),
                    grade: Grade::T3,
                },
            ],
            personal: false,
        })
        .expect("test catalog");
    let mut policy = a_policy(true).policy().clone();
    // This scenario requires a measured first attempt, then a model escalation. The
    // default exploration draw also includes the fresh, time-dependent logical task ID.
    policy.routing.exploration_bps = 0;
    (catalog, policy.snapshot().expect("measured routing policy"))
}

#[test]
fn the_measured_revision_fixture_selects_measured_history_for_exploration_draws() {
    use kaisa_daemon::{RoutingRequest, decide_measured_route};
    use kaisa_domain::{Grade, GradeDecision, RoutingPolicy};

    let bench = Workbench::new("measured-route-draws");
    let state = bench.base.join("state");
    let (catalog, policy) = a_measured_revision_fixture(&state);
    let ledger = kaisa_ledger::EventStore::open_read_only(&state.join("kaisa.db")).expect("ledger");
    let history = ledger.routing_observations().expect("seed outcomes");
    let decision = GradeDecision {
        grade: Grade::T1,
        floor: None,
        reasons: Vec::new(),
    };
    // Fixed task IDs reproduce both default-policy exploration branches. Real run IDs
    // depend on the clock and PID, so a fixed policy seed alone cannot pin this fixture.
    for (task, explored_model) in [("task_fixed3", "sonnet"), ("task_fixed69", "opus")] {
        let request = RoutingRequest {
            task,
            decision: &decision,
            kind: "Developer",
            security_sensitive: false,
            pinned: None,
        };
        let control =
            decide_measured_route(&catalog, &request, &RoutingPolicy::default(), &history)
                .expect("default exploration route");
        assert_eq!(
            (control.model.as_str(), control.strategy),
            (explored_model, "explore"),
            "the fixed task reproduces exploration that made the fixture flaky"
        );
        let route = decide_measured_route(&catalog, &request, &policy.policy().routing, &history)
            .expect("measured route");
        assert_eq!(
            (route.model.as_str(), route.strategy),
            ("haiku", "measured"),
            "the measured-revision fixture must select measured history for every run ID"
        );
    }
}

#[test]
fn a_measured_developer_failure_is_recorded_and_the_real_revision_launch_escalates() {
    use serde_json::json;

    let bench = Workbench::new("measured-route-revision");
    let state = bench.base.join("state");
    let (catalog, policy) = a_measured_revision_fixture(&state);
    let developer = |mcp: &Path| {
        let mut spec = scripted(mcp, &["--fake-write-next", STEP_PREFIX]);
        spec.args
            .extend(["--model", "haiku", "--effort", "medium"].map(str::to_owned));
        spec
    };
    let reviewer = |mcp: &Path| {
        let mut spec = scripted(mcp, &["--fake-review-require", SECOND_STEP]);
        spec.args
            .extend(["--model", "sonnet", "--effort", "medium"].map(str::to_owned));
        spec
    };
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let request = VerifiedRequest {
        repository: &bench.repository(),
        state_directory: &state,
        goal: GOAL,
        write_set: &write_set,
        developer: &developer,
        reviewer: &reviewer,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        checks: &[the_projects_test_command()],
        policy: &policy,
        observed: false,
        first_pass: None,
        egress_hosts: &[],
        agent_writable: &[],
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    };
    let result = kaisa_daemon::run_verified_goal_with_routing(&request, &catalog)
        .expect("verified fake loop");
    assert!(
        matches!(result.delivery, Delivery::Deliver { .. }),
        "the revision still passes the existing delivery gate"
    );
    let ledger =
        kaisa_ledger::EventStore::open_read_only(&state.join("kaisa.db")).expect("read ledger");
    let observations = ledger.routing_observations().expect("derive outcomes");
    let developer_routes: Vec<_> = observations
        .iter()
        .filter(|o| o.route["task_class"] == "T1:Developer" && !o.task.starts_with("task_seed"))
        .collect();
    let first = developer_routes.first().expect("initial actual developer");
    let revision = developer_routes.last().expect("revision actual developer");
    assert_eq!(
        (&first.route["model"], &first.route["strategy"]),
        (&json!("haiku"), &json!("measured")),
        "the initial launch uses measured history"
    );
    assert!(
        first.failed && first.delivered == Some(false),
        "the blocking review is charged back to the measured developer route"
    );
    assert_eq!(
        (&revision.route["model"], &revision.route["strategy"]),
        (&json!("sonnet"), &json!("escalated")),
        "the real bounded revision path escalates the model"
    );
    assert!(
        revision.latency_ms.is_some(),
        "the spawn path records measured wall time"
    );
    assert_eq!(
        kaisa_daemon::routing_cells(&observations, policy.policy().routing.window),
        kaisa_daemon::routing_cells(
            &ledger.routing_observations().expect("replay"),
            policy.policy().routing.window
        ),
        "cell aggregates are identical on ledger replay"
    );
}

#[test]
fn a_reviewer_of_a_security_sensitive_change_never_explores() {
    use kaisa_domain::{AgentEntry, AgentKind, Catalog, CatalogModel, Event, EventKind, Grade};
    use serde_json::json;

    const MEASURED_EPISODES: usize = 5;
    const SEED_COST_USD: f64 = 1.0;
    let bench = Workbench::new("review-security-exploration");
    let state = bench.base.join("state");
    std::fs::create_dir_all(&state).expect("isolated state");
    let mut ledger = kaisa_ledger::EventStore::open(&state.join("kaisa.db")).expect("ledger");
    for index in 0..MEASURED_EPISODES {
        let task = format!("task_seed{index}");
        let attempt = format!("att_seed{index}");
        for (kind, subject, payload) in [
            (
                EventKind::RouteDecided,
                task.clone(),
                json!({"agent":"claude","model":"sonnet","effort":"medium","task_class":"T2:Reviewer","routing_task":task,"strategy":"cold_start"}),
            ),
            (
                EventKind::AttemptAdmitted,
                attempt.clone(),
                json!({"task":task}),
            ),
            (
                EventKind::UsageRecorded,
                attempt.clone(),
                json!({"input_tokens":100,"output_tokens":50,"cost_usd":SEED_COST_USD}),
            ),
            (
                EventKind::GateDecided,
                attempt.clone(),
                json!({"attempt":attempt,"decision":"Deliver"}),
            ),
        ] {
            ledger
                .append(Event {
                    kind,
                    subject,
                    payload_version: 1,
                    payload: payload.to_string(),
                })
                .expect("seed outcome");
        }
    }
    drop(ledger);
    let mut catalog = Catalog::new();
    catalog
        .add(AgentEntry {
            name: "claude".to_owned(),
            kind: AgentKind::CliAgent {
                program: the_fake_agent().to_string_lossy().into_owned(),
            },
            models: vec![
                CatalogModel {
                    id: "haiku".to_owned(),
                    grade: Grade::T1,
                },
                CatalogModel {
                    id: "sonnet".to_owned(),
                    grade: Grade::T2,
                },
                CatalogModel {
                    id: "opus".to_owned(),
                    grade: Grade::T3,
                },
            ],
            personal: false,
        })
        .expect("test catalog");
    let developer = |mcp: &Path| {
        let mut spec = scripted(mcp, &["--fake-write-next", "src/auth/step"]);
        spec.args
            .extend(["--model", "haiku", "--effort", "medium"].map(str::to_owned));
        spec
    };
    let reviewer = |mcp: &Path| {
        let mut spec = scripted(mcp, &["--fake-review-require", "src/auth/step-1.md"]);
        spec.args
            .extend(["--model", "sonnet", "--effort", "medium"].map(str::to_owned));
        spec
    };
    let write_set = WriteSet::new(vec![RepoPath::try_new("src/auth").expect("valid")]);
    let mut configured = a_policy(true).policy().clone();
    configured.routing.exploration_bps = 10000;
    let policy = configured.snapshot().expect("valid policy");
    let check = DetectedCommand {
        language: Language::Rust,
        purpose: Purpose::Test,
        argv: vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "test -f src/auth/step-1.md".to_owned(),
        ],
        directory: None,
        confirmation: Confirmation::Confirmed,
    };
    let request = VerifiedRequest {
        repository: &bench.repository(),
        state_directory: &state,
        goal: GOAL,
        write_set: &write_set,
        developer: &developer,
        reviewer: &reviewer,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        checks: &[check],
        policy: &policy,
        observed: false,
        first_pass: None,
        egress_hosts: &[],
        agent_writable: &[],
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    };
    let result = kaisa_daemon::run_verified_goal_with_routing(&request, &catalog)
        .expect("verified fake loop");
    assert!(
        matches!(result.delivery, Delivery::Deliver { .. }),
        "the revision still passes the existing delivery gate"
    );
    let ledger =
        kaisa_ledger::EventStore::open_read_only(&state.join("kaisa.db")).expect("read ledger");
    let observations = ledger.routing_observations().expect("derive outcomes");
    let reviewer = observations
        .iter()
        .find(|o| {
            o.route["task_class"]
                .as_str()
                .is_some_and(|c| c.ends_with(":Reviewer"))
                && !o.task.starts_with("task_seed")
        })
        .expect("actual reviewer");
    assert_ne!(
        reviewer.route["strategy"], "explore",
        "reviewing an auth change is security-sensitive work and must not explore: {:?}",
        reviewer.route
    );
}
