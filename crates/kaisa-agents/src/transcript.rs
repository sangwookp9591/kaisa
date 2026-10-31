//! Reading what an agent said: the JSON events of its transcript and its final answer.
//!
//! Kaisa keeps an agent's standard output and error interleaved as its transcript. Claude Code
//! (`--output-format stream-json`) and Codex (`exec --json`) write one JSON event per line, so
//! the line-oriented error output around them is skipped. What an agent finally answered is in
//! its own event: never the transcript's last line, which for both is a bookkeeping event.

use serde_json::Value;

use crate::claude::claude_final_answer;
use crate::codex::codex_final_answer;

/// The JSON objects in `transcript`, one per line; any other line is skipped.
pub(crate) fn events(transcript: &str) -> impl Iterator<Item = Value> + '_ {
    transcript
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
}

/// An event's `type`, if it has one.
pub(crate) fn event_type(event: &Value) -> Option<&str> {
    event.get("type").and_then(Value::as_str)
}

/// Event types only Claude Code's stream-json output has.
const CLAUDE_EVENTS: [&str; 3] = ["system", "assistant", "result"];

fn is_codex_event(kind: &str) -> bool {
    kind == "error"
        || ["thread.", "turn.", "item."]
            .iter()
            .any(|prefix| kind.starts_with(prefix))
}

