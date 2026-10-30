//! A pure text protocol and bounded agent loop for tool-less local models.

use serde_json::Value;

use crate::ChatMessage;

/// Maximum bytes of a tool result or error supplied back to the model, including the marker.
pub const MAX_TOOL_RESULT_BYTES: usize = 16 * 1024;
const TRUNCATION_MARKER: &str = "\n[tool output truncated]";
const ALLOWED_TOOLS: &[&str] = &["fs_read", "fs_write", "exec", "search"];

/// A broker tool advertised to the local model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    /// The broker tool's name.
    pub name: String,
    /// A short explanation of the tool's behavior.
    pub description: String,
    /// The JSON schema of the tool's arguments.
    pub arguments_schema: Value,
}

/// Builds deterministic instructions for the local model's text protocol.
#[must_use]
pub fn system_prompt(tools: &[ToolSpec]) -> String {
    let mut prompt = String::from(
        "Reply with exactly one JSON object: {\"tool\": \"<name>\", \"arguments\": {...}} \
         to call a listed tool, or {\"done\": \"<one-line summary>\"} to finish. \
         Arguments must be an object. Any other text is ignored.\nTools:\n",
    );
    for tool in tools {
        prompt.push_str(
            &serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "arguments_schema": tool.arguments_schema,
            })
            .to_string(),
        );
        prompt.push('\n');
    }
    prompt
}

/// One parsed action from a local model.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelReply {
    /// Invoke an allowed broker tool with object arguments.
    Tool {
        /// The broker tool's name.
        name: String,
        /// The arguments object supplied by the model.
        arguments: Value,
    },
    /// Finish with a one-line summary.
    Done(String),
    /// Correct an invalid response using this explanation.
    Invalid(String),
}

/// Parses a bare, fenced, or last embedded JSON object.
///
/// Tool names are restricted to `fs_read`, `fs_write`, `exec`, and `search`;
/// [`run_loop`] further restricts calls to its supplied tool specifications.
#[must_use]
pub fn parse_reply(text: &str) -> ModelReply {
    let mut last_object = None;
    let mut consumed = 0;
    for (start, character) in text.char_indices() {
        if start < consumed || character != '{' {
            continue;
        }
        let mut objects = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        if let Some(Ok(value)) = objects.next() {
            consumed = start + objects.byte_offset();
            last_object = Some(value);
        }
    }
    let Some(Value::Object(object)) = last_object else {
        return ModelReply::Invalid("Reply with a JSON object.".into());
    };
    if object.len() == 1
        && let Some(summary) = object.get("done").and_then(Value::as_str)
        && !summary.contains(['\r', '\n'])
    {
        return ModelReply::Done(summary.to_owned());
    }
    if object.len() == 2
        && let Some(name) = object.get("tool").and_then(Value::as_str)
        && let Some(arguments) = object.get("arguments").filter(|value| value.is_object())
    {
        if !ALLOWED_TOOLS.contains(&name) {
            return ModelReply::Invalid(format!("Tool {name:?} is not allowed."));
        }
        return ModelReply::Tool {
            name: name.to_owned(),
            arguments: arguments.clone(),
        };
    }
    ModelReply::Invalid(
        "Use only tool and object arguments, or done with a one-line string summary.".into(),
    )
}

/// Why the model loop stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The model returned a done response.
    Done,
    /// The configured number of model turns was exhausted.
    TurnLimit,
    /// The chat callback failed with this explanation.
    ChatFailed(String),
}

/// The result and accounting of a local model loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopOutcome {
    /// Whether the model explicitly finished.
    pub finished: bool,
    /// The model's summary, present only when it finished.
    pub summary: Option<String>,
    /// Number of chat attempts, including invalid replies and failed attempts.
    pub turns: usize,
    /// Number of tool callback invocations, including failed calls.
    pub tool_calls: usize,
    /// The reason no further model turns were attempted.
    pub stop: StopReason,
}

