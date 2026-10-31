//! Shared vocabulary of the `kaisa mcp serve` integration tests: throwaway repositories and an
//! MCP client that talks to a `kaisa` binary's server over standard input and output.

#![allow(
    dead_code,
    reason = "each test crate uses a different part of this vocabulary"
)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long a scripted goal may take to finish.
pub(crate) const PATIENCE: Duration = Duration::from_secs(90);
/// The protocol version the test client announces and the server must answer with.
pub(crate) const PROTOCOL_VERSION: &str = "2025-06-18";

pub(crate) fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .expect("git is installed on every CI runner");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A temp directory with throwaway repositories and a state directory, removed on drop.
pub(crate) struct Workbench {
    pub(crate) base: PathBuf,
}

impl Workbench {
    pub(crate) fn new(name: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("kaisa-mcp-serve-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("writable temp dir");
        Self { base }
    }

    /// A repository with one commit on `main`.
    pub(crate) fn repository(&self, name: &str) -> PathBuf {
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

    pub(crate) fn state(&self) -> PathBuf {
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
pub(crate) fn scripted_run(fake_agent: &Path, with_check: bool) -> String {
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

/// The scripted fake agent built next to a `kaisa` binary of the test build.
pub(crate) fn fake_agent_beside(binary: &Path) -> PathBuf {
    binary.with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX))
}

/// An MCP client talking to a `kaisa mcp serve` process.
pub(crate) struct Client {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    pub(crate) fn start(
        binary: &Path,
        bench: &Workbench,
        directory: &Path,
        environment: &[(&str, &std::ffi::OsStr)],
    ) -> Self {
        let mut command = Command::new(binary);
        for (name, value) in environment {
            command.env(name, value);
        }
        let mut child = command
            .args(["mcp", "serve"])
            .current_dir(directory)
            .env("KAISA_STATE_DIR", bench.state())
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
            json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        );
        assert_eq!(hello["result"]["protocolVersion"], PROTOCOL_VERSION);
        writeln!(
            client.input,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .expect("written");
        client
    }

    pub(crate) fn request(&mut self, method: &str, params: Value) -> Value {
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
    pub(crate) fn call(&mut self, tool: &str, arguments: Value) -> (bool, String) {
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

    pub(crate) fn call_json(&mut self, tool: &str, arguments: Value) -> Value {
        let (refused, text) = self.call(tool, arguments);
        assert!(!refused, "{tool} was refused: {text}");
        serde_json::from_str(&text).expect("the tool answers in JSON")
    }

    /// Asks for the goal's status until it is no longer running.
    pub(crate) fn until_finished(&mut self, handle: &str) -> Value {
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
