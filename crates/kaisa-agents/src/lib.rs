//! Agent adapters (architecture §5.6): how Kaisa starts each agent CLI.
//!
//! M0 has the Claude Code adapter in brokered mode, as the T16.0 spike verified: every built-in
//! tool disabled, only the attempt's MCP bridge configured, the context pack sent as the first
//! message on standard input (never on the command line, which Windows limits to 32K
//! characters).

mod claude;
mod codex;

pub use claude::{ClaudeLaunch, claude_brokered_argv, first_message};
pub use codex::{CodexLaunch, CodexLaunchError, TrustMode, codex_observed_argv, codex_trust_mode};
