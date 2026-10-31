//! A scripted stand-in for a brokered agent CLI, for end-to-end tests without credentials.
//!
//! It does what Claude Code does in brokered mode and nothing else: reads its MCP config, starts
//! the bridge it names, reads the first message (the context pack) from standard input, and
//! writes one file through the bridge's `fs_write`. Its only direct file access is the opt-in
//! `--try-direct-write` probe, which exists to prove the agent's own process is confined: it
//! tries to write a file itself, and the agent fails if that succeeds.
//!
//! Usage: `kaisa-fake-agent --mcp-config <file> --fake-write <repository path>
//! [--try-direct-write <path relative to its working directory>]
//! (or `--fake-write-next <path prefix>`)
//! (or `--fake-review-require <path>`: act as a reviewer that approves only if the path exists,
//! answering as `--fake-review-format raw|claude|claude-prose` says)
//! [--fake-content <text>: what `--fake-write` writes, instead of the start of the pack]
//! [--try-direct-connect <ip:port>] [--expect-proxy-refuses <host>] [--crash-after-write]
//! [other flags]`
//!
//! `--crash-after-write` aborts the agent right after its brokered write, leaving its bridge
//! running, as a crashing CLI would; `--hang-after-write` keeps running instead, so a test can
//! kill Kaisa under a live agent (chaos suite, T16.6).
//!
//! The network probes work the same way: `--try-direct-connect` fails the agent if a direct
//! connection succeeds, and `--expect-proxy-refuses` fails it unless the proxy in
//! `HTTPS_PROXY` answers a `CONNECT` to that host with 403.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, ExitCode, Stdio};

use serde_json::{Value, json};

/// How much of the context pack the agent writes back, so tests can see it was delivered.
const PACK_ECHO_CHARS: usize = 200;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|argument| argument == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

/// The status line the proxy in `HTTPS_PROXY` answers to `CONNECT host:443`.
fn proxy_status_for(host: &str) -> Result<String, String> {
    let proxy: String = std::env::var("HTTPS_PROXY").map_err(|_| "no HTTPS_PROXY".to_owned())?;
    let address: &str = proxy.trim_start_matches("http://");
    let mut stream = std::net::TcpStream::connect(address)
        .map_err(|error| format!("cannot reach the egress broker at {address}: {error}"))?;
    write!(
        stream,
        "CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n"
    )
    .map_err(|error| error.to_string())?;
    let mut status = String::new();
    BufReader::new(stream)
        .read_line(&mut status)
        .map_err(|error| error.to_string())?;
    Ok(status)
}

/// How long a hanging agent waits: far longer than any test runs.
const HANG: std::time::Duration = std::time::Duration::from_secs(600);

/// The file the fake agent writes when it is started the way Codex is.
const CODEX_WRITES: &str = "src/written-by-codex.md";

/// The line of a review pack that names the candidate's tree.
const CANDIDATE_TREE_LABEL: &str = "candidate_tree: ";

/// How the scripted reviewer answers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReviewFormat {
    /// The verdict alone, in a final dedicated fence on standard output.
    Raw,
    /// Claude Code's stream-json events, the verdict in a code fence inside prose in the
    /// assistant message and the final `result` event, which is the transcript's last line.
    Claude,
    /// The same events, the answer only prose with no verdict.
    ClaudeProse,
    ClaudeTruncated,
    ClaudeFailed,
}

impl ReviewFormat {
    fn from_flag(value: Option<&str>) -> Result<Self, String> {
        match value {
            None | Some("raw") => Ok(Self::Raw),
            Some("claude") => Ok(Self::Claude),
            Some("claude-prose") => Ok(Self::ClaudeProse),
            Some("claude-truncated") => Ok(Self::ClaudeTruncated),
            Some("claude-failed") => Ok(Self::ClaudeFailed),
            Some(other) => Err(format!("unknown review format {other:?}")),
        }
    }
}

