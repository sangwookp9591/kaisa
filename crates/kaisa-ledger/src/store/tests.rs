use kaisa_domain::{EventKind, Sequence};

use super::test_support::{
    ScratchLedger, a_ledger_path_that_does_not_exist_yet, an_attacker_deletes,
    an_attacker_invents_an_event_kind_for, an_attacker_rewrites_the_subject_of,
    an_attacker_truncates_the_hash_of, attempt_41a_admitted, attempt_41a_running, run_41_started,
};
use super::{CorruptReason, EventStore, StoreError};
use crate::chain::{BreakReason, ChainBreak, ChainHash, ChainedEvent};

/// Each call to `append` records exactly this many events.
const EVENTS_PER_APPEND: u64 = 1;

fn an_open_empty_ledger(scratch: &ScratchLedger) -> EventStore {
    EventStore::open(&scratch.path()).expect("a new ledger opens")
}

/// A ledger holding run 41's history: started, admitted, running.
fn a_ledger_with_run_41_history(scratch: &ScratchLedger) -> (EventStore, Vec<ChainedEvent>) {
    let mut store: EventStore = an_open_empty_ledger(scratch);
    let history: Vec<ChainedEvent> = [
        run_41_started(),
        attempt_41a_admitted(),
        attempt_41a_running(),
    ]
    .into_iter()
    .map(|event| store.append(event).expect("append succeeds"))
    .collect();
    (store, history)
}

/// The journal mode that makes committed events crash-safe without blocking readers.
const WRITE_AHEAD_LOGGING: &str = "wal";

#[test]
fn opening_a_new_ledger_file_creates_an_empty_ledger() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();

    let store: EventStore = EventStore::open(&scratch.path()).expect("a new ledger opens");

    assert!(
        scratch.path().exists(),
        "opening must create the database file"
    );
    assert!(
        store.is_empty().expect("the count is readable"),
        "a new ledger has no events"
    );
}

#[test]
fn opening_an_existing_ledger_keeps_its_events_table() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    drop(EventStore::open(&scratch.path()).expect("the first open creates the ledger"));

    let reopened: EventStore =
        EventStore::open(&scratch.path()).expect("an existing ledger reopens");

    assert!(
        reopened
            .is_empty()
            .expect("the events table is still there"),
        "reopening must not fail or recreate a different table"
    );
}

#[test]
fn a_newly_opened_ledger_uses_write_ahead_logging() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();

    let store: EventStore = EventStore::open(&scratch.path()).expect("a new ledger opens");

    assert_eq!(
        store.journal_mode().expect("the journal mode is readable"),
        WRITE_AHEAD_LOGGING,
        "WAL keeps committed events durable while readers keep reading"
    );
}

#[test]
fn the_first_appended_event_gets_the_first_sequence() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);

    let chained: ChainedEvent = store.append(run_41_started()).expect("append succeeds");

    assert_eq!(
        chained.recorded.sequence,
        Sequence::FIRST,
        "history starts at the first sequence"
    );
}

#[test]
fn each_appended_event_gets_the_sequence_after_the_previous_one() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let started: ChainedEvent = store.append(run_41_started()).expect("append succeeds");

    let admitted: ChainedEvent = store
        .append(attempt_41a_admitted())
        .expect("append succeeds");

    assert_eq!(
        admitted.recorded.sequence,
        started.recorded.sequence.next(),
        "sequences have no gaps"
    );
}

#[test]
fn an_appended_event_is_linked_to_the_previous_hash() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let admitted: ChainedEvent = {
        store.append(run_41_started()).expect("append succeeds");
        store
            .append(attempt_41a_admitted())
            .expect("append succeeds")
    };

    let running: ChainedEvent = store
        .append(attempt_41a_running())
        .expect("append succeeds");

    assert_eq!(
        running.hash,
        admitted.hash.link(&running.recorded),
        "each hash covers the previous one, which makes the log tamper-evident"
    );
}

#[test]
fn the_first_appended_event_is_linked_to_genesis() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);

    let started: ChainedEvent = store.append(run_41_started()).expect("append succeeds");

    assert_eq!(
        started.hash,
        ChainHash::genesis().link(&started.recorded),
        "the chain is anchored at the genesis hash"
    );
}

#[test]
fn appending_an_event_increases_the_ledger_length_by_one() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let before: u64 = store.len().expect("the count is readable");

    store.append(run_41_started()).expect("append succeeds");

    assert_eq!(
        store.len().expect("the count is readable"),
        before + EVENTS_PER_APPEND,
        "one append records exactly one event"
    );
}

#[test]
fn reading_from_the_first_sequence_returns_every_event_in_order() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let (store, history) = a_ledger_with_run_41_history(&scratch);

    let read: Vec<ChainedEvent> = store.read_from(Sequence::FIRST).expect("read succeeds");

    assert_eq!(
        read, history,
        "reading from the start returns the whole history unchanged"
    );
}

#[test]
fn reading_from_a_later_sequence_skips_earlier_events() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let (store, history) = a_ledger_with_run_41_history(&scratch);
    let admission: usize = history
        .iter()
        .position(|chained| chained.recorded.event.kind == EventKind::AttemptAdmitted)
        .expect("run 41's history contains an admission");

    let read: Vec<ChainedEvent> = store
        .read_from(history[admission].recorded.sequence)
        .expect("read succeeds");

    assert_eq!(
        read,
        history[admission..],
        "events before the requested sequence are left out"
    );
}

