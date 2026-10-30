use kaisa_domain::{
    AttemptId, Cleanup, ContentDigest, Event, EventKind, Execution, ExitStatus, TaskId,
};

use super::test_support::{ScratchLedger, a_ledger_path_that_does_not_exist_yet};
use super::{AttemptRow, EventStore};

/// Payload schema version of the attempt events in these tests.
const PAYLOAD_VERSION: u16 = 1;
/// The bytes of the pack attempt 41a starts from.
const LOGIN_FIX_PACK: &[u8] = b"kaisa-context-pack v1\nspec: make the login test wait";

fn attempt_41a() -> AttemptId {
    AttemptId::try_new("att_41a").expect("a valid attempt id")
}

fn task_41() -> TaskId {
    TaskId::try_new("task_41").expect("a valid task id")
}

fn attempt_event(kind: EventKind, payload: String) -> Event {
    Event {
        kind,
        subject: attempt_41a().as_str().to_owned(),
        payload_version: PAYLOAD_VERSION,
        payload,
    }
}

fn the_scheduler_admits_attempt_41a(store: &mut EventStore) {
    store
        .append(attempt_event(
            EventKind::AttemptAdmitted,
            format!(r#"{{"task":"{}"}}"#, task_41()),
        ))
        .expect("admitting succeeds");
}

fn attempt_41a_moves_to(store: &mut EventStore, execution: Execution) {
    store
        .append(attempt_event(
            EventKind::AttemptStateChanged,
            format!(r#"{{"execution":"{}"}}"#, execution.code()),
        ))
        .expect("the state change is recorded");
}

fn projected_attempt_41a(store: &mut EventStore) -> AttemptRow {
    store.rebuild_attempts().expect("the projection rebuilds");
    store
        .attempt(&attempt_41a())
        .expect("reading succeeds")
        .expect("the attempt is projected")
}

fn an_open_ledger(scratch: &ScratchLedger) -> EventStore {
    EventStore::open(&scratch.path()).expect("a new ledger opens")
}

#[test]
fn an_admitted_attempt_appears_in_the_projection() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_ledger(&scratch);

    the_scheduler_admits_attempt_41a(&mut store);
    let row: AttemptRow = projected_attempt_41a(&mut store);

    assert_eq!(
        (row.task, row.execution, row.cleanup),
        (task_41(), Execution::Admitted, Cleanup::Held),
        "a new attempt belongs to its task, has not started and holds its resources"
    );
}

#[test]
fn the_projection_is_rebuilt_unchanged_after_reopening() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let before_restart: AttemptRow = {
        let mut store: EventStore = an_open_ledger(&scratch);
        the_scheduler_admits_attempt_41a(&mut store);
        attempt_41a_moves_to(&mut store, Execution::Running);
        attempt_41a_moves_to(&mut store, Execution::Exited(ExitStatus::Terminated));
        projected_attempt_41a(&mut store)
    };

    let mut reopened: EventStore = an_open_ledger(&scratch);
    let after_restart: AttemptRow = projected_attempt_41a(&mut reopened);

    assert_eq!(
        after_restart, before_restart,
        "a daemon restart must see exactly the state the events describe"
    );
}

#[test]
fn an_attempt_shows_the_digest_of_the_pack_it_started_from() {
    let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
    let mut store: EventStore = an_open_ledger(&scratch);
    let digest: ContentDigest =
        ContentDigest::try_new(blake3::hash(LOGIN_FIX_PACK).to_hex().as_str()).expect("blake3 hex");
    the_scheduler_admits_attempt_41a(&mut store);

    store
        .record_context_pack(&attempt_41a(), &digest, LOGIN_FIX_PACK)
        .expect("recording succeeds");
    let row: AttemptRow = projected_attempt_41a(&mut store);

    assert_eq!(
        row.context_pack,
        Some(digest),
        "anyone looking at an attempt can see exactly what context it started from"
    );
}
