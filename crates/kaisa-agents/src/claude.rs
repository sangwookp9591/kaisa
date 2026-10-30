//! The Claude Code adapter in brokered mode.

use std::path::Path;

use kaisa_context::ContextPack;
use serde_json::json;

/// What the router chose for one Claude attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeLaunch<'launch> {
    /// The provider model id, for example `claude-sonnet-5-5`.
    pub model: &'launch str,
    /// The effort level, for example `medium`.
    pub effort: &'launch str,
    /// The MCP configuration that names only this attempt's bridge.
    pub mcp_config: &'launch Path,
}

/// The arguments that run Claude Code headless with no built-in tools and only Kaisa's bridge.
#[must_use]
pub fn claude_brokered_argv(launch: &ClaudeLaunch<'_>) -> Vec<String> {
    [
        "-p",
        "--tools",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        &launch.mcp_config.to_string_lossy(),
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--model",
        launch.model,
        "--effort",
        launch.effort,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// The first line written to Claude's standard input: the rendered context pack as a user
/// message in the `stream-json` input format.
#[must_use]
pub fn first_message(pack: &ContextPack) -> String {
    json!({"type": "user", "message": {"role": "user", "content": pack.render()}}).to_string()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use kaisa_context::{ContextPack, PackInputs, ProjectProfile};
    use kaisa_domain::{RepoPath, WriteSet};
    use serde_json::Value;

    use super::{ClaudeLaunch, claude_brokered_argv, first_message};

    const LOGIN_FIX_SPEC: &str = "Make the login test wait for the token refresh.";

    fn a_sonnet_launch() -> ClaudeLaunch<'static> {
        ClaudeLaunch {
            model: "claude-sonnet-5-5",
            effort: "medium",
            mcp_config: Path::new("/tmp/att_41a/mcp.json"),
        }
    }

    fn a_login_fix_pack() -> ContextPack {
        ContextPack::assemble(PackInputs {
            spec: LOGIN_FIX_SPEC,
            acceptance: &["the login tests pass".to_owned()],
            write_set: &WriteSet::new(vec![RepoPath::try_new("src/auth").expect("valid")]),
            profile: &ProjectProfile::default(),
            findings: &[],
            impact: &[],
        })
    }

    #[test]
    fn the_claude_command_disables_every_built_in_tool() {
        let argv: Vec<String> = claude_brokered_argv(&a_sonnet_launch());
        let tools_value: Option<&String> = argv
            .iter()
            .position(|argument| argument == "--tools")
            .and_then(|index| argv.get(index + 1));

        assert_eq!(
            (
                tools_value.map(String::as_str),
                argv.contains(&"--strict-mcp-config".to_owned())
            ),
            (Some(""), true),
            "a brokered agent has no tools but Kaisa's, and no MCP servers but the bridge"
        );
    }

    #[test]
    fn the_first_message_is_the_rendered_context_pack() {
        let pack: ContextPack = a_login_fix_pack();

        let message: Value =
            serde_json::from_str(&first_message(&pack)).expect("the first message is JSON");

        assert_eq!(
            message.pointer("/message/content").and_then(Value::as_str),
            Some(pack.render().as_str()),
            "the agent starts from exactly the context Kaisa recorded"
        );
    }
}