/// Prints `answer` the way Claude Code's `-p --output-format stream-json --verbose` does: an
/// `init` event, the assistant message, and a successful `result` event carrying its text.
fn print_as_claude(answer: &str) {
    println!(
        "{}",
        json!({"type": "system", "subtype": "init", "cwd": ".", "tools": [], "mcp_servers": [],
               "model": "claude-haiku-5-5", "session_id": "fake"})
    );
    println!(
        "{}",
        json!({"type": "assistant", "session_id": "fake", "message": {"role": "assistant",
               "type": "message", "content": [{"type": "text", "text": answer}]}})
    );
    println!(
        "{}",
        json!({"duration_ms": 1, "is_error": false, "num_turns": 1, "subtype": "success",
               "result": answer, "session_id": "fake", "type": "result",
               "usage": {"input_tokens": 1, "output_tokens": 1}, "total_cost_usd": 0.0})
    );
}

/// Reviews the candidate in the working directory: approves if `required` exists there, else
/// reports one blocking finding, answering in `format`.
fn review(required: &str, format: ReviewFormat) -> Result<(), String> {
    let mut first_line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut first_line)
        .map_err(|error| error.to_string())?;
    let first: Value = serde_json::from_str(&first_line).map_err(|error| error.to_string())?;
    let pack: &str = first
        .pointer("/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tree: &str = pack
        .lines()
        .find_map(|line| {
            line.trim_start_matches("> ")
                .strip_prefix(CANDIDATE_TREE_LABEL)
        })
        .ok_or("the review pack names no candidate tree")?
        .trim();
    let approve: bool = std::path::Path::new(required).exists();
    let findings: Value = if approve {
        json!([])
    } else {
        json!([{"severity": "blocking", "path": required, "line": null,
                "message": format!("{required} is missing")}])
    };
    let verdict: Value = json!({"candidate_tree": tree, "approve": approve, "findings": findings});
    match format {
        ReviewFormat::Raw => println!("```kaisa-verdict\n{verdict}\n```"),
        ReviewFormat::Claude | ReviewFormat::ClaudeTruncated | ReviewFormat::ClaudeFailed => {
            print_as_claude(&format!(
                "I reviewed the candidate against the goal.\n\n```kaisa-verdict\n{verdict}\n```"
            ))
        }
        ReviewFormat::ClaudeProse => print_as_claude("I reviewed the candidate. Looks good to me!"),
    }
    if format == ReviewFormat::ClaudeTruncated {
        const CAPTURE_LIMIT: usize = 4 * 1024 * 1024;
        println!("{}", "x".repeat(CAPTURE_LIMIT));
        println!(
            "{}",
            json!({"type":"result", "is_error":true,
            "subtype":"error_during_execution", "result":"last result failed"})
        );
    }
    if format == ReviewFormat::ClaudeFailed {
        return Err("reviewer failed after printing success".to_owned());
    }
    Ok(())
}