#[test]
fn reading_past_the_last_event_returns_nothing() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let (store, history) = a_ledger_with_run_41_history(&scratch);
    let after_last: Sequence = history
        .last()
        .expect("the history is not empty")
        .recorded
        .sequence
        .next();

    let read: Vec<ChainedEvent> = store.read_from(after_last).expect("read succeeds");

    assert!(
        read.is_empty(),
        "nothing has been recorded after the last event"
    );
}

#[test]
fn events_survive_closing_and_reopening_the_store() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let (store, history) = a_ledger_with_run_41_history(&scratch);
    drop(store);

    let reopened: EventStore = EventStore::open(&scratch.path()).expect("the ledger reopens");

    assert_eq!(
        reopened.read_from(Sequence::FIRST).expect("read succeeds"),
        history,
        "committed events are durable across a restart"
    );
}

#[test]
fn appending_after_reopening_continues_the_same_chain() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let (store, history) = a_ledger_with_run_41_history(&scratch);
    let last_before_restart: ChainedEvent = history.last().expect("history is not empty").clone();
    drop(store);
    let mut reopened: EventStore = EventStore::open(&scratch.path()).expect("the ledger reopens");

    let after_restart: ChainedEvent = reopened.append(run_41_started()).expect("append succeeds");

    assert_eq!(
        after_restart.recorded.sequence,
        last_before_restart.recorded.sequence.next(),
        "a restart must not reset or skip sequences"
    );
    assert_eq!(
        after_restart.hash,
        last_before_restart.hash.link(&after_restart.recorded),
        "the chain continues from the last hash written before the restart"
    );
}

/// Where in run 41's history the attempt was admitted.
fn admission_of(history: &[ChainedEvent]) -> Sequence {
    history
        .iter()
        .find(|chained| chained.recorded.event.kind == EventKind::AttemptAdmitted)
        .expect("run 41's history contains an admission")
        .recorded
        .sequence
}

#[test]
fn an_untouched_store_verifies_cleanly() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let (store, _history) = a_ledger_with_run_41_history(&scratch);

    assert!(
        store.verify().is_ok(),
        "a ledger written only by Kaisa must verify"
    );
}

#[test]
fn a_row_edited_on_disk_fails_verification_at_that_event() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let (store, history) = a_ledger_with_run_41_history(&scratch);
    let edited: Sequence = admission_of(&history);

    an_attacker_rewrites_the_subject_of(&scratch.path(), edited);

    assert!(
        matches!(
            store.verify(),
            Err(StoreError::ChainBroken(ChainBreak { at, reason: BreakReason::HashMismatch }))
                if at == edited
        ),
        "an edit made outside Kaisa must be reported exactly where it happened"
    );
}

#[test]
fn a_row_deleted_on_disk_fails_verification_at_its_position() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let (store, history) = a_ledger_with_run_41_history(&scratch);
    let deleted: Sequence = admission_of(&history);

    an_attacker_deletes(&scratch.path(), deleted);

    assert!(
        matches!(
            store.verify(),
            Err(StoreError::ChainBroken(ChainBreak { at, reason: BreakReason::SequenceGap }))
                if at == deleted
        ),
        "a deletion made outside Kaisa must be reported where the event is missing"
    );
}

/// The stored sequence number of `sequence`, as it appears in a corruption report.
fn as_stored(sequence: Sequence) -> i64 {
    i64::try_from(sequence.value()).expect("test sequences are small")
}

#[test]
fn a_row_with_an_unknown_event_kind_is_reported_as_corrupt() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let (store, history) = a_ledger_with_run_41_history(&scratch);
    let invented: Sequence = admission_of(&history);

    an_attacker_invents_an_event_kind_for(&scratch.path(), invented);

    assert!(
        matches!(
            store.read_from(Sequence::FIRST),
            Err(StoreError::Corrupt { stored_sequence, reason: CorruptReason::UnknownEventKind })
                if stored_sequence == as_stored(invented)
        ),
        "an event Kaisa cannot interpret must be reported, never guessed"
    );
}

#[test]
fn a_row_with_a_wrong_length_hash_is_reported_as_corrupt() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let (store, history) = a_ledger_with_run_41_history(&scratch);
    let truncated: Sequence = admission_of(&history);

    an_attacker_truncates_the_hash_of(&scratch.path(), truncated);

    assert!(
        matches!(
            store.read_from(Sequence::FIRST),
            Err(StoreError::Corrupt { stored_sequence, reason: CorruptReason::WrongHashLength })
                if stored_sequence == as_stored(truncated)
        ),
        "a hash that cannot be a chain hash must be reported as corruption"
    );
}

#[test]
fn appending_after_the_last_hash_was_truncated_is_refused() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let (mut store, history) = a_ledger_with_run_41_history(&scratch);
    let last: Sequence = history
        .last()
        .expect("history is not empty")
        .recorded
        .sequence;
    an_attacker_truncates_the_hash_of(&scratch.path(), last);

    assert!(
        matches!(
            store.append(run_41_started()),
            Err(StoreError::Corrupt {
                reason: CorruptReason::WrongHashLength,
                ..
            })
        ),
        "new events must never be chained onto a hash that cannot be trusted"
    );
}
