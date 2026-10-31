//! Token and cost totals recorded in agent JSONL transcripts.
//!
//! A read-only `codex exec --json` probe observed a `turn.completed` event with a `usage`
//! object containing `input_tokens`, `cached_input_tokens`, `cache_write_input_tokens`,
//! `output_tokens`, and `reasoning_output_tokens`. The input and output fields are totals;
//! the cache and reasoning fields are not added again. Codex supplies no cost field.

use serde_json::Value;

use crate::transcript::{event_type, events};

/// The token totals and optional provider-reported cost of an agent attempt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Usage {
    /// Input tokens, including cached input.
    pub input_tokens: u64,
    /// Output tokens reported by the provider.
    pub output_tokens: u64,
    /// The provider-reported total in US dollars, when available.
    pub cost_usd: Option<f64>,
}

/// Reads the last usage event from a Claude Code or Codex JSONL transcript.
///
/// Non-JSON lines and unrelated events are ignored. Claude's last `result` or Codex's
/// last `turn.completed` replaces earlier totals. Missing, invalid, or overflowing token
/// counts in that final event yield `None`; an invalid or missing cost remains unavailable.
#[must_use]
pub fn parse_usage(transcript: &str) -> Option<Usage> {
    let mut usage = None;
    for event in events(transcript) {
        match event_type(&event) {
            Some("result") => usage = claude_usage(&event),
            Some("turn.completed") => usage = codex_usage(&event),
            _ => {}
        }
    }
    usage
}

fn claude_usage(event: &Value) -> Option<Usage> {
    let tokens = event.get("usage")?;
    let mut input_tokens = tokens.get("input_tokens")?.as_u64()?;
    for field in ["cache_creation_input_tokens", "cache_read_input_tokens"] {
        if let Some(cached_tokens) = tokens.get(field) {
            input_tokens = input_tokens.checked_add(cached_tokens.as_u64()?)?;
        }
    }
    Some(Usage {
        input_tokens,
        output_tokens: tokens.get("output_tokens")?.as_u64()?,
        cost_usd: event
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .filter(|cost| cost.is_finite() && *cost >= 0.0),
    })
}

