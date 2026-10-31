//! Agent adapters (architecture §5.6): how Kaisa starts each agent CLI.
//!
//! M0 has the Claude Code adapter in brokered mode, as the T16.0 spike verified: every built-in
//! tool disabled, only the attempt's MCP bridge configured, the context pack sent as the first
//! message on standard input (never on the command line, which Windows limits to 32K
//! characters).

mod claude;
mod codex;
mod local_loop;
mod openai_local;
mod transcript;
mod usage;

pub use claude::{
    ClaudeLaunch, claude_brokered_argv, claude_environment, claude_final_answer,
    claude_permission_denials, first_message,
};
pub use codex::{
    CODEX_EFFORTS, CodexLaunch, CodexLaunchError, TrustMode, codex_environment, codex_final_answer,
    codex_observed_argv, codex_trust_mode,
};
pub use local_loop::{
    LoopOutcome, MAX_TOOL_RESULT_BYTES, ModelReply, StopReason, ToolSpec, parse_reply, run_loop,
    system_prompt,
};
pub use openai_local::{ChatMessage, LocalEndpoint, LocalModelError, MAX_RESPONSE_SIZE, chat};
pub use transcript::final_answer;
pub use usage::{Usage, parse_usage};

/// Whether the adapter requires the login keychain for its subscription credential.
/// Local models and other CLI adapters do not receive this exception.
#[must_use]
pub fn needs_keychain_login(program: &std::path::Path) -> bool {
    program.file_stem().is_some_and(|name| name == "claude")
}

#[cfg(test)]
mod capability_tests {
    #[test]
    fn only_the_claude_adapter_requests_login_keychain_access() {
        for (program, expected) in [
            ("/usr/local/bin/claude", true),
            ("codex", false),
            ("kaisa-fake-agent", false),
            ("kaisa", false),
        ] {
            assert_eq!(
                super::needs_keychain_login(std::path::Path::new(program)),
                expected,
                "the adapter capability must be explicit for {program}"
            );
        }
    }
}
