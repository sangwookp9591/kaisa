//! The ledger's single writer.

use std::path::Path;

use kaisa_domain::{Event, RecordedEvent, Sequence};
use rusqlite::{Connection, Transaction, params};

use super::error::StoreError;
use super::rows::{StoredRow, decode_row, last_link, stored_sequence};
use crate::chain::{ChainHash, ChainedEvent, verify_chain};

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
    CREATE TABLE IF NOT EXISTS outbox (
        intent_id         TEXT    PRIMARY KEY,
        effect            TEXT    NOT NULL,
        detail            TEXT    NOT NULL,
        state             TEXT    NOT NULL CHECK (state IN ('pending', 'completed')),
        receipt           TEXT,
        recorded_sequence INTEGER NOT NULL REFERENCES events (sequence)
    );
    CREATE TABLE IF NOT EXISTS attempts (
        attempt_id    TEXT    PRIMARY KEY,
        task_id       TEXT    NOT NULL,
        execution     TEXT    NOT NULL,
        cleanup       TEXT    NOT NULL,
        context_pack  TEXT,
        last_sequence INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS context_packs (
        digest TEXT PRIMARY KEY,
        bytes  BLOB NOT NULL
    );
";

/// The ledger's event log. Holding an `EventStore` means holding the only write connection;
/// every mutating method takes `&mut self`.
#[derive(Debug)]
pub struct EventStore {
    pub(super) connection: Connection,
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

    /// Opens an existing ledger for reading only, leaving the state directory's file set and
    /// the database bytes unchanged. A missing file is an error rather than a new empty ledger.
    ///
    /// - With no `-wal` sidecar next to the file no writer is live and everything is in the main
    ///   file, so it is opened `immutable=1`: SQLite then creates no `-wal` or `-shm`, and the
    ///   directory may be read-only.
    /// - With a `-wal` sidecar a writer may be live and its newest commits are only in that
    ///   file, so an immutable open would show a stale view. A plain read-only open is used; it
    ///   creates nothing new. If that fails (for example a read-only directory with no `-shm`),
    ///   the open falls back to `immutable=1`, which then shows the main file without the
    ///   commits still waiting in the WAL.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] if the file does not exist or cannot be opened.
    pub fn open_read_only(path: &Path) -> Result<Self, StoreError> {
        let flags: rusqlite::OpenFlags =
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push("-wal");
        let live_writer_may_exist: bool = Path::new(&sidecar).exists();
        if live_writer_may_exist {
            if let Ok(connection) = Connection::open_with_flags(path, flags)
                && connection
                    .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .is_ok()
            {
                return Ok(Self { connection });
            }
        } else if !path.exists() {
            // Report a missing file as the plain open does, instead of a URI error.
            return Ok(Self {
                connection: Connection::open_with_flags(path, flags)?,
            });
        }
        let connection: Connection = Connection::open_with_flags(
            immutable_uri(path),
            flags | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )?;
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
        let chained: ChainedEvent = append_in(&transaction, event)?;
        transaction.commit()?;
        Ok(chained)
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

    /// Re-reads the whole log and checks that it forms an unbroken hash chain from genesis.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::ChainBroken`] at the first inconsistent position, or another
    /// [`StoreError`] if the log cannot be read.
    pub fn verify(&self) -> Result<(), StoreError> {
        let chain: Vec<ChainedEvent> = self.read_from(Sequence::FIRST)?;
        verify_chain(&chain).map_err(StoreError::ChainBroken)
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

/// Appends `event` inside an open transaction, so callers can commit it together with other
/// writes (the outbox records an intent and its event atomically).
pub(super) fn append_in(
    transaction: &Transaction<'_>,
    event: Event,
) -> Result<ChainedEvent, StoreError> {
    let (sequence, previous): (Sequence, ChainHash) = match last_link(transaction)? {
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
    Ok(ChainedEvent { recorded, hash })
}

/// The SQLite URI that opens `path` with `immutable=1`: unreserved characters and `/` stay,
/// everything else is percent-encoded, and a Windows drive path gets its leading `/`.
fn immutable_uri(path: &Path) -> String {
    let text: String = path.to_string_lossy().replace('\\', "/");
    let text: String = if text.as_bytes().get(1) == Some(&b':') {
        format!("/{text}")
    } else {
        text
    };
    let mut uri: String = String::from("file:");
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':') {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri.push_str("?immutable=1");
    uri
}
