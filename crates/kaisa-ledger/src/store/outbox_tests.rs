use kaisa_domain::{EventKind, IntentId, Sequence};

use super::test_support::{
    ScratchLedger, a_ledger_path_that_does_not_exist_yet,
    intent_to_create_the_worktree_of_attempt_41a, intent_to_spawn_the_agent_of_attempt_41a,
};
use super::{Completion, EventStore, Intent, PendingIntent, StoreError};
use crate::chain::ChainedEvent;

/// The receipt a worktree creation produces: where the worktree ended up.
fn worktree_created_receipt() -> &'static str {
    "worktree at ~/.kaisa/worktrees/att_41a on branch kaisa/41-a"
}

/// A different receipt for the same worktree creation, as a buggy or forged retry might send.
fn worktree_created_somewhere_else_receipt() -> &'static str {
    "worktree at /tmp/elsewhere on branch kaisa/41-a"
}

/// The id of an intent that was never recorded.
fn an_intent_nobody_recorded() -> IntentId {
    IntentId::try_new("intent_ghost").expect("a valid intent id")
}

fn an_open_empty_ledger(scratch: &ScratchLedger) -> EventStore {
    EventStore::open(&scratch.path()).expect("a new ledger opens")
}

fn intents_of(pending: &[PendingIntent]) -> Vec<Intent> {
    pending
        .iter()
        .map(|pending| pending.intent.clone())
        .collect()
}

#[test]
fn recording_an_intent_also_records_its_event() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let create_worktree: Intent = intent_to_create_the_worktree_of_attempt_41a();

    let announced: ChainedEvent = store
        .record_intent(&create_worktree)
        .expect("recording succeeds");

    let history: Vec<ChainedEvent> = store.read_from(Sequence::FIRST).expect("read succeeds");
    assert_eq!(
        history,
        vec![announced.clone()],
        "the intent's event is in the ledger"
    );
    assert_eq!(
        announced.recorded.event.kind,
        EventKind::IntentRecorded,
        "it is an intent event"
    );
    assert_eq!(
        announced.recorded.event.subject,
        create_worktree.id.as_str(),
        "the event is about the intent it announces"
    );
}

#[test]
fn a_recorded_intent_is_pending() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let create_worktree: Intent = intent_to_create_the_worktree_of_attempt_41a();

    store
        .record_intent(&create_worktree)
        .expect("recording succeeds");

    assert_eq!(
        intents_of(&store.pending_intents().expect("the outbox is readable")),
        vec![create_worktree],
        "an announced effect stays pending until it is completed"
    );
}

#[test]
fn pending_intents_are_listed_in_the_order_they_were_recorded() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let create_worktree: Intent = intent_to_create_the_worktree_of_attempt_41a();
    let spawn_agent: Intent = intent_to_spawn_the_agent_of_attempt_41a();

    store
        .record_intent(&create_worktree)
        .expect("recording succeeds");
    store
        .record_intent(&spawn_agent)
        .expect("recording succeeds");

    assert_eq!(
        intents_of(&store.pending_intents().expect("the outbox is readable")),
        vec![create_worktree, spawn_agent],
        "recovery must replay effects in the order they were announced"
    );
}

#[test]
fn recording_an_intent_whose_id_is_already_used_is_rejected() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let create_worktree: Intent = intent_to_create_the_worktree_of_attempt_41a();
    store
        .record_intent(&create_worktree)
        .expect("the first recording succeeds");

    assert!(
        matches!(
            store.record_intent(&create_worktree),
            Err(StoreError::DuplicateIntent(id)) if id == create_worktree.id
        ),
        "the same effect must never be announced twice"
    );
}

#[test]
fn a_rejected_duplicate_intent_leaves_no_extra_event_behind() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let create_worktree: Intent = intent_to_create_the_worktree_of_attempt_41a();
    store
        .record_intent(&create_worktree)
        .expect("the first recording succeeds");
    let events_before: u64 = store.len().expect("the count is readable");

    let _rejected = store.record_intent(&create_worktree);

    assert_eq!(
        store.len().expect("the count is readable"),
        events_before,
        "the intent row and its event commit together or not at all"
    );
}

