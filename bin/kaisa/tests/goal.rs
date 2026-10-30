//! `run_goal` end to end with a scripted agent: mission, pack, worktree, agent through the real
//! bridge, exit record, preservation, reclamation.

use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_daemon::{GoalOutcome, GoalRequest, run_goal};
use kaisa_domain::{RepoPath, WriteSet};
use kaisa_sys::{ProcessExit, ProcessSpec};

/// What the user asks for; the fake agent writes the start of its pack into a file.
const GOAL: &str = "Add notes about the login flow.";
/// The file the fake agent writes.
const NOTES: &str = "src/notes.md";

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .expect("git is installed on every CI runner");
    assert!(output.status.success(), "git {args:?} failed: {output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repository with one commit and a `src` directory, plus a state directory, removed on drop.
struct Workbench {
    base: PathBuf,
}

impl Workbench {
    fn new(name: &str) -> Self {
        let base: PathBuf =
            std::env::temp_dir().join(format!("kaisa-goal-{name}-{}", std::process::id()));
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

    fn repository(&self) -> PathBuf {
        self.base.join("repo")
    }

    fn state(&self) -> PathBuf {
        self.base.join("state")
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

#[test]
fn a_goal_runs_to_a_preserved_result_with_a_scripted_agent() {
    let bench = Workbench::new("preserved");
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let launch = |mcp_config: &Path| ProcessSpec {
        program: the_fake_agent(),
        args: vec![
            "--mcp-config".to_owned(),
            mcp_config.to_string_lossy().into_owned(),
            "--fake-write".to_owned(),
            NOTES.to_owned(),
        ],
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    };

    let outcome: GoalOutcome = run_goal(&GoalRequest {
        repository: &bench.repository(),
        goal: GOAL,
        state_directory: &bench.state(),
        write_set: &write_set,
        launch: &launch,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
    })
    .expect("the goal runs");
    let preserved_notes: String = git(
        &bench.repository(),
        &["show", &format!("{}:{NOTES}", outcome.branch)],
    );

    assert_eq!(
        (
            outcome.exit,
            outcome.commit.is_some(),
            outcome.kept_back.is_empty()
        ),
        (Some(ProcessExit::Code(0)), true, true),
        "the agent finished and its work was preserved, got {outcome:?}"
    );
    assert!(
        preserved_notes.contains(GOAL),
        "the preserved file shows the agent started from the context pack with the goal"
    );
    assert!(
        !bench
            .state()
            .join("worktrees")
            .join(outcome.attempt.as_str())
            .exists(),
        "the worktree is reclaimed once the work is on its branch"
    );
}

/// Where the probe tries to write around the broker, relative to the agent's worktree.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const DIRECT_WRITE: &str = "src/direct.md";

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_brokered_agent_process_cannot_write_the_worktree_directly() {
    let bench = Workbench::new("confined");
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let launch = |mcp_config: &Path| ProcessSpec {
        program: the_fake_agent(),
        args: vec![
            "--mcp-config".to_owned(),
            mcp_config.to_string_lossy().into_owned(),
            "--fake-write".to_owned(),
            NOTES.to_owned(),
            "--try-direct-write".to_owned(),
            DIRECT_WRITE.to_owned(),
        ],
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    };

    let outcome: GoalOutcome = run_goal(&GoalRequest {
        repository: &bench.repository(),
        goal: GOAL,
        state_directory: &bench.state(),
        write_set: &write_set,
        launch: &launch,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
    })
    .expect("the goal runs");
    let preserved: String = git(
        &bench.repository(),
        &["ls-tree", "-r", "--name-only", &outcome.branch],
    );

    assert!(
        outcome.confined && outcome.exit == Some(ProcessExit::Code(0)),
        "the probe's own write was refused, so the agent went on through the broker: {outcome:?}"
    );
    assert!(
        preserved.contains(NOTES) && !preserved.contains(DIRECT_WRITE),
        "only the brokered change reached the preserved branch, got {preserved:?}"
    );
}
