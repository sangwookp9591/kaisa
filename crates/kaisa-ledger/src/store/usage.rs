//! What attempts used (T30b): tokens and, when the agent reports it, cost.

use kaisa_domain::{AttemptId, Event, EventKind, Sequence};

use super::error::StoreError;
use super::event_store::EventStore;

/// Payload schema version of `attempt.usage_recorded` events.
const USAGE_PAYLOAD_VERSION: u16 = 1;

/// One attempt's recorded usage.
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptUsage {
    /// Input tokens, cached input included.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Cost in US dollars, if the agent reported one.
    pub cost_usd: Option<f64>,
}

impl EventStore {
    /// Records what `attempt` used.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the event cannot be appended.
    pub fn record_usage(
        &mut self,
        attempt: &AttemptId,
        usage: &AttemptUsage,
    ) -> Result<(), StoreError> {
        self.append(Event {
            kind: EventKind::UsageRecorded,
            subject: attempt.as_str().to_owned(),
            payload_version: USAGE_PAYLOAD_VERSION,
            payload: serde_json::json!({
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cost_usd": usage.cost_usd,
            })
            .to_string(),
        })?;
        Ok(())
    }

    /// The last usage recorded for `attempt`, if any.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the log cannot be read or a usage payload does not decode.
    pub fn usage_for(&self, attempt: &AttemptId) -> Result<Option<AttemptUsage>, StoreError> {
        let mut last: Option<AttemptUsage> = None;
        for chained in self.read_from(Sequence::FIRST)? {
            let recorded = chained.recorded;
            if recorded.event.kind != EventKind::UsageRecorded
                || recorded.event.subject != attempt.as_str()
            {
                continue;
            }
            let corrupt = || StoreError::CorruptPayload {
                stored_sequence: super::rows::stored_sequence(recorded.sequence),
            };
            let payload: serde_json::Value =
                serde_json::from_str(&recorded.event.payload).map_err(|_| corrupt())?;
            last = Some(AttemptUsage {
                input_tokens: payload["input_tokens"].as_u64().ok_or_else(corrupt)?,
                output_tokens: payload["output_tokens"].as_u64().ok_or_else(corrupt)?,
                cost_usd: payload["cost_usd"].as_f64(),
            });
        }
        Ok(last)
    }
}
