//! Trusted checks on a frozen candidate (T21b).

use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_context::{Confirmation, DetectedCommand, Language, Purpose};
use kaisa_daemon::{Evidence, run_trusted_checks};
use kaisa_workspace::{Candidate, freeze_candidate};

/// The file the project's test command requires.
const REQUIRED_FILE: &str = "src/ok.txt";

fn git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(directory)
        .status()
        .expect("git is installed on every CI runner");
    assert!(status.success(), "git {args:?} failed");
}

fn commit_all(repository: &Path, message: &str) {
    git(repository, &["add", "."]);
    git(
        repository,
        &[
            "-c",
            "user.name=Kaisa Test",
            "-c",
            "user.email=kaisa-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    );
}

/// A repository with one commit, removed on drop.
struct Workbench {
    base: PathBuf,
}

impl Workbench {
    fn new(name: &str) -> Self {
        let base: PathBuf =
            std::env::temp_dir().join(format!("kaisa-verify-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("repo/src")).expect("writable temp dir");
        std::fs::write(base.join("repo/src/lib.rs"), "pub fn login() {}\n").expect("written");
        git(
            &base.join("repo"),
            &["init", "--quiet", "--initial-branch=main"],
        );
        commit_all(&base.join("repo"), "initial");
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

/// The project's test command: passes only if the required file exists.
fn the_projects_test_command() -> DetectedCommand {
    let argv: Vec<String> = if cfg!(windows) {
        vec![
            "cmd".to_owned(),
            "/c".to_owned(),
            r"if exist src\ok.txt (exit 0) else (exit 1)".to_owned(),
        ]
    } else {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("test -f {REQUIRED_FILE}"),
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

#[test]
fn trusted_checks_run_on_a_clean_checkout_not_the_agents_worktree() {
    let bench = Workbench::new("clean");
    let candidate: Candidate = freeze_candidate(&bench.repository(), "HEAD").expect("frozen");
    // Left behind but never committed: it must not make the check pass.
    std::fs::write(bench.repository().join(REQUIRED_FILE), "uncommitted").expect("written");

    let evidence: Vec<Evidence> = run_trusted_checks(
        &bench.repository(),
        &bench.base.join("state"),
        &bench.base.join("state/attempts/att_developer"),
        "main",
        &candidate,
        &[the_projects_test_command()],
    )
    .expect("the checks run");

    assert_eq!(evidence.len(), 1, "one test command, one piece of evidence");
    assert!(
        !evidence[0].passed(),
        "the check sees only the candidate commit, which lacks the file: {evidence:?}"
    );
}

#[test]
fn a_passing_check_is_recorded_against_the_candidate_tree() {
    let bench = Workbench::new("passing");
    std::fs::write(bench.repository().join(REQUIRED_FILE), "committed").expect("written");
    commit_all(&bench.repository(), "candidate");
    let candidate: Candidate = freeze_candidate(&bench.repository(), "HEAD").expect("frozen");

    let evidence: Vec<Evidence> = run_trusted_checks(
        &bench.repository(),
        &bench.base.join("state"),
        &bench.base.join("state/attempts/att_developer"),
        "main",
        &candidate,
        &[the_projects_test_command()],
    )
    .expect("the checks run");

    assert!(
        evidence.iter().all(Evidence::passed)
            && evidence
                .iter()
                .all(|piece| piece.candidate_tree == candidate.tree),
        "passing evidence names exactly the tree it was produced on: {evidence:?}"
    );
}

#[test]
fn a_candidate_changed_after_review_needs_new_evidence() {
    let bench = Workbench::new("tamper");
    std::fs::write(bench.repository().join(REQUIRED_FILE), "committed").expect("written");
    commit_all(&bench.repository(), "reviewed candidate");
    let reviewed: Candidate = freeze_candidate(&bench.repository(), "HEAD").expect("frozen");
    let evidence: Vec<Evidence> = run_trusted_checks(
        &bench.repository(),
        &bench.base.join("state"),
        &bench.base.join("state/attempts/att_developer"),
        "main",
        &reviewed,
        &[the_projects_test_command()],
    )
    .expect("the checks run");
    std::fs::write(
        bench.repository().join("src/lib.rs"),
        "pub fn login() { todo!() }\n",
    )
    .expect("written");
    commit_all(&bench.repository(), "changed after review");

    let changed: Candidate = freeze_candidate(&bench.repository(), "HEAD").expect("frozen");

    assert!(
        evidence.iter().all(|piece| piece.applies_to(&reviewed)),
        "the evidence holds for the candidate it was produced on"
    );
    assert!(
        !evidence.iter().any(|piece| piece.applies_to(&changed)),
        "after any change the old evidence says nothing about the new content"
    );
}

#[cfg(unix)]
fn a_shell_check(script: &str) -> DetectedCommand {
    let mut check = the_projects_test_command();
    check.argv = vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()];
    check
}

#[cfg(unix)]
#[test]
fn a_check_can_write_its_private_tmpdir_and_create_new_build_output() {
    let bench = Workbench::new("private-temp");
    let candidate = freeze_candidate(&bench.repository(), "HEAD").expect("frozen");
    let check = a_shell_check(
        "test -n \"$TMPDIR\" && test -z \"${TMPDIR##*/attempts/*}\" && \
         test \"$TMPDIR\" = \"$TEMP\" && test \"$TMPDIR\" = \"$TMP\" && mkdir \"$TMPDIR/probe\" && echo ok > \"$TMPDIR/probe/result\" && \
         mkdir -p target && echo built > target/output",
    );
    let evidence = run_trusted_checks(
        &bench.repository(),
        &bench.base.join("state"),
        &bench.base.join("state/attempts/att_developer"),
        "main",
        &candidate,
        &[check.clone(), check],
    )
    .expect("the check starts");
    assert!(
        evidence.iter().all(Evidence::passed),
        "checks need a private writable TMPDIR and a writable disposable checkout: {evidence:?}"
    );
    assert!(
        std::fs::read_dir(bench.base.join("state/attempts/att_developer"))
            .expect("the attempt directory exists")
            .next()
            .is_none(),
        "each check's temporary directory is reclaimed after its process ends"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_check_cannot_write_the_original_repository_or_other_private_state() {
    let bench = Workbench::new("outside-write");
    let state = bench.base.join("state");
    let other_attempt = state.join("attempts/att_other");
    std::fs::create_dir_all(&other_attempt).expect("private state exists");
    let candidate = freeze_candidate(&bench.repository(), "HEAD").expect("frozen");
    let checks: Vec<_> = [bench.repository(), state.clone(), other_attempt]
        .iter()
        .map(|outside| {
            a_shell_check(&format!(
                "echo forbidden > '{}'",
                outside.join("forbidden").display()
            ))
        })
        .collect();
    let evidence = run_trusted_checks(
        &bench.repository(),
        &state,
        &state.join("attempts/att_developer"),
        "main",
        &candidate,
        &checks,
    )
    .expect("the checks start");
    assert!(
        evidence
            .iter()
            .all(|piece| piece.confined && !piece.passed()),
        "every write outside this check's grants must be denied: {evidence:?}"
    );
    assert!(
        !bench.repository().join("forbidden").exists(),
        "the user's original repository must remain untouched"
    );
}

#[cfg(unix)]
#[test]
fn a_check_that_changes_candidate_sources_refuses_all_evidence_before_the_next_check() {
    let bench = Workbench::new("changed-by-check");
    let candidate = freeze_candidate(&bench.repository(), "HEAD").expect("frozen");
    let mut unreachable_check = a_shell_check("exit 0");
    unreachable_check.argv = vec![
        bench
            .base
            .join("must-not-run")
            .to_string_lossy()
            .into_owned(),
    ];
    let checks = [
        a_shell_check("test -f src/lib.rs"),
        a_shell_check("echo changed > src/lib.rs"),
        unreachable_check,
    ];
    let result = run_trusted_checks(
        &bench.repository(),
        &bench.base.join("state"),
        &bench.base.join("state/attempts/att_developer"),
        "main",
        &candidate,
        &checks,
    );
    let evidence = result.expect("source mutation is a normal check failure");
    let changed = evidence
        .last()
        .expect("the changing check produced evidence");
    assert!(
        !changed.passed()
            && !changed.applies_to(&candidate)
            && changed.failure_reason.as_deref()
                == Some(
                    "체크가 후보 소스를 바꿨다. 체크 정의를 확인하라(예: 포매터를 --check 모드로 실행)."
                ),
        "source mutation must invalidate evidence and stop before another check can run: {evidence:?}"
    );
    assert_eq!(
        kaisa_daemon::decide_delivery(&candidate, &evidence, None),
        kaisa_daemon::Delivery::Refuse(
            "체크가 후보 소스를 바꿨다. 체크 정의를 확인하라(예: 포매터를 --check 모드로 실행)."
                .to_owned()
        ),
        "a mutated candidate must be refused with its specific reason"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_check_cannot_overwrite_its_root_git_controls() {
    let bench = Workbench::new("git-controls");
    let candidate = freeze_candidate(&bench.repository(), "HEAD").expect("frozen");
    let checks: Vec<_> = [".git", ".gitattributes", ".gitmodules"]
        .iter()
        .map(|control| a_shell_check(&format!("echo tampered > {control}")))
        .collect();
    let evidence = run_trusted_checks(
        &bench.repository(),
        &bench.base.join("state"),
        &bench.base.join("state/attempts/att_developer"),
        "main",
        &candidate,
        &checks,
    )
    .expect("denied control writes leave the checkout intact");
    assert!(
        evidence
            .iter()
            .all(|piece| piece.confined && !piece.passed()),
        "a checkout root grant must never authorize its Git pointer or control files: {evidence:?}"
    );
}

#[cfg(unix)]
#[test]
fn the_cli_refuses_a_source_changing_check_and_tells_the_user_to_fix_its_definition() {
    let bench = Workbench::new("changed-check-cli");
    let fake_agent = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let run = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args([
            "--agent-program",
            &fake_agent.to_string_lossy(),
            "--agent-arg",
            "--fake-write",
            "--agent-arg",
            "src/notes.md",
            "--reviewer-arg",
            "--fake-review-require",
            "--reviewer-arg",
            "src/notes.md",
            "--check-program",
            "sh",
            "--check-arg",
            "-c",
            "--check-arg",
            "echo changed > src/lib.rs",
            "Add notes about the login flow.",
        ])
        .current_dir(bench.repository())
        .env("KAISA_STATE_DIR", bench.base.join("state"))
        .output()
        .expect("the CLI runs");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(
        run.status.code(),
        Some(i32::from(kaisa_cli::EXIT_RUN_FAILED)),
        "source-changing checks must refuse delivery: {run:?}"
    );
    assert!(
        stdout.contains("not delivered: 체크가 후보 소스를 바꿨다")
            && stdout.contains("체크 정의를 확인하라")
            && stdout.contains("--check")
            && !stdout.contains("problem(s) remain after the revisions"),
        "the CLI must explain the refused check without starting a revision: {stdout}"
    );
}
