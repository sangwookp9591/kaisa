//! The policy each run adopted (T32b), kept as the snapshot's canonical text.

use kaisa_domain::{Event, EventKind, PolicySnapshot, Sequence};

use super::error::StoreError;
use super::event_store::EventStore;

/// Payload schema version of `run.policy_adopted` events.
const POLICY_PAYLOAD_VERSION: u16 = 1;

/// A policy a run adopted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedPolicy {
    /// The blake3 digest of the canonical text, in hex.
    pub digest: String,
    /// The snapshot's canonical text.
    pub canonical: String,
}

impl EventStore {
    /// Records that a run adopted `snapshot`, and returns the digest that names it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the event cannot be appended.
    pub fn record_policy(&mut self, snapshot: &PolicySnapshot) -> Result<String, StoreError> {
        let digest: String = blake3::hash(snapshot.canonical().as_bytes())
            .to_hex()
            .to_string();
        self.append(Event {
            kind: EventKind::PolicyAdopted,
            subject: digest.clone(),
            payload_version: POLICY_PAYLOAD_VERSION,
            payload: serde_json::json!({"canonical": snapshot.canonical()}).to_string(),
        })?;
        Ok(digest)
    }

    /// Every adopted policy, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the log cannot be read or a policy payload does not decode.
    pub fn adopted_policies(&self) -> Result<Vec<AdoptedPolicy>, StoreError> {
        let mut adopted: Vec<AdoptedPolicy> = Vec::new();
        for chained in self.read_from(Sequence::FIRST)? {
            let recorded = chained.recorded;
            if recorded.event.kind != EventKind::PolicyAdopted {
                continue;
            }
            let corrupt = || StoreError::CorruptPayload {
                stored_sequence: super::rows::stored_sequence(recorded.sequence),
            };
            let payload: serde_json::Value =
                serde_json::from_str(&recorded.event.payload).map_err(|_| corrupt())?;
            adopted.push(AdoptedPolicy {
                digest: recorded.event.subject.clone(),
                canonical: payload["canonical"]
                    .as_str()
                    .ok_or_else(corrupt)?
                    .to_owned(),
            });
        }
        Ok(adopted)
    }
}
