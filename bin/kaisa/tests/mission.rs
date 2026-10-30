//! A planned mission end to end: tasks run in dependency order, each starting from the work of
//! the task it depends on (T20d).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_daemon::{MissionOutcome, MissionRequest, run_mission};
use kaisa_domain::{Plan, PlannedTask, RepoPath, Role, TaskId, WriteSet};
use kaisa_sys::ProcessSpec;

/// Where the scripted agents write their step files.
const STEP_PREFIX: &str = "src/step";

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .expect("git is installed on every CI runner");
    assert!(output.status.success(), "git {args:?} failed: {output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn task(id: &str) -> TaskId {
    TaskId::try_new(id).expect("a valid task id")
}

#[test]
fn a_two_task_mission_runs_both_attempts_in_dependency_order() {
    let base: PathBuf = std::env::temp_dir().join(format!("kaisa-mission-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
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
    let src = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let plan = Plan::new(vec![
        PlannedTask {
            id: task("task_first"),
            role: Role::Developer,
            write_set: src.clone(),
            depends_on: Vec::new(),
        },
        PlannedTask {
            id: task("task_second"),
            role: Role::Developer,
            write_set: src,
            depends_on: vec![task("task_first")],
        },
    ])
    .expect("a valid plan");
    let instructions: BTreeMap<TaskId, String> = [
        (task("task_first"), "Write the first step.".to_owned()),
        (task("task_second"), "Write the next step.".to_owned()),
    ]
    .into_iter()
    .collect();
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let launch = |mcp_config: &Path| ProcessSpec {
        program: fake_agent.clone(),
        args: vec![
            "--mcp-config".to_owned(),
            mcp_config.to_string_lossy().into_owned(),
            "--fake-write-next".to_owned(),
            STEP_PREFIX.to_owned(),
        ],
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    };

    let outcome: MissionOutcome = run_mission(&MissionRequest {
        repository: &repository,
        state_directory: &base.join("state"),
        plan: &plan,
        instructions: &instructions,
        launch: &launch,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
    })
    .expect("the mission runs");
    let order: Vec<TaskId> = outcome.ran.iter().map(|(id, _)| id.clone()).collect();
    let last_branch: String = outcome
        .ran
        .last()
        .map(|(_, attempt)| attempt.branch.clone())
        .unwrap_or_default();
    let delivered: String = git(&repository, &["ls-tree", "-r", "--name-only", &last_branch]);
    let _ = std::fs::remove_dir_all(&base);

    assert_eq!(
        (order, outcome.stopped_at),
        (vec![task("task_first"), task("task_second")], None),
        "both tasks ran, the dependency first"
    );
    assert!(
        delivered.contains("src/step-1.md") && delivered.contains("src/step-2.md"),
        "the second task started from the first task's work and added its own: {delivered:?}"
    );
}
