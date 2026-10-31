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
        revision_limit: DEFAULT_REVISION_LIMIT,
        allowed_egress_hosts: Vec::new(),
        confine_agents,
        min_grade_for_security_review: kaisa_domain::Grade::T3,
        require_person_when_check_definitions_change: true,
        require_person_when_tests_weaken: true,
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