/// Runs the local model protocol using caller-owned chat and tool callbacks.
///
/// Invalid replies consume a turn and receive a correction. Tool results and errors
/// are bounded to [`MAX_TOOL_RESULT_BYTES`] before being added to the conversation.
#[must_use]
pub fn run_loop(
    pack: &str,
    tools: &[ToolSpec],
    max_turns: usize,
    chat: &mut dyn FnMut(&[ChatMessage]) -> Result<String, String>,
    call_tool: &mut dyn FnMut(&str, &Value) -> Result<String, String>,
) -> LoopOutcome {
    let mut messages = vec![
        message("system", system_prompt(tools)),
        message("user", pack.into()),
    ];
    let mut outcome = LoopOutcome {
        finished: false,
        summary: None,
        turns: 0,
        tool_calls: 0,
        stop: StopReason::TurnLimit,
    };
    for _ in 0..max_turns {
        outcome.turns += 1;
        let reply = match chat(&messages) {
            Ok(reply) => reply,
            Err(error) => {
                outcome.stop = StopReason::ChatFailed(error);
                return outcome;
            }
        };
        messages.push(message("assistant", reply.clone()));
        let correction = match parse_reply(&reply) {
            ModelReply::Tool { name, arguments } => {
                if tools.iter().any(|tool| tool.name == name) {
                    outcome.tool_calls += 1;
                    let (prefix, result) = match call_tool(&name, &arguments) {
                        Ok(result) => ("tool result", result),
                        Err(error) => ("tool error", error),
                    };
                    messages.push(message(
                        "user",
                        format!("{prefix}:\n{}", bounded_result(&result)),
                    ));
                    continue;
                }
                format!("Tool {name:?} is not listed for this attempt.")
            }
            ModelReply::Done(summary) => {
                outcome.finished = true;
                outcome.summary = Some(summary);
                outcome.stop = StopReason::Done;
                return outcome;
            }
            ModelReply::Invalid(reason) => reason,
        };
        messages.push(message("user", format!("Invalid reply: {correction} Reply with exactly one JSON object using a listed tool or done.")));
    }
    outcome
}

fn message(role: &str, content: String) -> ChatMessage {
    ChatMessage {
        role: role.into(),
        content,
    }
}

