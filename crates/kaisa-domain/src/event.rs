//! Ledger events: what happened, to which subject, in which order.
//!
//! The ledger stores events in order and hash-chains them (architecture §5.4). The chain hashes
//! the canonical bytes defined here, so the encoding must be deterministic and must never
//! depend on Rust field order or enum discriminants.

use std::fmt;

/// Tag that starts every canonical encoding. A new encoding gets a new tag.
const CANONICAL_TAG: &[u8] = b"kaisa-event-v1";

/// Position of an event in the ledger. Sequences start at [`Sequence::FIRST`] and have no gaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sequence(u64);

impl Sequence {
    /// The sequence of the first event ever recorded.
    pub const FIRST: Self = Self(1);

    /// Wraps a stored sequence number. Returns `None` for zero, which is never a valid position.
    #[must_use]
    pub fn from_stored(value: u64) -> Option<Self> {
        (value >= Self::FIRST.0).then_some(Self(value))
    }

    /// The sequence that directly follows this one.
    ///
    /// # Panics
    ///
    /// Panics if the ledger has run out of 64-bit sequence numbers, which is an invariant
    /// violation rather than a recoverable error.
    #[must_use]
    pub fn next(self) -> Self {
        Self(
            self.0
                .checked_add(1)
                .expect("ledger sequence space exhausted"),
        )
    }

    /// The raw number, for storage.
    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Sequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// What kind of thing happened. New kinds may be added; the stable code never changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EventKind {
    /// A provisional specification version was drafted.
    SpecDrafted,
    /// A specification version received approval.
    SpecApproved,
    /// Approved specification bytes were frozen.
    SpecFrozen,
    /// A mission was created from what the user asked for.
    MissionCreated,
    /// A run started inside a mission: the root run, or a sub-run whose parent is named in the
    /// payload. One kind for both, so there is one way to say "a run began".
    RunStarted,
    /// A writing task declared the paths it may change.
    WriteSetDeclared,
    /// The router chose an agent, model and effort for a task, with its reasons.
    RouteDecided,
    /// A context pack was assembled for an attempt; the payload names its digest.
    ContextPackRecorded,
    /// Verification payloads carry `run_id`, `mission_id` and repository identity.
    /// A developer result was frozen as a commit and tree.
    CandidateFrozen,
    /// A trusted check produced evidence on a frozen tree.
    CheckEvidence,
    /// A reviewer returned a verdict (or no valid verdict) on a frozen tree.
    ReviewVerdict,
    /// The delivery gate decided what happens next.
    GateDecided,
    /// A bounded revision began from a frozen candidate.
    RevisionStarted,
    /// A knowledge item was proposed (not yet trusted).
    KnowledgeProposed,
    /// A knowledge item became trusted, with the evidence that promoted it.
    KnowledgePromoted,
    /// A knowledge item was given to an attempt; usefulness is scored against its outcome.
    KnowledgeInjected,
    /// An attempt was admitted for a task.
    AttemptAdmitted,
    /// One of an attempt's state axes changed.
    AttemptStateChanged,
    /// An external effect was announced through the outbox.
    IntentRecorded,
    /// An outbox effect finished and produced a receipt.
    IntentCompleted,
    /// A planned task was handed to an attempt (the subject is the task).
    TaskStarted,
    /// A planned task is done; tasks waiting on it may start (the subject is the task).
    TaskFinished,
    /// The tokens (and cost, when the agent reports one) an attempt used (T30b).
    UsageRecorded,
    /// A run adopted a policy snapshot; the subject is the digest of its canonical text (T32b).
    PolicyAdopted,
}

impl EventKind {
    /// Every event kind, for exhaustive lookups.
    pub const ALL: [Self; 24] = [
        Self::SpecDrafted,
        Self::SpecApproved,
        Self::SpecFrozen,
        Self::MissionCreated,
        Self::RunStarted,
        Self::WriteSetDeclared,
        Self::RouteDecided,
        Self::ContextPackRecorded,
        Self::CandidateFrozen,
        Self::CheckEvidence,
        Self::ReviewVerdict,
        Self::GateDecided,
        Self::RevisionStarted,
        Self::KnowledgeProposed,
        Self::KnowledgePromoted,
        Self::KnowledgeInjected,
        Self::AttemptAdmitted,
        Self::AttemptStateChanged,
        Self::IntentRecorded,
        Self::IntentCompleted,
        Self::TaskStarted,
        Self::TaskFinished,
        Self::UsageRecorded,
        Self::PolicyAdopted,
    ];

    /// Stable textual code used in storage and in the canonical encoding.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::SpecDrafted => "spec.drafted",
            Self::SpecApproved => "spec.approved",
            Self::SpecFrozen => "spec.frozen",
            Self::MissionCreated => "mission.created",
            Self::RunStarted => "run.started",
            Self::WriteSetDeclared => "task.write_set_declared",
            Self::RouteDecided => "route.decided",
            Self::ContextPackRecorded => "context_pack.recorded",
            Self::CandidateFrozen => "candidate.frozen",
            Self::CheckEvidence => "check.evidence",
            Self::ReviewVerdict => "review.verdict",
            Self::GateDecided => "gate.decided",
            Self::RevisionStarted => "revision.started",
            Self::KnowledgeProposed => "knowledge.proposed",
            Self::KnowledgePromoted => "knowledge.promoted",
            Self::KnowledgeInjected => "knowledge.injected",
            Self::AttemptAdmitted => "attempt.admitted",
            Self::AttemptStateChanged => "attempt.state_changed",
            Self::IntentRecorded => "outbox.intent_recorded",
            Self::IntentCompleted => "outbox.intent_completed",
            Self::TaskStarted => "task.started",
            Self::TaskFinished => "task.finished",
            Self::UsageRecorded => "attempt.usage_recorded",
            Self::PolicyAdopted => "run.policy_adopted",
        }
    }

    /// Looks a kind up by its stable code.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.code() == code)
    }
}

