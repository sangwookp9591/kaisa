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
