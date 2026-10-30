//! Encoding and strict decoding of stored event rows.

use kaisa_domain::{Event, EventKind, RecordedEvent, Sequence};
use rusqlite::{OptionalExtension, Transaction};

use super::error::{CorruptReason, StoreError};
use crate::chain::{ChainHash, ChainedEvent};

/// The sequence and hash of the last stored event, if any.
pub(super) fn last_link(
    transaction: &Transaction<'_>,
) -> Result<Option<(Sequence, ChainHash)>, StoreError> {
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
pub(super) fn stored_sequence(sequence: Sequence) -> i64 {
    i64::try_from(sequence.value()).expect("sequences stay below i64::MAX")
}

pub(super) fn decode_sequence(stored: i64) -> Result<Sequence, StoreError> {
    u64::try_from(stored)
        .ok()
        .and_then(Sequence::from_stored)
        .ok_or(StoreError::Corrupt {
            stored_sequence: stored,
            reason: CorruptReason::NonPositiveSequence,
        })
}

/// A row exactly as SQLite returns it, before validation.
pub(super) struct StoredRow {
    pub(super) sequence: i64,
    pub(super) kind: String,
    pub(super) subject: String,
    pub(super) payload_version: i64,
    pub(super) payload: String,
    pub(super) hash: Vec<u8>,
}

pub(super) fn decode_row(row: StoredRow) -> Result<ChainedEvent, StoreError> {
    let corrupt = |reason: CorruptReason| StoreError::Corrupt {
        stored_sequence: row.sequence,
        reason,
    };
    let kind: EventKind =
        EventKind::from_code(&row.kind).ok_or(corrupt(CorruptReason::UnknownEventKind))?;
    let payload_version: u16 = u16::try_from(row.payload_version)
        .map_err(|_| corrupt(CorruptReason::PayloadVersionOutOfRange))?;
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

pub(super) fn decode_hash(stored_sequence: i64, bytes: Vec<u8>) -> Result<ChainHash, StoreError> {
    let bytes: [u8; blake3::OUT_LEN] = bytes.try_into().map_err(|_| StoreError::Corrupt {
        stored_sequence,
        reason: CorruptReason::WrongHashLength,
    })?;
    Ok(ChainHash::from_bytes(bytes))
}
