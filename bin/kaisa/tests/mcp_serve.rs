//! `kaisa mcp serve` from a Claude Code session's point of view (user-flows F05): an MCP client
//! on the server's standard input and output, a real detached `kaisa` run with the scripted
//! fake agent, and the repository as the only thing that must stay untouched.

use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::json;
use support::{Client, Workbench, fake_agent_beside, git, scripted_run};

mod support;

/// The environment variable of the test hook; it exists only in the `test-hooks` test build.
const TEST_HOOK_ENV: &str = "KAISA_MCP_TEST_GOAL_ARGS";
/// The three tools of the catalog: start, status, result.
const CATALOG_TOOLS: [&str; 3] = ["kaisa_start_goal", "kaisa_status", "kaisa_result"];
/// Architecture §15: the whole catalog stays inside the 2k-token budget; the byte bound is the
/// same number, which is far stricter than four bytes per token.
const CATALOG_BYTE_BUDGET: usize = 2_000;
/// How a private directory and file are readable: the owner only.
#[cfg(unix)]
const OWNER_ONLY_DIRECTORY: u32 = 0o700;
#[cfg(unix)]
const OWNER_ONLY_FILE: u32 = 0o600;
#[cfg(unix)]
const PERMISSION_BITS: u32 = 0o777;
/// A state directory as an ordinary umask leaves it.
#[cfg(unix)]
const WORLD_READABLE_DIRECTORY: u32 = 0o755;

/// The `kaisa` binary of this test build, which has the `test-hooks` feature.
fn test_build() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
}

/// A server of the test build whose hook scripts the fake agent (and the check, if asked).
fn scripted_server(bench: &Workbench, directory: &Path, with_check: bool) -> Client {
    let hook = scripted_run(&fake_agent_beside(&test_build()), with_check);
    Client::start(
        &test_build(),
        bench,
        directory,
        &[(TEST_HOOK_ENV, OsStr::new(&hook))],
    )
}

/// A server of the test build with no hook set.
fn plain_server(bench: &Workbench, directory: &Path) -> Client {
    Client::start(&test_build(), bench, directory, &[])
}