/// An event before the ledger has given it a sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// What happened.
    pub kind: EventKind,
    /// The identifier of the thing it happened to, such as a run or attempt id.
    pub subject: String,
    /// Version of the payload schema, so old events can be upcast when read.
    pub payload_version: u16,
    /// The payload, as canonical JSON text produced by the caller.
    pub payload: String,
}

/// An event together with the position the ledger assigned to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedEvent {
    /// Position in the ledger.
    pub sequence: Sequence,
    /// The event itself.
    pub event: Event,
}

impl RecordedEvent {
    /// Deterministic bytes that the hash chain covers.
    ///
    /// Layout: tag, then sequence (u64 BE), kind code, subject, payload version (u16 BE) and
    /// payload. Every variable-length field is prefixed with its length (u64 BE), so no two
    /// different events can produce the same bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        push_field(&mut bytes, CANONICAL_TAG);
        bytes.extend_from_slice(&self.sequence.value().to_be_bytes());
        push_field(&mut bytes, self.event.kind.code().as_bytes());
        push_field(&mut bytes, self.event.subject.as_bytes());
        bytes.extend_from_slice(&self.event.payload_version.to_be_bytes());
        push_field(&mut bytes, self.event.payload.as_bytes());
        bytes
    }
}

/// Appends `field` with a big-endian u64 length prefix.
fn push_field(bytes: &mut Vec<u8>, field: &[u8]) {
    let length: u64 = u64::try_from(field.len()).expect("a field cannot exceed u64::MAX bytes");
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(field);
}

#[cfg(test)]
mod tests {
    use super::{Event, EventKind, RecordedEvent, Sequence};

    const CURRENT_PAYLOAD_VERSION: u16 = 1;

    fn run_started(run: &str, goal: &str) -> Event {
        Event {
            kind: EventKind::RunStarted,
            subject: run.to_owned(),
            payload_version: CURRENT_PAYLOAD_VERSION,
            payload: format!("{{\"goal\":\"{goal}\"}}"),
        }
    }

    fn recorded_first(event: Event) -> RecordedEvent {
        RecordedEvent {
            sequence: Sequence::FIRST,
            event,
        }
    }

    #[test]
    fn the_first_sequence_is_followed_by_the_second_sequence() {
        let second: Sequence = Sequence::FIRST.next();

        assert!(second > Sequence::FIRST, "sequences only move forward");
        assert_eq!(
            Sequence::from_stored(Sequence::FIRST.value() + 1),
            Some(second),
            "next() leaves no gap after the first sequence"
        );
    }

    #[test]
    fn a_stored_sequence_of_zero_is_not_a_valid_position() {
        let before_first: u64 = Sequence::FIRST.value() - 1;

        assert_eq!(
            Sequence::from_stored(before_first),
            None,
            "nothing can be recorded before the first sequence"
        );
    }

    #[test]
    fn every_event_kind_is_found_again_by_its_stable_code() {
        for kind in EventKind::ALL {
            assert_eq!(
                EventKind::from_code(kind.code()),
                Some(kind),
                "the stored code must map back to the same kind"
            );
        }
    }

    #[test]
    fn no_two_event_kinds_share_a_code() {
        let codes: std::collections::BTreeSet<&str> =
            EventKind::ALL.into_iter().map(EventKind::code).collect();

        assert_eq!(
            codes.len(),
            EventKind::ALL.len(),
            "a shared code would replay one kind of event as another"
        );
    }

    #[test]
    fn the_canonical_bytes_of_an_event_are_the_same_on_every_call() {
        let fix_login: RecordedEvent = recorded_first(run_started("run_41", "fix login test"));

        assert_eq!(
            fix_login.canonical_bytes(),
            fix_login.canonical_bytes(),
            "the hash chain needs the same bytes for the same event"
        );
    }

    #[test]
    fn events_that_differ_only_in_subject_have_different_canonical_bytes() {
        let run_41: RecordedEvent = recorded_first(run_started("run_41", "fix login test"));
        let run_42: RecordedEvent = recorded_first(run_started("run_42", "fix login test"));

        assert_ne!(
            run_41.canonical_bytes(),
            run_42.canonical_bytes(),
            "changing which run an event is about must change its hash"
        );
    }

    #[test]
    fn moving_text_between_subject_and_payload_changes_the_canonical_bytes() {
        let mut shifted: Event = run_started("run_4", "fix login test");
        shifted.payload = format!("1{}", shifted.payload);
        let original: RecordedEvent = recorded_first(run_started("run_41", "fix login test"));

        assert_ne!(
            original.canonical_bytes(),
            recorded_first(shifted).canonical_bytes(),
            "length prefixes keep field boundaries from being forged"
        );
    }
}
