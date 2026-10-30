//! The per-attempt MCP bridge (architecture §3, §5.6 "Brokered" trust mode).
//!
//! A brokered agent runs with its built-in tools disabled; this bridge is its only way to act.
//! MCP's stdio transport is newline-delimited JSON-RPC, the same framing as KRPC, so the bridge
//! reuses `kaisa-protocol`'s bounded frame reader. M0 implements the three methods an agent
//! needs (`initialize`, `tools/list`, `tools/call`); anything else gets a JSON-RPC error.

mod attempt;
mod broker;
#[cfg(unix)]
mod endpoint;
#[cfg(windows)]
mod endpoint_windows;
mod server;
mod token;
mod tools;

pub use attempt::{BRIDGE_CONFIG_FILE, BridgeConfig, bridge, serve_attempt};
pub use broker::{ExecSandbox, FileBroker};
#[cfg(unix)]
pub use endpoint::{BROKER_ENDPOINT_FILE, BROKER_SOCKET, BrokerEndpoint, relay};
#[cfg(windows)]
pub use endpoint_windows::{BROKER_ENDPOINT_FILE, BrokerEndpoint, CONTAINER_SID_FILE, relay};
pub use server::{McpServer, ToolHandler, ToolOutcome};
pub use token::{AttemptToken, TOKEN_FILE};
pub use tools::{ToolDescription, tools_for};