#[test]
fn the_server_greets_and_lists_exactly_three_small_tools() {
    let bench = Workbench::new("tools");
    let repository = bench.repository("repo");
    let mut client = plain_server(&bench, &repository);

    let listed = client.request("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().expect("a tool list");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();

    assert_eq!(
        names, CATALOG_TOOLS,
        "the catalog is exactly the start, status and result tools"
    );
    assert!(
        listed.to_string().len() < CATALOG_BYTE_BUDGET,
        "the catalog stays far inside the 2k-token budget of architecture §15"
    );
    assert!(
        !names.iter().any(|name| name.contains("merge")
            || name.contains("approve")
            || name.contains("publish")),
        "nothing in the catalog can merge, approve or publish"
    );
    for tool in tools {
        let properties = tool["inputSchema"]["properties"]
            .as_object()
            .expect("every tool declares its properties");
        assert!(
            properties
                .keys()
                .all(|key| ["goal", "agent", "repository", "handle"].contains(&key.as_str())),
            "no tool accepts arguments, flags or environment: {properties:?}"
        );
    }
}

#[test]
fn a_directory_that_is_not_a_git_repository_is_refused_with_the_reason() {
    let bench = Workbench::new("not-a-repo");
    let repository = bench.repository("repo");
    let plain = bench.base.join("plain");
    std::fs::create_dir_all(&plain).expect("created");
    let mut client = plain_server(&bench, &repository);

    let (refused, reason) = client.call(
        "kaisa_start_goal",
        json!({"goal": "fix it", "repository": plain.to_string_lossy()}),
    );
    let (bad_agent, _) = client.call(
        "kaisa_start_goal",
        json!({"goal": "fix it", "agent": "gpt"}),
    );

    assert!(
        refused && reason.contains("not inside a git repository"),
        "a plain directory is refused and the reason says why, got {reason:?}"
    );
    assert!(bad_agent, "an agent Kaisa does not know is refused");
    assert!(
        !bench.state().join("goals").exists(),
        "a refused goal leaves no run behind"
    );
}

#[test]
fn a_goal_runs_in_the_background_to_verified_and_the_result_only_names_the_merge() {
    let bench = Workbench::new("verified");
    let repository = bench.repository("repo");
    let head_before = git(&repository, &["rev-parse", "HEAD"]);
    let mut client = scripted_server(&bench, &repository, true);

    let started = client.call_json(
        "kaisa_start_goal",
        json!({"goal": "Add notes about the login flow."}),
    );
    let handle = started["handle"].as_str().expect("a handle").to_owned();
    let finished = client.until_finished(&handle);
    let result = client.call_json("kaisa_result", json!({"handle": handle}));

    assert_eq!(
        started["state"], "running",
        "start returns at once, with a handle"
    );
    assert_eq!(
        finished["state"], "verified",
        "the scripted run passes its check and review, got {finished}"
    );
    let branch = finished["branch"].as_str().expect("the result branch");
    assert!(
        branch.starts_with("kaisa/att_"),
        "the result lives on a Kaisa attempt branch, got {branch}"
    );
    assert_eq!(
        finished["merge_command"],
        format!("git merge --ff-only {branch}"),
        "the result names the fast-forward merge for the person to run"
    );
    assert!(
        finished["tokens"].is_object(),
        "tokens come from the ledger: {finished}"
    );
    assert_eq!(
        result["merge_command"], finished["merge_command"],
        "status and result name the same merge"
    );
    assert!(
        result["final_lines"].to_string().contains("verified"),
        "the final lines carry the verdict: {result}"
    );
    assert_eq!(
        git(&repository, &["rev-parse", "HEAD"]),
        head_before,
        "reading the result does not move HEAD"
    );
    assert_eq!(
        git(&repository, &["rev-parse", "main"]),
        head_before,
        "main is untouched"
    );
    assert!(
        !repository.join("src/step-1.md").exists(),
        "the result stays on its own branch until the person merges it"
    );
}

#[test]
fn a_goal_survives_the_session_that_started_it() {
    let bench = Workbench::new("survives");
    let repository = bench.repository("repo");
    let handle = {
        let mut session = scripted_server(&bench, &repository, true);
        let started = session.call_json("kaisa_start_goal", json!({"goal": "Add notes."}));
        started["handle"].as_str().expect("a handle").to_owned()
        // The session ends here: its server is killed, as when Claude Code exits.
    };

    let mut later = scripted_server(&bench, &repository, true);
    let finished = later.until_finished(&handle);
    let recent = later.call_json("kaisa_status", json!({}));

    assert_eq!(
        finished["state"], "verified",
        "the detached run finished on its own: {finished}"
    );
    assert_eq!(
        recent["goals"][0]["handle"], handle,
        "recent goals list it: {recent}"
    );
}

#[test]
fn a_goal_without_passing_evidence_is_reported_not_delivered_with_the_reason() {
    let bench = Workbench::new("not-delivered");
    let repository = bench.repository("repo");
    let mut client = scripted_server(&bench, &repository, false);

    let started = client.call_json("kaisa_start_goal", json!({"goal": "Add notes."}));
    let finished = client.until_finished(started["handle"].as_str().expect("a handle"));
    let result = client.call_json("kaisa_result", json!({"handle": started["handle"]}));

    assert_eq!(
        finished["state"], "not_delivered",
        "without the user's check nothing is verified, got {finished}"
    );
    assert!(
        finished["reason"]
            .as_str()
            .is_some_and(|reason| reason.starts_with("not delivered")),
        "the reason says the goal was not delivered: {finished}"
    );
    assert!(
        result.get("merge_command").is_none(),
        "nothing was delivered, so there is nothing to merge"
    );
}

#[test]
fn several_goals_run_at_once_with_their_own_handles() {
    let bench = Workbench::new("concurrent");
    let first = bench.repository("first");
    let second = bench.repository("second");
    let mut client = scripted_server(&bench, &first, true);

    let a = client.call_json("kaisa_start_goal", json!({"goal": "Add notes."}));
    let b = client.call_json(
        "kaisa_start_goal",
        json!({"goal": "Add notes.", "repository": second.to_string_lossy()}),
    );
    let (a_handle, b_handle) = (
        a["handle"].as_str().expect("handle"),
        b["handle"].as_str().expect("handle"),
    );
    let a_done = client.until_finished(a_handle);
    let b_done = client.until_finished(b_handle);

    assert_ne!(a_handle, b_handle, "every goal has its own handle");
    assert_eq!(
        a_done["state"], "verified",
        "the first goal verifies, got {a_done}"
    );
    assert_eq!(
        b_done["state"], "verified",
        "the second goal verifies, got {b_done}"
    );
    assert_ne!(
        a_done["repository"], b_done["repository"],
        "each goal ran in its own repository"
    );
}

#[test]
fn a_handle_cannot_name_a_path_or_a_goal_that_does_not_exist() {
    let bench = Workbench::new("handles");
    let repository = bench.repository("repo");
    let mut client = plain_server(&bench, &repository);

    for handle in ["../state", "goal-1-1", "x"] {
        let (status_refused, _) = client.call("kaisa_status", json!({"handle": handle}));
        let (result_refused, _) = client.call("kaisa_result", json!({"handle": handle}));
        assert!(
            status_refused && result_refused,
            "{handle:?} names no goal, so both tools refuse it"
        );
    }
    assert_eq!(
        client.call_json("kaisa_status", json!({})),
        json!({"goals": []}),
        "no goal was started, so none is listed"
    );
}

#[test]
fn a_goal_that_looks_like_options_is_goal_text_and_never_an_option() {
    let bench = Workbench::new("option-goal");
    let repository = bench.repository("repo");
    let goal_text = "--agent-program /DO-NOT-RUN --check-program /DO-NOT-RUN";
    let mut client = scripted_server(&bench, &repository, true);

    let started = client.call_json("kaisa_start_goal", json!({"goal": goal_text}));
    let handle = started["handle"].as_str().expect("a handle").to_owned();
    let finished = client.until_finished(&handle);
    let meta: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bench.state().join("goals").join(&handle).join("goal.json"))
            .expect("the goal is recorded"),
    )
    .expect("goal.json is JSON");

    assert_eq!(
        meta["goal"], goal_text,
        "the goal is recorded word for word as text"
    );
    assert_eq!(
        finished["state"], "verified",
        "the scripted program ran, not /DO-NOT-RUN: got {finished}"
    );
}