#[test]
fn a_completed_intent_is_no_longer_pending() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let create_worktree: Intent = intent_to_create_the_worktree_of_attempt_41a();
    store
        .record_intent(&create_worktree)
        .expect("recording succeeds");

    store
        .complete(&create_worktree.id, worktree_created_receipt())
        .expect("completion succeeds");

    assert!(
        store
            .pending_intents()
            .expect("the outbox is readable")
            .is_empty(),
        "a completed effect must not be replayed on recovery"
    );
}

#[test]
fn completing_an_intent_records_an_intent_completed_event() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let create_worktree: Intent = intent_to_create_the_worktree_of_attempt_41a();
    store
        .record_intent(&create_worktree)
        .expect("recording succeeds");

    let completion: Completion = store
        .complete(&create_worktree.id, worktree_created_receipt())
        .expect("completion succeeds");

    let Completion::Recorded(completed) = completion else {
        panic!("a pending intent's first completion must record an event, got {completion:?}");
    };

    assert_eq!(
        completed.recorded.event.kind,
        EventKind::IntentCompleted,
        "it is a completion event"
    );
    assert_eq!(
        completed.recorded.event.subject,
        create_worktree.id.as_str(),
        "the completion is about the intent it closes"
    );
}

#[test]
fn completing_one_intent_leaves_the_other_intents_pending() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let create_worktree: Intent = intent_to_create_the_worktree_of_attempt_41a();
    let spawn_agent: Intent = intent_to_spawn_the_agent_of_attempt_41a();
    store
        .record_intent(&create_worktree)
        .expect("recording succeeds");
    store
        .record_intent(&spawn_agent)
        .expect("recording succeeds");

    store
        .complete(&create_worktree.id, worktree_created_receipt())
        .expect("completion succeeds");

    assert_eq!(
        intents_of(&store.pending_intents().expect("the outbox is readable")),
        vec![spawn_agent],
        "completion closes exactly the intent it names"
    );
}

#[test]
fn completing_an_intent_twice_with_the_same_receipt_is_accepted() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let create_worktree: Intent = intent_to_create_the_worktree_of_attempt_41a();
    store
        .record_intent(&create_worktree)
        .expect("recording succeeds");
    store
        .complete(&create_worktree.id, worktree_created_receipt())
        .expect("the first completion succeeds");

    let retried: Result<Completion, StoreError> =
        store.complete(&create_worktree.id, worktree_created_receipt());

    assert!(
        matches!(retried, Ok(Completion::AlreadyRecorded)),
        "a retry after a crash must be safe, got {retried:?}"
    );
}

#[test]
fn completing_an_intent_twice_with_the_same_receipt_records_no_second_event() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let create_worktree: Intent = intent_to_create_the_worktree_of_attempt_41a();
    store
        .record_intent(&create_worktree)
        .expect("recording succeeds");
    store
        .complete(&create_worktree.id, worktree_created_receipt())
        .expect("the first completion succeeds");
    let events_before_retry: u64 = store.len().expect("the count is readable");

    store
        .complete(&create_worktree.id, worktree_created_receipt())
        .expect("the retry is accepted");

    assert_eq!(
        store.len().expect("the count is readable"),
        events_before_retry,
        "an idempotent retry must not add history"
    );
}

#[test]
fn completing_an_intent_twice_with_a_different_receipt_is_rejected() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let create_worktree: Intent = intent_to_create_the_worktree_of_attempt_41a();
    store
        .record_intent(&create_worktree)
        .expect("recording succeeds");
    store
        .complete(&create_worktree.id, worktree_created_receipt())
        .expect("the first completion succeeds");

    assert!(
        matches!(
            store.complete(&create_worktree.id, worktree_created_somewhere_else_receipt()),
            Err(StoreError::ReceiptConflict(id)) if id == create_worktree.id
        ),
        "two different results for one effect means something is wrong and must be surfaced"
    );
}

#[test]
fn completing_an_unknown_intent_is_rejected() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_empty_ledger(&scratch);
    let ghost: IntentId = an_intent_nobody_recorded();

    assert!(
        matches!(
            store.complete(&ghost, worktree_created_receipt()),
            Err(StoreError::UnknownIntent(id)) if id == ghost
        ),
        "an effect that was never announced cannot be completed"
    );
}
