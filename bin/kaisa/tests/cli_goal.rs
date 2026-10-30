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
    fn new(name: &str) -> Self {
        let base: PathBuf =
            std::env::temp_dir().join(format!("kaisa-cli-goal-{name}-{}", std::process::id()));
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
    let bench = Workbench::new("summary");
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

/// How long the killed run may take to reach the agent's brokered write.
#[cfg(unix)]
const WRITE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

/// The goal of the run that gets killed.
#[cfg(unix)]
const INTERRUPTED_GOAL: &str = "Write the session notes.";

#[cfg(unix)]
#[test]
fn an_attempt_abandoned_by_a_killed_kaisa_is_recovered_on_the_next_run() {
    let bench = Workbench::new("recovery");
    let repository: PathBuf = bench.base.join("repo");
    let state: PathBuf = bench.base.join("state");
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let kaisa_running = |goal: &str, extra: &[&str]| {
        let mut args: Vec<String> = vec![
            "--agent-program".to_owned(),
            fake_agent.to_string_lossy().into_owned(),
            "--agent-arg".to_owned(),
            "--fake-write".to_owned(),
            "--agent-arg".to_owned(),
            "src/notes.md".to_owned(),
        ];
        for flag in extra {
            args.extend(["--agent-arg".to_owned(), (*flag).to_owned()]);
        }
        args.push(goal.to_owned());
        Command::new(env!("CARGO_BIN_EXE_kaisa"))
            .args(args)
            .current_dir(&repository)
            .env("KAISA_STATE_DIR", &state)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("kaisa starts")
    };
    let mut interrupted = kaisa_running(INTERRUPTED_GOAL, &["--hang-after-write"]);
    let deadline = std::time::Instant::now() + WRITE_PATIENCE;
    let written = || {
        std::fs::read_dir(state.join("worktrees"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|worktree| worktree.path().join("src/notes.md").exists())
    };
    while !written() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    interrupted.kill().expect("kaisa is killed mid-attempt");
    interrupted.wait().expect("reaped");

    let next: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args([
            "--agent-program",
            &fake_agent.to_string_lossy(),
            "--agent-arg",
            "--fake-write",
            "--agent-arg",
            "src/more.md",
            GOAL,
        ])
        .current_dir(&repository)
        .env("KAISA_STATE_DIR", &state)
        .output()
        .expect("kaisa runs again");
    let stdout: String = String::from_utf8_lossy(&next.stdout).into_owned();
    let leftover_worktrees: usize = std::fs::read_dir(state.join("worktrees"))
        .map(|entries| entries.count())
        .unwrap_or(0);

    assert!(
        stdout.contains("recovered att_") && stdout.contains("was ended"),
        "the next run ends the abandoned agent and says so; got {stdout:?}"
    );
    assert!(
        stdout.contains("work preserved on kaisa/att_") && leftover_worktrees == 0,
        "the abandoned attempt's work is preserved and no worktree is left; got {stdout:?}"
    );
}
