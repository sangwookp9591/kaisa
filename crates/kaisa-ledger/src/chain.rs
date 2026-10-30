//! The tamper-evident hash chain over recorded events (architecture §5.4).
//!
//! Each event's hash covers the previous hash and the event's canonical bytes, so editing,
//! removing or reordering any event changes every later hash. `verify_chain` reports the first
//! sequence at which the chain stops being consistent.

use std::fmt;

use kaisa_domain::{RecordedEvent, Sequence};

/// Domain separator for the hash that anchors an empty ledger.
const GENESIS_SEED: &[u8] = b"kaisa-ledger-genesis-v1";

/// A blake3 chain hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChainHash([u8; blake3::OUT_LEN]);

impl ChainHash {
    /// The hash that precedes the first event.
    #[must_use]
    pub fn genesis() -> Self {
        Self(*blake3::hash(GENESIS_SEED).as_bytes())
    }

    /// Wraps hash bytes read back from storage.
    #[must_use]
    pub fn from_bytes(bytes: [u8; blake3::OUT_LEN]) -> Self {
        Self(bytes)
    }

    /// The raw bytes, for storage.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; blake3::OUT_LEN] {
        &self.0
    }

    /// The hash of `event` linked after `self`.
    #[must_use]
    pub fn link(&self, event: &RecordedEvent) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.0);
        hasher.update(&event.canonical_bytes());
        Self(*hasher.finalize().as_bytes())
    }
}

impl fmt::Debug for ChainHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ChainHash({})",
            blake3::Hash::from_bytes(self.0).to_hex()
        )
    }
}

/// A recorded event together with its chain hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainedEvent {
    /// The event and its sequence.
    pub recorded: RecordedEvent,
    /// `previous.link(&recorded)`.
    pub hash: ChainHash,
}

/// Why a chain failed verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BreakReason {
    /// The sequence is not the one directly after the previous event.
    SequenceGap,
    /// The stored hash does not match the recomputed one.
    HashMismatch,
}

/// The first point at which a chain is inconsistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainBreak {
    /// The position where the chain stops verifying: the sequence the event at that position
    /// should have had. For a deleted event this is the deleted event's own position.
    pub at: Sequence,
    /// What was wrong with it.
    pub reason: BreakReason,
}

impl fmt::Display for ChainBreak {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ledger chain breaks at {}: {:?}", self.at, self.reason)
    }
}

impl std::error::Error for ChainBreak {}

/// Links `events` (already in sequence order) into a chain starting from genesis.
#[must_use]
pub fn build_chain(events: &[RecordedEvent]) -> Vec<ChainedEvent> {
    let mut previous: ChainHash = ChainHash::genesis();
    events
        .iter()
        .map(|recorded| {
            let hash: ChainHash = previous.link(recorded);
            previous = hash;
            ChainedEvent {
                recorded: recorded.clone(),
                hash,
            }
        })
        .collect()
}

/// Checks that `chain` starts at [`Sequence::FIRST`], has no gaps, and that every hash links.
///
/// # Errors
///
/// Returns the first [`ChainBreak`] found.
pub fn verify_chain(chain: &[ChainedEvent]) -> Result<(), ChainBreak> {
    let mut previous_hash: ChainHash = ChainHash::genesis();
    let mut expected_sequence: Sequence = Sequence::FIRST;
    for chained in chain {
        let at: Sequence = expected_sequence;
        if chained.recorded.sequence != at {
            return Err(ChainBreak {
                at,
                reason: BreakReason::SequenceGap,
            });
        }
        if previous_hash.link(&chained.recorded) != chained.hash {
            return Err(ChainBreak {
                at,
                reason: BreakReason::HashMismatch,
            });
        }
        previous_hash = chained.hash;
        expected_sequence = at.next();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{Event, EventKind, RecordedEvent, Sequence};

    use super::{BreakReason, ChainBreak, ChainedEvent, build_chain, verify_chain};

    const CURRENT_PAYLOAD_VERSION: u16 = 1;

    fn event(kind: EventKind, subject: &str, payload: &str) -> Event {
        Event {
            kind,
            subject: subject.to_owned(),
            payload_version: CURRENT_PAYLOAD_VERSION,
            payload: payload.to_owned(),
        }
    }

    /// The ledger of a run that fixed the login test: started, admitted, ran, exited.
    fn fix_login_run_history() -> Vec<RecordedEvent> {
        let story: Vec<Event> = vec![
            event(
                EventKind::RunStarted,
                "run_41",
                r#"{"goal":"fix login test"}"#,
            ),
            event(
                EventKind::AttemptAdmitted,
                "att_41a",
                r#"{"task":"task_41"}"#,
            ),
            event(
                EventKind::AttemptStateChanged,
                "att_41a",
                r#"{"execution":"running"}"#,
            ),
            event(
                EventKind::AttemptStateChanged,
                "att_41a",
                r#"{"execution":"exited"}"#,
            ),
        ];
        let mut sequence: Sequence = Sequence::FIRST;
        story
            .into_iter()
            .map(|event| {
                let recorded = RecordedEvent { sequence, event };
                sequence = sequence.next();
                recorded
            })
            .collect()
    }

    /// The position of the "attempt admitted" event in [`fix_login_run_history`].
    fn admitted_event_index() -> usize {
        fix_login_run_history()
            .iter()
            .position(|recorded| recorded.event.kind == EventKind::AttemptAdmitted)
            .expect("the history contains an admission")
    }

    #[test]
    fn a_chain_built_from_recorded_events_verifies_cleanly() {
        let chain: Vec<ChainedEvent> = build_chain(&fix_login_run_history());

        assert_eq!(
            verify_chain(&chain),
            Ok(()),
            "an untouched chain must verify"
        );
    }

    #[test]
    fn an_empty_chain_verifies_cleanly() {
        assert_eq!(
            verify_chain(&[]),
            Ok(()),
            "a fresh ledger has nothing to contradict"
        );
    }

    #[test]
    fn editing_a_recorded_event_breaks_the_chain_at_that_event() {
        let mut chain: Vec<ChainedEvent> = build_chain(&fix_login_run_history());
        let tampered = &mut chain[admitted_event_index()];
        tampered.recorded.event.subject = "att_forged".to_owned();
        let tampered_at: Sequence = tampered.recorded.sequence;

        assert_eq!(
            verify_chain(&chain),
            Err(ChainBreak {
                at: tampered_at,
                reason: BreakReason::HashMismatch
            }),
            "an edit must be reported exactly where it happened"
        );
    }

    #[test]
    fn removing_an_event_from_the_middle_is_reported_as_a_gap_at_its_position() {
        let mut chain: Vec<ChainedEvent> = build_chain(&fix_login_run_history());
        let removed: ChainedEvent = chain.remove(admitted_event_index());

        assert_eq!(
            verify_chain(&chain),
            Err(ChainBreak {
                at: removed.recorded.sequence,
                reason: BreakReason::SequenceGap
            }),
            "a deleted event is reported at the position where it is missing"
        );
    }

    #[test]
    fn swapping_two_events_breaks_the_chain_at_the_earlier_position() {
        let mut chain: Vec<ChainedEvent> = build_chain(&fix_login_run_history());
        let first_swapped: usize = admitted_event_index();
        let earlier_sequence: Sequence = chain[first_swapped].recorded.sequence;
        chain.swap(first_swapped, first_swapped + 1);

        assert_eq!(
            verify_chain(&chain).map_err(|broken| broken.at),
            Err(earlier_sequence),
            "reordering history must be detected where the order first changed"
        );
    }
}