fn flag_present(args: &[String], name: &str) -> bool {
    args.iter().any(|argument| argument == name)
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Started with Codex's own arguments (`exec --json ...`), it acts as Codex would: it reads
    // the plain pack and writes `CODEX_WRITES` into its working directory.
    if args.first().map(String::as_str) == Some("exec") {
        let mut pack = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut pack)
            .map_err(|error| error.to_string())?;
        std::fs::create_dir_all("src").map_err(|error| error.to_string())?;
        return std::fs::write(CODEX_WRITES, "written by Codex\n")
            .map_err(|error| error.to_string());
    }
    // `--fake-observed-write <path>` acts like an observed agent (Codex): it reads the whole
    // pack as plain text and writes straight into its working directory, with no bridge.
    if let Some(path) = flag(&args, "--fake-observed-write") {
        let mut pack = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut pack)
            .map_err(|error| error.to_string())?;
        if pack.trim_start().starts_with('{') {
            return Err(
                "an observed agent was given a JSON message, not the plain pack".to_owned(),
            );
        }
        if let Some(parent) = std::path::Path::new(&path).parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        return std::fs::write(&path, "written by an observed agent\n")
            .map_err(|error| error.to_string());
    }
    let config_path: String = flag(&args, "--mcp-config").ok_or("missing --mcp-config")?;
    if let Some(required) = flag(&args, "--fake-review-require") {
        let format = ReviewFormat::from_flag(flag(&args, "--fake-review-format").as_deref())?;
        return review(&required, format);
    }
    // `--fake-usage <input>/<output>/<cost>` reports usage the way Claude Code's stream-json
    // result event does, so usage recording can be tested without a model.
    if let Some(usage) = flag(&args, "--fake-usage") {
        let parts: Vec<&str> = usage.split('/').collect();
        if let [input, output, cost] = parts.as_slice() {
            println!(
                "{}",
                json!({"type": "result", "usage": {
                    "input_tokens": input.parse::<u64>().unwrap_or_default(),
                    "output_tokens": output.parse::<u64>().unwrap_or_default()},
                    "total_cost_usd": cost.parse::<f64>().unwrap_or_default()})
            );
        }
    }
    // `--fake-write-next src/step` writes the first of src/step-1.md, src/step-2.md, … that its
    // worktree (its working directory) does not have yet, so a mission's later task shows it
    // started from the earlier task's work.
    let target: String = match flag(&args, "--fake-write-next") {
        Some(prefix) => (1..)
            .map(|step| format!("{prefix}-{step}.md"))
            .find(|candidate| !std::path::Path::new(candidate).exists())
            .ok_or("no free step file")?,
        None => flag(&args, "--fake-write").ok_or("missing --fake-write")?,
    };
    if let Some(probe) = flag(&args, "--try-direct-write")
        && std::fs::write(&probe, "written around the broker\n").is_ok()
    {
        return Err(format!(
            "wrote {probe} directly: the agent process is not confined"
        ));
    }
    if let Some(address) = flag(&args, "--try-direct-connect")
        && std::net::TcpStream::connect(&address).is_ok()
    {
        return Err(format!(
            "connected to {address} directly, around the egress broker"
        ));
    }
    if let Some(host) = flag(&args, "--expect-proxy-refuses") {
        let status: String = proxy_status_for(&host)?;
        if !status.contains(" 403 ") {
            return Err(format!("the egress broker did not refuse {host}: {status}"));
        }
    }
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(&config_path).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let server: &Value = config
        .pointer("/mcpServers/kaisa")
        .ok_or("the MCP config names no `kaisa` server")?;
    let command: &str = server["command"].as_str().ok_or("no bridge command")?;
    let bridge_args: Vec<&str> = server["args"]
        .as_array()
        .map(|parts| parts.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let mut first_line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut first_line)
        .map_err(|error| error.to_string())?;
    let first: Value = serde_json::from_str(&first_line).map_err(|error| error.to_string())?;
    let pack: String = flag(&args, "--fake-content").unwrap_or_else(|| {
        first
            .pointer("/message/content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(PACK_ECHO_CHARS)
            .collect()
    });

    let mut bridge = Command::new(command)
        .args(&bridge_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start the bridge: {error}"))?;
    let mut to_bridge = bridge.stdin.take().ok_or("no bridge stdin")?;
    let mut from_bridge = BufReader::new(bridge.stdout.take().ok_or("no bridge stdout")?);
    for request in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "fs_write", "arguments": {"path": target, "content": pack}}}),
    ] {
        writeln!(to_bridge, "{request}").map_err(|error| error.to_string())?;
    }
    drop(to_bridge);
    let mut refused: bool = true;
    for _ in 0..2 {
        let mut line = String::new();
        from_bridge
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        let response: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
        if response["id"] == 2 {
            refused = response.pointer("/result/isError").and_then(Value::as_bool) != Some(false);
        }
    }
    if flag_present(&args, "--crash-after-write") {
        std::process::abort();
    }
    if flag_present(&args, "--hang-after-write") {
        std::thread::sleep(HANG);
    }
    let _ = bridge.wait();
    if refused {
        Err(format!("the bridge refused to write {target}"))
    } else {
        Ok(())
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("kaisa-fake-agent: {reason}");
            ExitCode::FAILURE
        }
    }
}
