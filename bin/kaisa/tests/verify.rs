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
