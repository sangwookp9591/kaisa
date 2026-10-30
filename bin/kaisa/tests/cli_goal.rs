//! `kaisa "<goal>"` from a user's terminal, end to end, with a scripted agent.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// What the user asks for.
const GOAL: &str = "Add notes about the login flow.";
/// Where the summary says the work went.
const PRESERVED_ON: &str = "work preserved on kaisa/att_";

fn git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(directory)
        .status()
        .expect("git is installed on every CI runner");
    assert!(status.success(), "git {args:?} failed");
}

/// A repository with one commit and a `src` directory, removed on drop.
struct Workbench {
    base: PathBuf,
}

impl Workbench {
    fn new() -> Self {
        let base: PathBuf =
            std::env::temp_dir().join(format!("kaisa-cli-goal-{}", std::process::id()));
        let repository: PathBuf = base.join("repo");
        std::fs::create_dir_all(repository.join("src")).expect("writable temp dir");
        std::fs::write(repository.join("src/lib.rs"), "pub fn login() {}\n").expect("written");
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
}

impl Drop for Workbench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[test]
fn kaisa_with_a_goal_prints_where_the_preserved_work_is() {
    let bench = Workbench::new();
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));

    let run: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args([
            "--agent-program",
            &fake_agent.to_string_lossy(),
            "--agent-arg",
            "--fake-write",
            "--agent-arg",
            "src/notes.md",
            GOAL,
        ])
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", bench.base.join("state"))
        .output()
        .expect("kaisa runs");
    let stdout: String = String::from_utf8_lossy(&run.stdout).into_owned();

    assert!(
        run.status.success() && stdout.contains(PRESERVED_ON),
        "one line tells the user the agent finished and where its work is; got {stdout:?} {:?}",
        String::from_utf8_lossy(&run.stderr)
    );
}