fn bounded_result(result: &str) -> String {
    if result.len() <= MAX_TOOL_RESULT_BYTES {
        return result.to_owned();
    }
    let mut end = MAX_TOOL_RESULT_BYTES - TRUNCATION_MARKER.len();
    while !result.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATION_MARKER}", &result[..end])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const TURN_LIMIT: usize = 2;
    const ONE_TURN: usize = 1;
    const ONE_TOOL_CALL: usize = 1;
    const NO_CALLS: usize = 0;
    const PACK: &str = "Read the project notes.";
    const TOOL_REPLY: &str = r#"{"tool":"fs_read","arguments":{"path":"notes.md"}}"#;
    const DONE_REPLY: &str = r#"{"done":"Reviewed notes."}"#;

    fn tools() -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "fs_read".into(),
            description: "Read project notes.".into(),
            arguments_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
        }]
    }

    #[test]
    fn a_tool_call_reply_is_parsed() {
        assert_eq!(
            parse_reply(TOOL_REPLY),
            ModelReply::Tool {
                name: "fs_read".into(),
                arguments: json!({"path":"notes.md"})
            },
            "a broker call retains its name and object arguments"
        );
    }

    #[test]
    fn a_reply_in_a_fenced_block_is_parsed() {
        assert_eq!(
            parse_reply(&format!("```json\n{TOOL_REPLY}\n```")),
            parse_reply(TOOL_REPLY),
            "JSON fences do not change the tool action"
        );
    }

    #[test]
    fn a_tool_outside_the_allowed_list_is_invalid() {
        assert!(
            matches!(
                parse_reply(r#"{"tool":"network","arguments":{}}"#),
                ModelReply::Invalid(_)
            ),
            "unknown tools must never become callable actions"
        );
    }

    #[test]
    fn the_last_embedded_object_is_parsed_with_nested_arguments() {
        assert_eq!(
            parse_reply(&format!(
                "Earlier: {DONE_REPLY}\nNow: {TOOL_REPLY}\nFinished reasoning."
            )),
            parse_reply(TOOL_REPLY),
            "the last complete outer object wins without selecting its nested arguments"
        );
    }

    #[test]
    fn malformed_actions_are_invalid() {
        for reply in [
            "no action",
            "[]",
            r#"{"tool":"exec","arguments":[]}"#,
            r#"{"done":"first\nsecond"}"#,
            r#"{"done":"done","tool":"exec","arguments":{}}"#,
        ] {
            assert!(
                matches!(parse_reply(reply), ModelReply::Invalid(_)),
                "malformed action {reply:?} must receive a correction"
            );
        }
    }

    #[test]
    fn the_loop_runs_a_tool_and_stops_when_done() {
        let mut replies = [TOOL_REPLY, DONE_REPLY].into_iter();
        let outcome = run_loop(
            PACK,
            &tools(),
            TURN_LIMIT,
            &mut |messages| {
                assert_eq!(
                    messages.first().expect("the system message exists").role,
                    "system",
                    "the protocol leads the conversation"
                );
                assert_eq!(
                    messages
                        .get(ONE_TURN)
                        .expect("the pack message exists")
                        .content,
                    PACK,
                    "the supplied pack is the first user message"
                );
                Ok(replies
                    .next()
                    .expect("each turn has a fixture reply")
                    .into())
            },
            &mut |name, arguments| {
                assert_eq!(name, "fs_read", "the requested tool is dispatched");
                assert_eq!(
                    arguments,
                    &json!({"path":"notes.md"}),
                    "the broker receives unchanged arguments"
                );
                Ok("Project notes".into())
            },
        );
        assert_eq!(
            outcome,
            LoopOutcome {
                finished: true,
                summary: Some("Reviewed notes.".into()),
                turns: TURN_LIMIT,
                tool_calls: ONE_TOOL_CALL,
                stop: StopReason::Done
            },
            "a tool turn followed by done finishes with accurate accounting"
        );
    }

    #[test]
    fn the_loop_stops_at_its_turn_limit() {
        let outcome = run_loop(
            PACK,
            &tools(),
            TURN_LIMIT,
            &mut |_| Ok(TOOL_REPLY.into()),
            &mut |_, _| Ok("notes".into()),
        );
        assert_eq!(
            outcome,
            LoopOutcome {
                finished: false,
                summary: None,
                turns: TURN_LIMIT,
                tool_calls: TURN_LIMIT,
                stop: StopReason::TurnLimit
            },
            "a model that keeps calling tools cannot exceed the turn budget"
        );
    }

    #[test]
    fn an_invalid_reply_is_corrected_not_executed() {
        let mut replies = ["not JSON", DONE_REPLY].into_iter();
        let outcome = run_loop(
            PACK,
            &tools(),
            TURN_LIMIT,
            &mut |messages| {
                if messages.len() > TURN_LIMIT {
                    let correction = messages.last().expect("the correction exists");
                    assert_eq!(correction.role, "user", "corrections use the user role");
                    assert!(
                        correction.content.starts_with("Invalid reply:"),
                        "the next turn explains how to correct the protocol"
                    );
                }
                Ok(replies
                    .next()
                    .expect("each turn has a fixture reply")
                    .into())
            },
            &mut |_, _| panic!("an invalid reply must not invoke a tool"),
        );
        assert_eq!(
            outcome.tool_calls, NO_CALLS,
            "invalid replies are never dispatched"
        );
        assert_eq!(
            outcome.turns, TURN_LIMIT,
            "a correction consumes its model turn"
        );
        assert!(outcome.finished, "the corrected model can finish");
    }

    #[test]
    fn a_long_tool_result_is_cut_before_it_reaches_the_model() {
        let mut replies = [TOOL_REPLY, DONE_REPLY].into_iter();
        let oversized_notes = "界".repeat(MAX_TOOL_RESULT_BYTES);
        let outcome = run_loop(
            PACK,
            &tools(),
            TURN_LIMIT,
            &mut |messages| {
                if messages.len() > TURN_LIMIT {
                    let feedback = messages
                        .last()
                        .expect("tool feedback exists")
                        .content
                        .strip_prefix("tool result:\n")
                        .expect("successful feedback has the result prefix");
                    assert!(
                        feedback.len() <= MAX_TOOL_RESULT_BYTES,
                        "feedback respects the named byte budget"
                    );
                    assert!(
                        feedback.ends_with(TRUNCATION_MARKER),
                        "truncated output is explicitly marked"
                    );
                    assert!(
                        feedback.starts_with('界'),
                        "multibyte text survives truncation on a character boundary"
                    );
                }
                Ok(replies
                    .next()
                    .expect("each turn has a fixture reply")
                    .into())
            },
            &mut |_, _| Ok(oversized_notes.clone()),
        );
        assert!(
            outcome.finished,
            "bounded feedback allows the next model turn"
        );
    }

    #[test]
    fn an_unlisted_broker_tool_is_not_executed() {
        let outcome = run_loop(
            PACK,
            &[],
            ONE_TURN,
            &mut |_| Ok(TOOL_REPLY.into()),
            &mut |_, _| panic!("an unlisted tool must not execute"),
        );
        assert_eq!(
            outcome.tool_calls, NO_CALLS,
            "the attempt tool list further restricts broker tools"
        );
    }

    #[test]
    fn a_chat_failure_stops_the_loop_and_counts_the_attempt() {
        let failure = "local model unavailable";
        let outcome = run_loop(
            PACK,
            &tools(),
            TURN_LIMIT,
            &mut |_| Err(failure.into()),
            &mut |_, _| panic!("failed chat cannot dispatch tools"),
        );
        assert_eq!(
            outcome.stop,
            StopReason::ChatFailed(failure.into()),
            "the caller receives the chat failure reason"
        );
        assert_eq!(
            outcome.turns, ONE_TURN,
            "the failed chat attempt consumes a turn"
        );
        assert!(
            !outcome.finished,
            "a transport failure is not model completion"
        );
    }

    #[test]
    fn a_tool_error_is_returned_to_the_model() {
        let mut replies = [TOOL_REPLY, DONE_REPLY].into_iter();
        let outcome = run_loop(
            PACK,
            &tools(),
            TURN_LIMIT,
            &mut |messages| {
                if messages.len() > TURN_LIMIT {
                    assert_eq!(
                        messages.last().expect("tool feedback exists").content,
                        "tool error:\naccess refused",
                        "tool failures are delivered as user feedback"
                    );
                }
                Ok(replies
                    .next()
                    .expect("each turn has a fixture reply")
                    .into())
            },
            &mut |_, _| Err("access refused".into()),
        );
        assert_eq!(
            outcome.tool_calls, ONE_TOOL_CALL,
            "failed calls are included in tool accounting"
        );
        assert!(
            outcome.finished,
            "tool failure does not prevent the model from finishing"
        );
    }

    #[test]
    fn a_zero_turn_budget_calls_neither_callback() {
        let outcome = run_loop(
            PACK,
            &tools(),
            NO_CALLS,
            &mut |_| panic!("zero budget cannot chat"),
            &mut |_, _| panic!("zero budget cannot call tools"),
        );
        assert_eq!(
            outcome.turns, NO_CALLS,
            "no turns are attempted with an empty budget"
        );
        assert_eq!(
            outcome.stop,
            StopReason::TurnLimit,
            "an empty budget stops at the limit"
        );
    }
}