fn codex_usage(event: &Value) -> Option<Usage> {
    let tokens = event.get("usage")?;
    Some(Usage {
        input_tokens: tokens.get("input_tokens")?.as_u64()?,
        output_tokens: tokens.get("output_tokens")?.as_u64()?,
        cost_usd: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{Usage, parse_usage};

    const INPUT_TOKENS: u64 = 120;
    const OUTPUT_TOKENS: u64 = 30;
    const CACHE_CREATION_TOKENS: u64 = 40;
    const CACHE_READ_TOKENS: u64 = 60;
    const COST_USD: f64 = 0.012;

    fn a_claude_result() -> Value {
        json!({
            "type": "result",
            "usage": {"input_tokens": INPUT_TOKENS, "output_tokens": OUTPUT_TOKENS},
            "total_cost_usd": COST_USD,
        })
    }

    fn a_codex_turn() -> Value {
        json!({
            "type": "turn.completed",
            "usage": {
                "input_tokens": INPUT_TOKENS,
                "cached_input_tokens": CACHE_READ_TOKENS,
                "cache_write_input_tokens": CACHE_CREATION_TOKENS,
                "output_tokens": OUTPUT_TOKENS,
                "reasoning_output_tokens": OUTPUT_TOKENS,
            },
        })
    }

    fn claude_totals() -> Usage {
        Usage {
            input_tokens: INPUT_TOKENS,
            output_tokens: OUTPUT_TOKENS,
            cost_usd: Some(COST_USD),
        }
    }

    #[test]
    fn a_claude_result_event_gives_its_tokens_and_cost() {
        assert_eq!(
            parse_usage(&a_claude_result().to_string()),
            Some(claude_totals()),
            "Claude's final result supplies both token counts and the dollar cost"
        );
    }

    #[test]
    fn cached_input_counts_as_input() {
        let mut result = a_claude_result();
        result["usage"]["cache_creation_input_tokens"] = json!(CACHE_CREATION_TOKENS);
        result["usage"]["cache_read_input_tokens"] = json!(CACHE_READ_TOKENS);
        let expected = Usage {
            input_tokens: INPUT_TOKENS + CACHE_CREATION_TOKENS + CACHE_READ_TOKENS,
            ..claude_totals()
        };

        assert_eq!(
            parse_usage(&result.to_string()),
            Some(expected),
            "both cache creation and cache reads contribute to Claude's input total"
        );
    }

    #[test]
    fn the_last_result_event_wins() {
        let mut earlier_result = a_claude_result();
        earlier_result["usage"]["input_tokens"] = json!(CACHE_READ_TOKENS);
        let transcript = format!("{earlier_result}\n{}", a_claude_result());

        assert_eq!(
            parse_usage(&transcript),
            Some(claude_totals()),
            "final totals replace earlier results instead of accumulating them"
        );
    }

    #[test]
    fn a_codex_token_event_gives_its_tokens_without_a_cost() {
        assert_eq!(
            parse_usage(&a_codex_turn().to_string()),
            Some(Usage {
                cost_usd: None,
                ..claude_totals()
            }),
            "Codex reports total tokens, already including cache and reasoning, without cost"
        );
    }

    #[test]
    fn the_last_codex_turn_wins() {
        let mut earlier_turn = a_codex_turn();
        earlier_turn["usage"]["input_tokens"] = json!(CACHE_READ_TOKENS);
        let transcript = format!("{earlier_turn}\n{}", a_codex_turn());

        assert_eq!(
            parse_usage(&transcript),
            Some(Usage {
                cost_usd: None,
                ..claude_totals()
            }),
            "the last Codex totals replace any earlier cumulative counts"
        );
    }

    #[test]
    fn a_transcript_without_usage_gives_none() {
        let transcript = "starting agent\n{bad json\n{\"type\":\"turn.started\"}\nnull";

        assert_eq!(
            parse_usage(transcript),
            None,
            "unrelated events and diagnostic lines do not invent usage"
        );
    }

    #[test]
    fn non_json_lines_do_not_hide_a_result() {
        let transcript = format!(
            "starting agent\n{{bad json\n{}\nfinished",
            a_claude_result()
        );

        assert_eq!(
            parse_usage(&transcript),
            Some(claude_totals()),
            "diagnostics surrounding a valid result do not discard its totals"
        );
    }

    #[test]
    fn malformed_token_numbers_give_none() {
        for malformed_count in [json!(-1), json!(1.5), json!("120"), Value::Null] {
            for mut event in [a_claude_result(), a_codex_turn()] {
                event["usage"]["input_tokens"] = malformed_count.clone();
                assert_eq!(
                    parse_usage(&event.to_string()),
                    None,
                    "a token count must be an unsigned integer, not {malformed_count}"
                );
            }
        }
    }

    #[test]
    fn overflowing_cached_input_gives_none() {
        let mut result = a_claude_result();
        result["usage"]["cache_read_input_tokens"] = json!(u64::MAX);

        assert_eq!(
            parse_usage(&result.to_string()),
            None,
            "adding cached input must not overflow or panic"
        );
    }

    #[test]
    fn a_final_result_without_tokens_does_not_reuse_stale_usage() {
        let transcript = format!("{}\n{{\"type\":\"result\"}}", a_claude_result());

        assert_eq!(
            parse_usage(&transcript),
            None,
            "the last result remains authoritative even when its usage is absent"
        );
    }

    #[test]
    fn a_missing_or_invalid_cost_preserves_valid_tokens() {
        for unavailable_cost in [Value::Null, json!("unknown"), json!(-COST_USD)] {
            let mut result = a_claude_result();
            result["total_cost_usd"] = unavailable_cost;

            assert_eq!(
                parse_usage(&result.to_string()),
                Some(Usage {
                    cost_usd: None,
                    ..claude_totals()
                }),
                "unavailable cost does not discard otherwise valid token counts"
            );
        }
    }
}
