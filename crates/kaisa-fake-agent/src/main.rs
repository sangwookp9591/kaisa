//! A scripted stand-in for a brokered agent CLI, for end-to-end tests without credentials.
//!
//! It does what Claude Code does in brokered mode and nothing else: reads its MCP config, starts
//! the bridge it names, reads the first message (the context pack) from standard input, and
//! writes one file through the bridge's `fs_write`. Its only direct file access is the opt-in
//! `--try-direct-write` probe, which exists to prove the agent's own process is confined: it
//! tries to write a file itself, and the agent fails if that succeeds.
//!
//! Usage: `kaisa-fake-agent --mcp-config <file> --fake-write <repository path>
//! [--try-direct-write <path relative to its working directory>] [other flags]`

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

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path: String = flag(&args, "--mcp-config").ok_or("missing --mcp-config")?;
    let target: String = flag(&args, "--fake-write").ok_or("missing --fake-write")?;
    if let Some(probe) = flag(&args, "--try-direct-write")
        && std::fs::write(&probe, "written around the broker\n").is_ok()
    {
        return Err(format!(
            "wrote {probe} directly: the agent process is not confined"
        ));
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