#[cfg(unix)]
#[test]
fn goal_data_is_private_even_when_the_run_fails_before_any_agent_starts() {
    let bench = Workbench::new("private-early-failure");
    let repository = bench.repository("repo");
    let state = bench.state();
    std::fs::create_dir_all(&state).expect("created");
    std::fs::set_permissions(
        &state,
        std::fs::Permissions::from_mode(WORLD_READABLE_DIRECTORY),
    )
    .expect("the state directory starts as an ordinary umask leaves it");
    // A policy with an empty hook array is malformed: the goal fails before any agent starts.
    std::fs::write(state.join("policy.json"), r#"{"hooks": []}"#).expect("written");
    let mut client = plain_server(&bench, &repository);

    let started = client.call_json("kaisa_start_goal", json!({"goal": "Add notes."}));
    let handle = started["handle"].as_str().expect("a handle").to_owned();
    let finished = client.until_finished(&handle);
    let goal = state.join("goals").join(&handle);

    assert_eq!(
        finished["state"], "not_delivered",
        "a malformed policy stops the goal before any agent, got {finished}"
    );
    let mode_of = |path: &Path| {
        std::fs::metadata(path)
            .unwrap_or_else(|error| panic!("{} should exist: {error}", path.display()))
            .permissions()
            .mode()
            & PERMISSION_BITS
    };
    for directory in [&state, &state.join("goals"), &goal] {
        assert_eq!(
            mode_of(directory),
            OWNER_ONLY_DIRECTORY,
            "{} holds the goal's data, so only its owner may enter it",
            directory.display()
        );
    }
    for file in ["goal.json", "output.log", "process", "exit"] {
        assert_eq!(
            mode_of(&goal.join(file)),
            OWNER_ONLY_FILE,
            "{file} holds the goal text or its output, so only its owner may read it"
        );
    }
}
