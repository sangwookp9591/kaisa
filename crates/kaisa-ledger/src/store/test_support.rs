//! Test vocabulary for the event store: scratch ledgers, story events and attacker actions.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes ledgers created by tests running in parallel in the same process.
static NEXT_LEDGER: AtomicU64 = AtomicU64::new(0);

/// A ledger file in its own temporary directory, removed when dropped.
pub(crate) struct ScratchLedger {
    directory: PathBuf,
}

impl ScratchLedger {
    /// Where the ledger database lives.
    pub(crate) fn path(&self) -> PathBuf {
        self.directory.join("kaisa.db")
    }
}

impl Drop for ScratchLedger {
    fn drop(&mut self) {
        // Best effort: a leftover temp directory must not fail the test that used it.
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

use kaisa_domain::{Event, EventKind};

const CURRENT_PAYLOAD_VERSION: u16 = 1;

fn event(kind: EventKind, subject: &str, payload: &str) -> Event {
    Event {
        kind,
        subject: subject.to_owned(),
        payload_version: CURRENT_PAYLOAD_VERSION,
        payload: payload.to_owned(),
    }
}

/// Run 41 was started to fix the login test.
pub(crate) fn run_41_started() -> Event {
    event(
        EventKind::RunStarted,
        "run_41",
        r#"{"goal":"fix login test"}"#,
    )
}

/// Run 41's first attempt was admitted.
pub(crate) fn attempt_41a_admitted() -> Event {
    event(
        EventKind::AttemptAdmitted,
        "att_41a",
        r#"{"task":"task_41"}"#,
    )
}

/// Run 41's first attempt started running.
pub(crate) fn attempt_41a_running() -> Event {
    event(
        EventKind::AttemptStateChanged,
        "att_41a",
        r#"{"execution":"running"}"#,
    )
}

/// Someone with file access rewrites the subject of the event at `sequence`, bypassing Kaisa.
pub(crate) fn an_attacker_rewrites_the_subject_of(
    ledger: &std::path::Path,
    sequence: kaisa_domain::Sequence,
) {
    let connection = rusqlite::Connection::open(ledger).expect("the ledger file opens");
    connection
        .execute(
            "UPDATE events SET subject = 'att_forged' WHERE sequence = ?1",
            [i64::try_from(sequence.value()).expect("test sequences are small")],
        )
        .expect("the attacker's edit succeeds");
}

/// Someone with file access deletes the event at `sequence`, bypassing Kaisa.
pub(crate) fn an_attacker_deletes(ledger: &std::path::Path, sequence: kaisa_domain::Sequence) {
    let connection = rusqlite::Connection::open(ledger).expect("the ledger file opens");
    connection
        .execute(
            "DELETE FROM events WHERE sequence = ?1",
            [i64::try_from(sequence.value()).expect("test sequences are small")],
        )
        .expect("the attacker's delete succeeds");
}

/// Someone with file access stores a kind code that no Kaisa version ever wrote.
pub(crate) fn an_attacker_invents_an_event_kind_for(
    ledger: &std::path::Path,
    sequence: kaisa_domain::Sequence,
) {
    let connection = rusqlite::Connection::open(ledger).expect("the ledger file opens");
    connection
        .execute(
            "UPDATE events SET kind = 'invented.kind' WHERE sequence = ?1",
            [i64::try_from(sequence.value()).expect("test sequences are small")],
        )
        .expect("the attacker's edit succeeds");
}

/// Someone with file access cuts the stored chain hash of an event in half.
pub(crate) fn an_attacker_truncates_the_hash_of(
    ledger: &std::path::Path,
    sequence: kaisa_domain::Sequence,
) {
    let connection = rusqlite::Connection::open(ledger).expect("the ledger file opens");
    connection
        .execute(
            "UPDATE events SET hash = substr(hash, 1, length(hash) / 2) WHERE sequence = ?1",
            [i64::try_from(sequence.value()).expect("test sequences are small")],
        )
        .expect("the attacker's edit succeeds");
}

/// A path where no ledger exists yet.
pub(crate) fn a_ledger_path_that_does_not_exist_yet() -> ScratchLedger {
    let directory: PathBuf = std::env::temp_dir().join(format!(
        "kaisa-ledger-test-{}-{}",
        std::process::id(),
        NEXT_LEDGER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory).expect("the temp directory is writable");
    ScratchLedger { directory }
}
