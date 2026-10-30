//! The SQLite-backed event store: the single writer of the ledger (architecture §5.4).

use std::fmt;
use std::path::Path;

use kaisa_domain::{Event, EventKind, RecordedEvent, Sequence};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::chain::{ChainHash, ChainedEvent};

/// Schema of the event log. Rows are only ever inserted.
const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS events (
        sequence        INTEGER PRIMARY KEY,
        kind            TEXT    NOT NULL,
        subject         TEXT    NOT NULL,
        payload_version INTEGER NOT NULL,
        payload         TEXT    NOT NULL,
        hash            BLOB    NOT NULL
    );
";

/// Why the event store could not do what was asked.
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreError {
    /// SQLite reported an error.
    Sqlite(rusqlite::Error),
    /// A stored row cannot be decoded into an event.
    Corrupt {
        /// The stored sequence of the bad row.
        stored_sequence: i64,
        /// What was wrong with it.
        reason: &'static str,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(f, "ledger database error: {error}"),
            Self::Corrupt {
                stored_sequence,
                reason,
            } => {
                write!(f, "ledger row {stored_sequence} is corrupt: {reason}")
            }
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            Self::Corrupt { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// The ledger's event log. Holding an `EventStore` means holding the only write connection;
/// every mutating method takes `&mut self`.
#[derive(Debug)]
pub struct EventStore {
    connection: Connection,
}

impl EventStore {
    /// Opens the ledger at `path`, creating the file and schema if needed. The database uses
    /// write-ahead logging with full synchronous commits, so a committed event survives a
    /// crash or power loss.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] if the file cannot be opened or configured.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let connection: Connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self { connection })
    }

    /// Records `event` at the next sequence, linked to the previous event's hash. Assigning the
    /// sequence, computing the hash and inserting the row happen in one transaction.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] if the transaction fails, or [`StoreError::Corrupt`] if
    /// the last stored row cannot be decoded.
    pub fn append(&mut self, event: Event) -> Result<ChainedEvent, StoreError> {
        let transaction: Transaction<'_> = self.connection.transaction()?;
        let (sequence, previous): (Sequence, ChainHash) = match last_link(&transaction)? {
            Some((last_sequence, last_hash)) => (last_sequence.next(), last_hash),
            None => (Sequence::FIRST, ChainHash::genesis()),
        };
        let recorded = RecordedEvent { sequence, event };
        let hash: ChainHash = previous.link(&recorded);
        transaction.execute(
            "INSERT INTO events (sequence, kind, subject, payload_version, payload, hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                stored_sequence(sequence),
                recorded.event.kind.code(),
                recorded.event.subject,
                recorded.event.payload_version,
                recorded.event.payload,
                hash.as_bytes().as_slice(),
            ],
        )?;
        transaction.commit()?;
        Ok(ChainedEvent { recorded, hash })
    }

    /// Every event at `from` or later, in sequence order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] if the query fails, or [`StoreError::Corrupt`] if a row
    /// cannot be decoded.
    pub fn read_from(&self, from: Sequence) -> Result<Vec<ChainedEvent>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT sequence, kind, subject, payload_version, payload, hash
             FROM events WHERE sequence >= ?1 ORDER BY sequence",
        )?;
        let rows = statement.query_map(params![stored_sequence(from)], |row| {
            Ok(StoredRow {
                sequence: row.get(0)?,
                kind: row.get(1)?,
                subject: row.get(2)?,
                payload_version: row.get(3)?,
                payload: row.get(4)?,
                hash: row.get(5)?,
            })
        })?;
        rows.map(|row| decode_row(row?)).collect()
    }

    /// Number of events recorded so far.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] if the count cannot be read.
    pub fn len(&self) -> Result<u64, StoreError> {
        let count: i64 = self
            .connection
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
        Ok(u64::try_from(count).expect("SQLite never returns a negative COUNT"))
    }

    /// True when no event has been recorded.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] if the count cannot be read.
    pub fn is_empty(&self) -> Result<bool, StoreError> {
        Ok(self.len()? == 0)
    }

    /// The journal mode SQLite is actually using, lowercase (for example `"wal"`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] if the pragma cannot be read.
    pub fn journal_mode(&self) -> Result<String, StoreError> {
        let mode: String = self
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        Ok(mode.to_lowercase())
    }
}

/// The sequence and hash of the last stored event, if any.
fn last_link(transaction: &Transaction<'_>) -> Result<Option<(Sequence, ChainHash)>, StoreError> {
    let row: Option<(i64, Vec<u8>)> = transaction
        .query_row(
            "SELECT sequence, hash FROM events ORDER BY sequence DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(stored, hash)| Ok((decode_sequence(stored)?, decode_hash(stored, hash)?)))
        .transpose()
}

/// Converts a sequence to SQLite's signed integer type.
fn stored_sequence(sequence: Sequence) -> i64 {
    i64::try_from(sequence.value()).expect("sequences stay below i64::MAX")
}

fn decode_sequence(stored: i64) -> Result<Sequence, StoreError> {
    u64::try_from(stored)
        .ok()
        .and_then(Sequence::from_stored)
        .ok_or(StoreError::Corrupt {
            stored_sequence: stored,
            reason: "sequence is not positive",
        })
}

/// A row exactly as SQLite returns it, before validation.
struct StoredRow {
    sequence: i64,
    kind: String,
    subject: String,
    payload_version: i64,
    payload: String,
    hash: Vec<u8>,
}

fn decode_row(row: StoredRow) -> Result<ChainedEvent, StoreError> {
    let corrupt = |reason: &'static str| StoreError::Corrupt {
        stored_sequence: row.sequence,
        reason,
    };
    let kind: EventKind = EventKind::from_code(&row.kind).ok_or(corrupt("unknown event kind"))?;
    let payload_version: u16 =
        u16::try_from(row.payload_version).map_err(|_| corrupt("payload version out of range"))?;
    let recorded = RecordedEvent {
        sequence: decode_sequence(row.sequence)?,
        event: Event {
            kind,
            subject: row.subject,
            payload_version,
            payload: row.payload,
        },
    };
    Ok(ChainedEvent {
        recorded,
        hash: decode_hash(row.sequence, row.hash)?,
    })
}

fn decode_hash(stored_sequence: i64, bytes: Vec<u8>) -> Result<ChainHash, StoreError> {
    let bytes: [u8; blake3::OUT_LEN] = bytes.try_into().map_err(|_| StoreError::Corrupt {
        stored_sequence,
        reason: "hash has the wrong length",
    })?;
    Ok(ChainHash::from_bytes(bytes))
}

#[cfg(test)]
pub(crate) mod test_support {
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
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{EventKind, Sequence};

    use super::EventStore;
    use super::test_support::{
        ScratchLedger, a_ledger_path_that_does_not_exist_yet, attempt_41a_admitted,
        attempt_41a_running, run_41_started,
    };
    use crate::chain::{ChainHash, ChainedEvent};

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
        let last_before_restart: ChainedEvent =
            history.last().expect("history is not empty").clone();
        drop(store);
        let mut reopened: EventStore =
            EventStore::open(&scratch.path()).expect("the ledger reopens");

        let after_restart: ChainedEvent =
            reopened.append(run_41_started()).expect("append succeeds");

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
}
