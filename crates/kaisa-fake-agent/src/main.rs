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

fn flag_present(args: &[String], name: &str) -> bool {
    args.iter().any(|argument| argument == name)
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path: String = flag(&args, "--mcp-config").ok_or("missing --mcp-config")?;
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
    let pack: String = first
        .pointer("/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(PACK_ECHO_CHARS)
        .collect();

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
