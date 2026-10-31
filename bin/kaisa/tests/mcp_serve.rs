//! `kaisa mcp serve` from a Claude Code session's point of view (user-flows F05): an MCP client
//! on the server's standard input and output, a real detached `kaisa` run with the scripted
//! fake agent, and the repository as the only thing that must stay untouched.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long a scripted goal may take to finish.
const PATIENCE: Duration = Duration::from_secs(90);

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .expect("git is installed on every CI runner");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A temp directory with throwaway repositories and a state directory, removed on drop.
struct Workbench {
    base: PathBuf,
}

impl Workbench {
    fn new(name: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("kaisa-mcp-serve-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("writable temp dir");
        Self { base }
    }

    /// A repository with one commit on `main`.
    fn repository(&self, name: &str) -> PathBuf {
        let repository = self.base.join(name);
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
        repository
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

/// The extra `kaisa` arguments of a scripted verified run; `with_check` adds the user's check
/// that requires the developer's file, without it nothing is verified.
fn scripted_run(with_check: bool) -> String {
    let fake_agent = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let mut args: Vec<String> = [
        "--agent-program",
        &fake_agent.to_string_lossy(),
        "--agent-arg",
        "--fake-write-next",
        "--agent-arg",
        "src/step",
        "--reviewer-arg",
        "--fake-review-require",
        "--reviewer-arg",
        "src/step-1.md",
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect();
    if with_check {
        let (program, flag, check) = if cfg!(windows) {
            (
                "cmd",
                "/c",
                r"if exist src\step-1.md (exit 0) else (exit 1)",
            )
        } else {
            ("sh", "-c", "test -f src/step-1.md")
        };
        for arg in [
            "--check-program",
            program,
            "--check-arg",
            flag,
            "--check-arg",
            check,
        ] {
            args.push(arg.to_owned());
        }
    }
    serde_json::to_string(&args).expect("serializes")
}

/// An MCP client talking to a `kaisa mcp serve` process.
struct Client {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn start(bench: &Workbench, directory: &Path, goal_args: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kaisa"))
            .args(["mcp", "serve"])
            .current_dir(directory)
            .env("KAISA_STATE_DIR", bench.state())
            .env("KAISA_MCP_TEST_GOAL_ARGS", goal_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("kaisa runs");
        let input = child.stdin.take().expect("piped");
        let output = BufReader::new(child.stdout.take().expect("piped"));
        let mut client = Self {
            child,
            input,
            output,
            next_id: 1,
        };
        let hello = client.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        );
        assert_eq!(hello["result"]["protocolVersion"], "2025-06-18");
        writeln!(
            client.input,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .expect("written");
        client
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.input, "{request}").expect("the server is listening");
        self.input.flush().expect("flushed");
        let mut line = String::new();
        self.output.read_line(&mut line).expect("a response");
        let response: Value = serde_json::from_str(&line).expect("a JSON response");
        assert_eq!(response["id"], id, "the response answers this request");
        response
    }

    /// Calls a tool; returns whether it was refused and the text it gave back.
    fn call(&mut self, tool: &str, arguments: Value) -> (bool, String) {
        let response = self.request("tools/call", json!({"name": tool, "arguments": arguments}));
        let result = &response["result"];
        (
            result["isError"]
                .as_bool()
                .expect("tool results say whether they failed"),
            result["content"][0]["text"]
                .as_str()
                .expect("text")
                .to_owned(),
        )
    }

    fn call_json(&mut self, tool: &str, arguments: Value) -> Value {
        let (refused, text) = self.call(tool, arguments);
        assert!(!refused, "{tool} was refused: {text}");
        serde_json::from_str(&text).expect("the tool answers in JSON")
    }

    /// Asks for the goal's status until it is no longer running.
    fn until_finished(&mut self, handle: &str) -> Value {
        let started = Instant::now();
        loop {
            let status = self.call_json("kaisa_status", json!({"handle": handle}));
            if status["state"] != "running" {
                return status;
            }
            assert!(
                started.elapsed() < PATIENCE,
                "the goal did not finish: {status}"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn the_server_greets_and_lists_exactly_three_small_tools() {
    let bench = Workbench::new("tools");
    let repository = bench.repository("repo");
    let mut client = Client::start(&bench, &repository, "[]");

    let listed = client.request("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().expect("a tool list");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();

    assert_eq!(names, ["kaisa_start_goal", "kaisa_status", "kaisa_result"]);
    assert!(
        listed.to_string().len() < 2_000,
        "the catalog stays far inside the 2k-token budget of architecture §15"
    );
    assert!(
        !names.iter().any(|name| name.contains("merge")
            || name.contains("approve")
            || name.contains("publish")),
        "nothing in the catalog can merge, approve or publish"
    );
}

#[test]
fn a_directory_that_is_not_a_git_repository_is_refused_with_the_reason() {
    let bench = Workbench::new("not-a-repo");
    let repository = bench.repository("repo");
    let plain = bench.base.join("plain");
    std::fs::create_dir_all(&plain).expect("created");
    let mut client = Client::start(&bench, &repository, "[]");

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
        "got {reason:?}"
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
    let mut client = Client::start(&bench, &repository, &scripted_run(true));

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
    assert_eq!(finished["state"], "verified", "got {finished}");
    let branch = finished["branch"].as_str().expect("the result branch");
    assert!(branch.starts_with("kaisa/att_"), "got {branch}");
    assert_eq!(
        finished["merge_command"],
        format!("git merge --ff-only {branch}")
    );
    assert!(
        finished["tokens"].is_object(),
        "tokens come from the ledger: {finished}"
    );
    assert_eq!(result["merge_command"], finished["merge_command"]);
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
    let hook = scripted_run(true);
    let handle = {
        let mut session = Client::start(&bench, &repository, &hook);
        let started = session.call_json("kaisa_start_goal", json!({"goal": "Add notes."}));
        started["handle"].as_str().expect("a handle").to_owned()
        // The session ends here: its server is killed, as when Claude Code exits.
    };

    let mut later = Client::start(&bench, &repository, &hook);
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
    let mut client = Client::start(&bench, &repository, &scripted_run(false));

    let started = client.call_json("kaisa_start_goal", json!({"goal": "Add notes."}));
    let finished = client.until_finished(started["handle"].as_str().expect("a handle"));
    let result = client.call_json("kaisa_result", json!({"handle": started["handle"]}));

    assert_eq!(finished["state"], "not_delivered", "got {finished}");
    assert!(
        finished["reason"]
            .as_str()
            .is_some_and(|reason| reason.starts_with("not delivered"))
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
    let mut client = Client::start(&bench, &first, &scripted_run(true));

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

    assert_ne!(a_handle, b_handle);
    assert_eq!(a_done["state"], "verified", "got {a_done}");
    assert_eq!(b_done["state"], "verified", "got {b_done}");
    assert_ne!(a_done["repository"], b_done["repository"]);
}

#[test]
fn a_handle_cannot_name_a_path_or_a_goal_that_does_not_exist() {
    let bench = Workbench::new("handles");
    let repository = bench.repository("repo");
    let mut client = Client::start(&bench, &repository, "[]");

    for handle in ["../state", "goal-1-1", "x"] {
        let (status_refused, _) = client.call("kaisa_status", json!({"handle": handle}));
        let (result_refused, _) = client.call("kaisa_result", json!({"handle": handle}));
        assert!(status_refused && result_refused, "{handle:?} names no goal");
    }
    assert_eq!(
        client.call_json("kaisa_status", json!({})),
        json!({"goals": []})
    );
}
