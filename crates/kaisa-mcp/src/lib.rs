//! The per-attempt MCP bridge (architecture §3, §5.6 "Brokered" trust mode).
//!
//! A brokered agent runs with its built-in tools disabled; this bridge is its only way to act.
//! MCP's stdio transport is newline-delimited JSON-RPC, the same framing as KRPC, so the bridge
//! reuses `kaisa-protocol`'s bounded frame reader. M0 implements the three methods an agent
//! needs (`initialize`, `tools/list`, `tools/call`); anything else gets a JSON-RPC error.

mod broker;
mod server;
mod tools;

pub use broker::{ExecSandbox, FileBroker};
pub use server::{McpServer, ToolHandler, ToolOutcome};
pub use tools::{ToolDescription, tools_for};