/// The final answer an agent gave in `transcript`, in whichever format it wrote:
///
/// - Claude Code's stream-json: the text of its last `result` event, if that run succeeded;
/// - Codex's `exec --json`: its last `agent_message`, if the turn then completed;
/// - plain text (a local model's loop or a scripted agent): the complete text.
///
/// A transcript with Claude or Codex events but no such answer, or with events of both, has
/// no final answer: nothing falls back to a raw line of it.
#[must_use]
pub fn final_answer(transcript: &str) -> Option<String> {
    let (mut claude, mut codex) = (false, false);
    for event in events(transcript) {
        match event_type(&event) {
            Some(kind) if CLAUDE_EVENTS.contains(&kind) => claude = true,
            Some(kind) if is_codex_event(kind) => codex = true,
            _ => {}
        }
    }
    match (claude, codex) {
        (true, false) => claude_final_answer(transcript),
        (false, true) => codex_final_answer(transcript),
        (false, false) => {
            Some(transcript.trim_end().to_owned()).filter(|answer| !answer.is_empty())
        }
        (true, true) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::final_answer;

    /// A real `claude -p --output-format stream-json --verbose` review (Claude Code 2.1.316,
    /// claude-haiku-5-5), ids and paths redacted, its answer replaced by an approval that wraps
    /// the verdict in prose and a Markdown code fence, as the model answered when captured.
    const CLAUDE_APPROVES: &str = include_str!("fixtures/claude-review-approves.jsonl");
    /// The same run, its answer only prose.
    const CLAUDE_PROSE_ONLY: &str = include_str!("fixtures/claude-review-prose-only.jsonl");
    /// A real `codex exec --json` review (codex-cli 0.166.0), ids and paths redacted, with a
    /// line of Codex's standard error where Kaisa's capture interleaves it.
    const CODEX_APPROVES: &str = include_str!("fixtures/codex-review-approves.jsonl");

    /// The approval both captured answers carry.
    const APPROVING_VERDICT: &str = r#"{"candidate_tree": "4b825dc642cb6eb9a060e54bf8d69288fbee4904", "approve": true, "findings": []}"#;

    /// `transcript` without its lines whose JSON `type` is `kind`.
    fn without_events(transcript: &str, kind: &str) -> String {
        transcript
            .lines()
            .filter(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .map_or(true, |event| event["type"] != kind)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `transcript` with its `result` event reporting a failed run.
    fn with_a_failed_result(transcript: &str) -> String {
        transcript
            .lines()
            .map(
                |line| match serde_json::from_str::<serde_json::Value>(line) {
                    Ok(mut event) if event["type"] == "result" => {
                        event["is_error"] = serde_json::Value::Bool(true);
                        event["subtype"] = "error_during_execution".into();
                        event.to_string()
                    }
                    _ => line.to_owned(),
                },
            )
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn a_new_codex_turn_after_approval(boundary: &str, completed: bool) -> String {
        let mut stream = format!("{CODEX_APPROVES}{{\"type\":\"{boundary}\"}}\n");
        if completed {
            stream.push_str("{\"type\":\"turn.completed\"}\n");
        }
        stream
    }

    fn a_failed_codex_transcript() -> String {
        format!(
            "{CODEX_APPROVES}{{\"type\":\"turn.failed\",\"error\":{{\"message\":\"stream closed\"}}}}\n"
        )
    }

    fn a_failed_codex_turn_with_a_late_message_and_completion() -> String {
        format!(
            "{}{}",
            a_failed_codex_transcript(),
            CODEX_APPROVES
                .lines()
                .filter(|line| !line.contains("thread.started") && !line.contains("turn.started"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }

    fn a_mixed_provider_transcript() -> String {
        format!("{CLAUDE_APPROVES}{CODEX_APPROVES}")
    }

    fn a_plain_local_review_transcript() -> String {
        format!(
            "local agent: finished after 2 turns\n```kaisa-verdict\n{APPROVING_VERDICT}\n```\n\n"
        )
    }

    fn an_indented_plain_verdict_marker() -> String {
        format!(" kaisa-verdict: {APPROVING_VERDICT}")
    }

    #[test]
    fn a_plain_answer_keeps_indentation_so_a_quote_cannot_become_a_verdict() {
        let answer = an_indented_plain_verdict_marker();
        assert_eq!(
            final_answer(&answer),
            Some(answer),
            "extracting plain output must not promote an indented marker to column one"
        );
    }

    #[test]
    fn a_failed_codex_turn_cannot_be_revived_by_a_late_completion() {
        assert_eq!(
            final_answer(&a_failed_codex_turn_with_a_late_message_and_completion()),
            None,
            "turn.failed remains terminal until an explicit new turn or thread starts"
        );
    }

    #[test]
    fn a_new_codex_turn_or_thread_cannot_reuse_a_previous_approval() {
        for boundary in ["turn.started", "thread.started"] {
            for completed in [false, true] {
                assert_eq!(
                    final_answer(&a_new_codex_turn_after_approval(boundary, completed)),
                    None,
                    "{boundary} resets both the previous message and completion, even when completed={completed}"
                );
            }
        }
    }

    #[test]
    fn a_claude_stream_json_review_answers_with_its_result_text() {
        let answer = final_answer(CLAUDE_APPROVES).expect("the result event holds the answer");

        assert!(
            answer.contains(APPROVING_VERDICT) && answer.contains("```kaisa-verdict"),
            "the answer is the model's final text, prose and code fence included: {answer:?}"
        );
        assert!(
            !CLAUDE_APPROVES
                .lines()
                .last()
                .is_some_and(|line| line.contains(&answer)),
            "the transcript's last line is the result event, not the answer itself"
        );
    }

    #[test]
    fn a_claude_review_without_a_result_event_has_no_final_answer() {
        let cut_short = without_events(CLAUDE_APPROVES, "result");

        assert_eq!(
            final_answer(&cut_short),
            None,
            "an assistant message before an unfinished run is not its final answer"
        );
    }

    #[test]
    fn a_failed_claude_run_has_no_final_answer() {
        assert_eq!(
            final_answer(&with_a_failed_result(CLAUDE_APPROVES)),
            None,
            "a run that ended in error answered nothing, whatever text it left"
        );
    }

    #[test]
    fn a_claude_review_in_prose_answers_with_that_prose() {
        let answer = final_answer(CLAUDE_PROSE_ONLY).expect("the result event holds the answer");

        assert!(
            !answer.contains("candidate_tree"),
            "the prose is the answer; there is no verdict in it to find: {answer:?}"
        );
    }

    #[test]
    fn a_codex_review_answers_with_its_last_agent_message() {
        let answer = final_answer(CODEX_APPROVES).expect("the agent message holds the answer");

        assert!(
            answer.contains(APPROVING_VERDICT),
            "the answer is Codex's last agent message, not its usage event: {answer:?}"
        );
    }

    #[test]
    fn a_codex_turn_that_never_completed_has_no_final_answer() {
        let cut_short = without_events(CODEX_APPROVES, "turn.completed");

        assert_eq!(
            final_answer(&cut_short),
            None,
            "a message from a turn that did not complete is not a final answer"
        );
    }

    #[test]
    fn a_failed_codex_turn_has_no_final_answer() {
        let failed = a_failed_codex_transcript();

        assert_eq!(
            final_answer(&failed),
            None,
            "a turn that failed after its message gave no final answer"
        );
    }

    #[test]
    fn a_transcript_with_both_agents_events_has_no_final_answer() {
        let mixed = a_mixed_provider_transcript();

        assert_eq!(
            final_answer(&mixed),
            None,
            "one attempt is one agent; a transcript claiming two is not trusted for either"
        );
    }

    #[test]
    fn a_plain_transcript_preserves_the_complete_final_verdict_region() {
        let transcript = a_plain_local_review_transcript();

        assert_eq!(
            final_answer(&transcript).as_deref(),
            Some(transcript.trim()),
            "plain output must preserve multiline verdict fences"
        );
    }
}
