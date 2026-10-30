//! The MCP stdio server loop.

use std::io::{BufRead, Write};

use kaisa_protocol::{DEFAULT_FRAME_LIMIT, FrameError, FrameReader, write_frame};
use serde_json::{Value, json};

use crate::tools::ToolDescription;

/// JSON-RPC: the method does not exist.
const METHOD_NOT_FOUND: i64 = -32_601;
/// JSON-RPC: the parameters are invalid (for example, a tool that is not listed).
const INVALID_PARAMS: i64 = -32_602;
/// The MCP protocol version this bridge speaks when the client does not name one.
const DEFAULT_MCP_VERSION: &str = "2025-06-18";

/// How one tool call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutcome {
    /// The tool ran; this text goes back to the model.
    Done(String),
    /// The broker refused; the reason goes back to the model as a tool error.
    Refused(String),
}

/// Runs the tools the bridge lists. The broker implements this.
pub trait ToolHandler {
    /// Runs `tool` with `arguments`. Only listed tools ever reach this.
    fn call(&mut self, tool: &str, arguments: &Value) -> ToolOutcome;
}

/// An MCP server for one attempt.
#[derive(Debug)]
pub struct McpServer<H> {
    tools: Vec<ToolDescription>,
    handler: H,
}

impl<H: ToolHandler> McpServer<H> {
    /// A server offering `tools`, run by `handler`.
    pub fn new(tools: Vec<ToolDescription>, handler: H) -> Self {
        Self { tools, handler }
    }

    /// Serves requests from `input` until it ends, writing responses to `output`.
    ///
    /// # Errors
    ///
    /// Returns the framing error if a frame is malformed, truncated or too large.
    pub fn serve(&mut self, input: impl BufRead, output: &mut dyn Write) -> Result<(), FrameError> {
        let mut reader = FrameReader::new(input, DEFAULT_FRAME_LIMIT);
        while let Some(request) = reader.read_frame::<Value>()? {
            // Notifications carry no id and get no response.
            let Some(id) = request.get("id").cloned() else {
                continue;
            };
            let method: &str = request.get("method").and_then(Value::as_str).unwrap_or("");
            let response: Value = match method {
                "initialize" => {
                    let version = request
                        .pointer("/params/protocolVersion")
                        .and_then(Value::as_str)
                        .unwrap_or(DEFAULT_MCP_VERSION);
                    result(
                        &id,
                        json!({"protocolVersion": version, "capabilities": {"tools": {}},
                               "serverInfo": {"name": "kaisa", "version": env!("CARGO_PKG_VERSION")}}),
                    )
                }
                "tools/list" => {
                    let tools: Vec<Value> = self
                        .tools
                        .iter()
                        .map(|tool| json!({"name": tool.name, "description": tool.description, "inputSchema": tool.input_schema}))
                        .collect();
                    result(&id, json!({"tools": tools}))
                }
                "tools/call" => self.call(&id, &request),
                _ => error(&id, METHOD_NOT_FOUND, &format!("unknown method {method:?}")),
            };
            write_frame(output, &response)?;
        }
        Ok(())
    }

    fn call(&mut self, id: &Value, request: &Value) -> Value {
        let name: &str = request
            .pointer("/params/name")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !self.tools.iter().any(|tool| tool.name == name) {
            return error(
                id,
                INVALID_PARAMS,
                &format!("no tool named {name:?} for this attempt"),
            );
        }
        let arguments: Value = request
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or(Value::Null);
        let (text, is_error) = match self.handler.call(name, &arguments) {
            ToolOutcome::Done(text) => (text, false),
            ToolOutcome::Refused(reason) => (reason, true),
        };
        result(
            id,
            json!({"content": [{"type": "text", "text": text}], "isError": is_error}),
        )
    }
}

fn result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

#[cfg(test)]
mod tests {
    use kaisa_domain::Role;
    use serde_json::{Value, json};

    use super::{McpServer, ToolHandler, ToolOutcome};
    use crate::tools::tools_for;

    /// JSON-RPC: the method does not exist.
    const METHOD_NOT_FOUND: i64 = -32_601;

    /// A handler that must never be reached in these tests.
    struct NoTools;

    impl ToolHandler for NoTools {
        fn call(&mut self, tool: &str, _arguments: &Value) -> ToolOutcome {
            ToolOutcome::Refused(format!("{tool} is not expected in this test"))
        }
    }

    /// Feeds `requests` to a bridge for `role` and returns every response.
    fn exchange(role: Role, requests: &[Value]) -> Vec<Value> {
        let input: String = requests
            .iter()
            .map(|request| format!("{request}\n"))
            .collect();
        let mut output: Vec<u8> = Vec::new();
        McpServer::new(tools_for(role), NoTools)
            .serve(input.as_bytes(), &mut output)
            .expect("well-formed frames are served");
        String::from_utf8(output)
            .expect("responses are UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("each response is JSON"))
            .collect()
    }

    #[test]
    fn the_bridge_lists_only_the_tools_of_the_attempts_role() {
        let responses: Vec<Value> = exchange(
            Role::Reviewer,
            &[
                json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            ],
        );
        let names: Vec<&str> = responses
            .last()
            .and_then(|list| list.pointer("/result/tools"))
            .and_then(Value::as_array)
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| tool["name"].as_str())
                    .collect()
            })
            .unwrap_or_default();

        assert_eq!(
            names,
            vec!["fs_read", "search"],
            "a reviewer can read and search, and has no way to write or run anything"
        );
    }

    #[test]
    fn an_unknown_method_gets_a_json_rpc_error() {
        let responses: Vec<Value> = exchange(
            Role::Developer,
            &[json!({"jsonrpc": "2.0", "id": 7, "method": "resources/list"})],
        );

        assert_eq!(
            responses
                .first()
                .and_then(|response| response.pointer("/error/code"))
                .and_then(Value::as_i64),
            Some(METHOD_NOT_FOUND),
            "anything the bridge does not offer is refused, never improvised"
        );
    }
}
