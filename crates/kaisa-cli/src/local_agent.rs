//! `kaisa local-agent`: Kaisa itself acts as the agent for a local OpenAI-compatible model
//! (T28b, tier 0). The model only proposes; every tool call goes through the attempt's MCP
//! bridge and broker, exactly as a CLI agent's would, so write sets and confinement hold.

use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

/// A running MCP bridge, spoken to one request at a time.
pub(crate) struct BridgeClient {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl BridgeClient {
    /// Starts the bridge the MCP config at `config` names, and initializes it.
    pub(crate) fn start(config: &Path) -> io::Result<Self> {
        let text = std::fs::read_to_string(config)?;
        let config: Value = serde_json::from_str(&text)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let server = config
            .pointer("/mcpServers/kaisa")
            .ok_or_else(|| io::Error::other("the MCP config names no kaisa server"))?;
        let command = server["command"]
            .as_str()
            .ok_or_else(|| io::Error::other("the kaisa server has no command"))?;
        let args: Vec<&str> = server["args"]
            .as_array()
            .map(|args| args.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut child = Command::new(command)
            .args(&args)
            .env_clear()
            .envs(kaisa_sys::base_environment())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(io::Error::other("the bridge started without its pipes"));
        };
        let mut client = Self {
            child,
            input,
            output: BufReader::new(output),
            next_id: 1,
        };
        client.request("initialize", json!({"protocolVersion": "2025-06-18"}))?;
        Ok(client)
    }

    fn request(&mut self, method: &str, params: Value) -> io::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )?;
        self.input.flush()?;
        let mut line = String::new();
        if self.output.read_line(&mut line)? == 0 {
            return Err(io::Error::other("the bridge closed"));
        }
        serde_json::from_str(&line)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    /// The tools the bridge offers this attempt.
    pub(crate) fn tools(&mut self) -> io::Result<Vec<kaisa_agents::ToolSpec>> {
        let response = self.request("tools/list", json!({}))?;
        Ok(response
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| {
                        Some(kaisa_agents::ToolSpec {
                            name: tool["name"].as_str()?.to_owned(),
                            description: tool["description"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned(),
                            arguments_schema: tool["inputSchema"].clone(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Calls `tool` with `arguments`; the tool's text, or its error as an `Err`.
    pub(crate) fn call(&mut self, tool: &str, arguments: &Value) -> Result<String, String> {
        let response = self
            .request("tools/call", json!({"name": tool, "arguments": arguments}))
            .map_err(|error| error.to_string())?;
        let text: String = response
            .pointer("/result/content")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        match response.pointer("/result/isError").and_then(Value::as_bool) {
            Some(false) => Ok(text),
            _ => Err(if text.is_empty() {
                response["error"].to_string()
            } else {
                text
            }),
        }
    }
}

impl Drop for BridgeClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Turns a local model may take before Kaisa stops it.
const MAX_TURNS: usize = 24;
/// How long one model reply may take.
const CHAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|argument| argument == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

/// Runs `kaisa local-agent --mcp-config <file> --endpoint <url> --model <id>`, reading the
/// context pack as the first message on standard input, like any brokered agent.
pub(crate) fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    let (Some(config), Some(endpoint), Some(model)) = (
        flag(args, "--mcp-config"),
        flag(args, "--endpoint"),
        flag(args, "--model"),
    ) else {
        writeln!(
            err,
            "usage: kaisa local-agent --mcp-config <file> --endpoint <url> --model <id>"
        )?;
        return Ok(crate::EXIT_USAGE);
    };
    let endpoint = match kaisa_agents::LocalEndpoint::parse(&endpoint) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            writeln!(err, "kaisa local-agent: {error}")?;
            return Ok(crate::EXIT_USAGE);
        }
    };
    let mut first = String::new();
    io::stdin().lock().read_line(&mut first)?;
    let pack: String = serde_json::from_str::<Value>(&first)
        .ok()
        .and_then(|message| {
            message
                .pointer("/message/content")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let mut bridge = BridgeClient::start(Path::new(&config))?;
    let tools = bridge.tools()?;
    let mut chat = |messages: &[kaisa_agents::ChatMessage]| {
        kaisa_agents::chat(&endpoint, &model, messages, CHAT_TIMEOUT)
            .map_err(|error| error.to_string())
    };
    let mut call = |tool: &str, arguments: &Value| bridge.call(tool, arguments);
    let outcome = kaisa_agents::run_loop(&pack, &tools, MAX_TURNS, &mut chat, &mut call);
    writeln!(
        out,
        "local agent: {} after {} turns and {} tool calls; {}",
        if outcome.finished {
            "finished"
        } else {
            "stopped"
        },
        outcome.turns,
        outcome.tool_calls,
        if outcome.summary.is_some() {
            "its final answer follows"
        } else {
            "no final answer"
        }
    )?;
    // The model's final answer is the last line, where a reviewer's verdict is read from.
    if let Some(summary) = &outcome.summary {
        writeln!(out, "{summary}")?;
    }
    Ok(if outcome.finished {
        crate::EXIT_SUCCESS
    } else {
        crate::EXIT_FAILURE
    })
}
