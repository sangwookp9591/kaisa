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
    }
    .snapshot()
    .expect("a valid policy")
}

fn a_team_run(bench: &Workbench, reviewer_requires: &str) -> VerifiedOutcome {
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let developer = |mcp_config: &Path| scripted(mcp_config, &["--fake-write-next", STEP_PREFIX]);
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
