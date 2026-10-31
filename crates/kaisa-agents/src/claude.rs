//! The Claude Code adapter in brokered mode.

use std::path::Path;

use kaisa_context::ContextPack;
use serde_json::{Value, json};

use crate::transcript::{event_type, events};

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
        "--allowedTools",
        "mcp__kaisa__*",
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

/// Tool names Claude reported as denied in result events, in first-seen order.
/// A zero process exit does not imply that its requested tools were permitted.
#[must_use]
pub fn claude_permission_denials(transcript: &str) -> Vec<String> {
    let mut tools = Vec::new();
    for result in events(transcript).filter(|event| event_type(event) == Some("result")) {
        if let Some(denials) = result.get("permission_denials").and_then(Value::as_array) {
            for denial in denials {
                if let Some(name) = denial.get("tool_name").and_then(Value::as_str)
                    && !tools.iter().any(|tool| tool == name)
                {
                    tools.push(name.to_owned());
                }
            }
        }
    }
    tools
}

/// What Claude Code finally answered in a stream-json `transcript`: the `result` text of its
/// last `result` event, if that run succeeded. An assistant message is never taken instead: one
/// before a missing or failed result may be a step the model went on from, not its answer.
#[must_use]
pub fn claude_final_answer(transcript: &str) -> Option<String> {
    let result: Value = events(transcript)
        .filter(|event| event_type(event) == Some("result"))
        .last()?;
    let succeeded: bool = result.get("subtype").and_then(Value::as_str) == Some("success")
        && result.get("is_error").and_then(Value::as_bool) == Some(false);
    if !succeeded {
        return None;
    }
    result
        .get("result")
        .and_then(Value::as_str)
        .map(str::to_owned)
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
            diff: "",
            lessons: &[],
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
    fn a_brokered_launch_preapproves_only_kaisas_mcp_tools() {
        let argv = claude_brokered_argv(&a_sonnet_launch());
        let allowed = argv.windows(2).find(|pair| pair[0] == "--allowedTools");
        assert_eq!(
            allowed.map(|pair| pair[1].as_str()),
            Some("mcp__kaisa__*"),
            "headless Claude must be allowed to call the attempt's bridge"
        );
        assert!(
            !argv
                .iter()
                .any(|arg| arg == "--dangerously-skip-permissions"),
            "preapproving the bridge must not bypass other permission checks"
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
