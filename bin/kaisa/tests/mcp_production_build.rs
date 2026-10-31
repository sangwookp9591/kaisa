//! A normally built `kaisa` (what `cargo install --path bin/kaisa` and release builds produce)
//! has no test hook: the environment variable of the integration-test build means nothing to it.
//!
//! The test build of the binary has the `test-hooks` feature (through the dev-dependency), so
//! this file builds the binary again the way an installer does, without dev-dependencies and
//! into its own target directory, and drives that one.

#![cfg(unix)]

mod support;

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

use serde_json::{Value, json};
use support::{Client, PATIENCE, Workbench, fake_agent_beside, scripted_run};

/// The variable of the test hook; the production build must not read it.
const TEST_HOOK_ENV: &str = "KAISA_MCP_TEST_GOAL_ARGS";
/// What the stand-in `claude` and `codex` programs record when anything starts them.
const STAND_IN_MARKER: &str = "stand-in-was-started";
/// Mode of a script the stand-in programs are made executable with.
const EXECUTABLE: u32 = 0o755;

/// The binary a plain `cargo build -p kaisa` produces: no dev-dependency features.
fn production_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("production-build");
        let status = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args([
                "build",
                "--locked",
                "--offline",
                "--quiet",
                "-p",
                "kaisa",
                "--bin",
                "kaisa",
            ])
            .arg("--target-dir")
            .arg(&target)
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .status()
            .expect("cargo can build the production binary");
        assert!(status.success(), "the production binary builds");
        target
            .join("debug")
            .join(format!("kaisa{}", std::env::consts::EXE_SUFFIX))
    })
}

/// A directory with `claude` and `codex` programs that only record that they were started, put
/// in front of `PATH`: if the hook's fake agent is not used, the real agent names are, and the
/// real agents never run in a test.
fn stand_in_agents(bench: &Workbench) -> (PathBuf, std::ffi::OsString) {
    let directory = bench.base.join("stand-in-bin");
    std::fs::create_dir_all(&directory).expect("created");
    let marker = bench.base.join(STAND_IN_MARKER);
    for name in ["claude", "codex"] {
        let program = directory.join(name);
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\necho started >> '{}'\nexit 1\n",
                marker.display()
            ),
        )
        .expect("written");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(EXECUTABLE))
            .expect("made executable");
    }
    let mut path = directory.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    (marker, path)
}

/// The hook value a test-build server would act on: the fake agent as developer and reviewer,
/// and a check that passes.
fn the_hook_a_test_build_would_honor() -> String {
    scripted_run(
        &fake_agent_beside(Path::new(env!("CARGO_BIN_EXE_kaisa"))),
        true,
    )
}

fn wait_for_file(path: &Path) {
    let started = Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < PATIENCE,
            "{} did not appear in time",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[test]
fn a_production_server_ignores_the_hook_and_cannot_swap_agent_reviewer_or_checks() {
    let bench = Workbench::new("production-server");
    let repository = bench.repository("repo");
    let (marker, path) = stand_in_agents(&bench);
    let hook = the_hook_a_test_build_would_honor();
    let mut client = Client::start(
        production_binary(),
        &bench,
        &repository,
        &[
            (TEST_HOOK_ENV, OsStr::new(&hook)),
            ("PATH", path.as_os_str()),
        ],
    );

    let started = client.call_json("kaisa_start_goal", json!({"goal": "Add notes."}));
    let handle = started["handle"].as_str().expect("a handle").to_owned();
    let finished = client.until_finished(&handle);
    let meta: Value = serde_json::from_slice(
        &std::fs::read(bench.state().join("goals").join(&handle).join("goal.json"))
            .expect("the goal is recorded"),
    )
    .expect("goal.json is JSON");

    assert!(
        meta.get("extra_args").is_none(),
        "a production server records no hook arguments: {meta}"
    );
    assert_ne!(
        finished["state"], "verified",
        "the hook's fake agent, reviewer and check must not be able to verify a goal: {finished}"
    );
    assert!(
        marker.exists(),
        "the real agent program name was launched instead of the hook's fake agent"
    );
}

#[test]
fn a_production_runner_ignores_hook_arguments_planted_in_goal_metadata() {
    let bench = Workbench::new("production-runner");
    let repository = bench.repository("repo");
    let (marker, path) = stand_in_agents(&bench);
    let hook: Vec<String> = serde_json::from_str(&the_hook_a_test_build_would_honor())
        .expect("the hook is a JSON array");
    let goal = bench.state().join("goals").join("goal-1-0");
    std::fs::create_dir_all(&goal).expect("created");
    std::fs::write(
        goal.join("goal.json"),
        json!({"handle": "goal-1-0", "goal": "Add notes.", "agent": "claude",
            "repository": repository, "started_ms": 0, "extra_args": hook})
        .to_string(),
    )
    .expect("written");

    let status = Command::new(production_binary())
        .args(["mcp", "--run-goal"])
        .arg(&goal)
        .current_dir(&repository)
        .env("KAISA_STATE_DIR", bench.state())
        .env("PATH", &path)
        .status()
        .expect("the runner starts");
    let exit: Value = serde_json::from_slice(
        &std::fs::read(goal.join("exit")).expect("the runner records how it ended"),
    )
    .expect("exit is JSON");
    wait_for_file(&goal.join("exit"));

    assert!(
        !status.success() && exit["code"] != 0,
        "planted fake-agent arguments cannot make the goal succeed: {exit}"
    );
    assert!(
        marker.exists(),
        "the real agent program name was launched, not the planted fake agent"
    );
}
