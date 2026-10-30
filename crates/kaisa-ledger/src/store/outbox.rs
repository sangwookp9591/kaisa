//! The outbox: external effects are announced in the ledger before they happen (P9).
//!
//! Recording an intent writes the outbox row and its `IntentRecorded` event in one
//! transaction, so after a crash the ledger always knows which effects may have started.

use kaisa_domain::{EffectKind, Event, EventKind, IntentId};
use rusqlite::{Transaction, params};

use super::error::StoreError;
use super::event_store::{EventStore, append_in};
use crate::chain::ChainedEvent;

/// Payload schema version of `IntentRecorded` events.
const INTENT_PAYLOAD_VERSION: u16 = 1;

/// SQLite's extended result code for a violated PRIMARY KEY constraint.
const SQLITE_CONSTRAINT_PRIMARYKEY: i32 = 1555;

/// An external effect Kaisa is about to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// Identifies this intent; also the subject of its events.
    pub id: IntentId,
    /// What kind of effect it is.
    pub effect: EffectKind,
    /// Effect-specific detail as canonical JSON text, such as the worktree path.
    pub detail: String,
}

/// An intent that has been recorded but not yet completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingIntent {
    /// The intent as it was recorded.
    pub intent: Intent,
}

impl EventStore {
    /// Records `intent` as pending and appends its `IntentRecorded` event, atomically.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::DuplicateIntent`] if an intent with the same id exists; in that
    /// case nothing is written. Returns another [`StoreError`] if the transaction fails.
    pub fn record_intent(&mut self, intent: &Intent) -> Result<ChainedEvent, StoreError> {
        let transaction: Transaction<'_> = self.connection.transaction()?;
        let chained: ChainedEvent = append_in(&transaction, intent_recorded_event(intent))?;
        transaction
            .execute(
                "INSERT INTO outbox (intent_id, effect, detail, state, recorded_sequence)
                 VALUES (?1, ?2, ?3, 'pending', ?4)",
                params![
                    intent.id.as_str(),
                    intent.effect.code(),
                    intent.detail,
                    super::rows::stored_sequence(chained.recorded.sequence),
                ],
            )
            .map_err(|error| duplicate_or(error, &intent.id))?;
        transaction.commit()?;
        Ok(chained)
    }

    /// Every intent that has been recorded and not completed, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the outbox cannot be read or a row cannot be decoded.
    pub fn pending_intents(&self) -> Result<Vec<PendingIntent>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT intent_id, effect, detail FROM outbox
             WHERE state = 'pending' ORDER BY recorded_sequence",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (id, effect, detail) = row?;
            Ok(PendingIntent {
                intent: decode_intent(&id, &effect, detail)?,
            })
        })
        .collect()
    }
}

/// The ledger event that announces `intent`.
fn intent_recorded_event(intent: &Intent) -> Event {
    Event {
        kind: EventKind::IntentRecorded,
        subject: intent.id.as_str().to_owned(),
        payload_version: INTENT_PAYLOAD_VERSION,
        payload: format!(
            r#"{{"effect":"{}","detail":{}}}"#,
            intent.effect.code(),
            intent.detail
        ),
    }
}

/// Maps a primary-key violation on the outbox to [`StoreError::DuplicateIntent`].
fn duplicate_or(error: rusqlite::Error, id: &IntentId) -> StoreError {
    match &error {
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.extended_code == SQLITE_CONSTRAINT_PRIMARYKEY =>
        {
            StoreError::DuplicateIntent(id.clone())
        }
        _ => StoreError::Sqlite(error),
    }
}

fn decode_intent(id: &str, effect: &str, detail: String) -> Result<Intent, StoreError> {
    let id: IntentId = IntentId::try_new(id).map_err(|_| StoreError::CorruptOutbox)?;
    let effect: EffectKind = EffectKind::from_code(effect).ok_or(StoreError::CorruptOutbox)?;
    Ok(Intent { id, effect, detail })
}
