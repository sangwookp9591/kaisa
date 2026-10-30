//! Which broker tools each role gets (architecture §5.2 role table, §5.6 broker contracts).

use kaisa_domain::Role;
use serde_json::{Value, json};

/// A tool as `tools/list` describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDescription {
    /// MCP tool name (letters, digits, `_` and `-` only).
    pub name: &'static str,
    /// What the tool does, for the model.
    pub description: &'static str,
    /// JSON schema of the arguments.
    pub input_schema: Value,
}

fn fs_read() -> ToolDescription {
    ToolDescription {
        name: "fs_read",
        description: "Read a file of the repository, by repository-relative path.",
        input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
    }
}

fn fs_write() -> ToolDescription {
    ToolDescription {
        name: "fs_write",
        description: "Write a whole file inside your write set. Paths outside it are refused.",
        input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"]}),
    }
}

fn exec() -> ToolDescription {
    ToolDescription {
        name: "exec",
        description: "Run a program (no shell) in the sandbox, with a deadline and an output cap.",
        input_schema: json!({"type": "object", "properties": {"argv": {"type": "array", "items": {"type": "string"}}}, "required": ["argv"]}),
    }
}

fn search() -> ToolDescription {
    ToolDescription {
        name: "search",
        description: "Search the repository's files for a text.",
        input_schema: json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
    }
}

/// The tools an attempt with `role` may use; nothing else is ever listed or callable.
#[must_use]
pub fn tools_for(role: Role) -> Vec<ToolDescription> {
    match role {
        Role::Developer | Role::Tester => vec![fs_read(), fs_write(), exec(), search()],
        Role::Documentation => vec![fs_read(), fs_write(), search()],
        Role::Coordinator => Vec::new(),
        _ => vec![fs_read(), search()],
    }
}
