//! Errors reported by the event store.

use std::fmt;

use kaisa_domain::IntentId;

use crate::chain::ChainBreak;

/// What is wrong with a stored row that cannot be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CorruptReason {
    /// The stored sequence is zero or negative.
    NonPositiveSequence,
    /// The stored kind code does not name any [`EventKind`].
    UnknownEventKind,
    /// The stored payload version does not fit the payload version type.
    PayloadVersionOutOfRange,
    /// The stored hash does not have the length of a chain hash.
    WrongHashLength,
}

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
        reason: CorruptReason,
    },
    /// The stored events do not form an unbroken chain.
    ChainBroken(ChainBreak),
    /// An intent with this id was already recorded.
    DuplicateIntent(IntentId),
    /// No intent with this id was ever recorded.
    UnknownIntent(IntentId),
    /// The intent was already completed with a different receipt.
    ReceiptConflict(IntentId),
    /// An outbox row cannot be decoded into an intent.
    CorruptOutbox,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(f, "ledger database error: {error}"),
            Self::Corrupt {
                stored_sequence,
                reason,
            } => {
                write!(f, "ledger row {stored_sequence} is corrupt: {reason:?}")
            }
            Self::ChainBroken(chain_break) => write!(f, "{chain_break}"),
            Self::DuplicateIntent(id) => write!(f, "intent {id} was already recorded"),
            Self::UnknownIntent(id) => write!(f, "intent {id} was never recorded"),
            Self::ReceiptConflict(id) => {
                write!(
                    f,
                    "intent {id} was already completed with a different receipt"
                )
            }
            Self::CorruptOutbox => write!(f, "an outbox row cannot be decoded"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            Self::Corrupt { .. } => None,
            Self::ChainBroken(chain_break) => Some(chain_break),
            Self::DuplicateIntent(_)
            | Self::UnknownIntent(_)
            | Self::ReceiptConflict(_)
            | Self::CorruptOutbox => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}
