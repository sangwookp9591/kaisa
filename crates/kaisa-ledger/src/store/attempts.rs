//! The attempts projection: the current state of every attempt, rebuilt from the event log
//! (architecture §5.4, §7). The events are the authority; this table is a cache that can always
//! be thrown away and rebuilt, which is what recovery after a crash does.

use kaisa_domain::{AttemptId, Cleanup, ContentDigest, EventKind, Execution, Sequence, TaskId};
use rusqlite::{OptionalExtension, params};

use super::error::StoreError;
use super::event_store::EventStore;

/// Payload key of the task an admitted attempt belongs to.
const TASK_KEY: &str = "task";
/// Payload key of a new execution state.
const EXECUTION_KEY: &str = "execution";
/// Payload key of a new cleanup state.
const CLEANUP_KEY: &str = "cleanup";
/// Payload key of a context pack's digest.
const DIGEST_KEY: &str = "digest";

/// One attempt as the projection sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRow {
    /// The attempt.
    pub attempt: AttemptId,
    /// The task it belongs to.
    pub task: TaskId,
    /// Where its process is.
    pub execution: Execution,
    /// What has happened to its resources.
    pub cleanup: Cleanup,
    /// The pack it started from, once recorded.
    pub context_pack: Option<ContentDigest>,
}

impl EventStore {
    /// Throws the attempts projection away and rebuilds it from every event, in one transaction.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::CorruptPayload`] if an attempt event cannot be read, or another
    /// [`StoreError`] if reading or writing fails.
    pub fn rebuild_attempts(&mut self) -> Result<(), StoreError> {
        let events = self.read_from(Sequence::FIRST)?;
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM attempts", [])?;
        for chained in events {
            let recorded = chained.recorded;
            let sequence: i64 = super::rows::stored_sequence(recorded.sequence);
            let event = recorded.event;
            let corrupt = || StoreError::CorruptPayload {
                stored_sequence: sequence,
            };
            match event.kind {
                EventKind::AttemptAdmitted => {
                    let task: String =
                        payload_field(&event.payload, TASK_KEY).ok_or_else(corrupt)?;
                    TaskId::try_new(&task).map_err(|_| corrupt())?;
                    transaction.execute(
                        "INSERT INTO attempts (attempt_id, task_id, execution, cleanup, last_sequence)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            event.subject,
                            task,
                            Execution::Admitted.code(),
                            Cleanup::Held.code(),
                            sequence
                        ],
                    )?;
                }
                EventKind::AttemptStateChanged => {
                    if let Some(execution) = payload_field(&event.payload, EXECUTION_KEY) {
                        Execution::from_code(&execution).ok_or_else(corrupt)?;
                        transaction.execute(
                            "UPDATE attempts SET execution = ?2, last_sequence = ?3 WHERE attempt_id = ?1",
                            params![event.subject, execution, sequence],
                        )?;
                    } else if let Some(cleanup) = payload_field(&event.payload, CLEANUP_KEY) {
                        Cleanup::from_code(&cleanup).ok_or_else(corrupt)?;
                        transaction.execute(
                            "UPDATE attempts SET cleanup = ?2, last_sequence = ?3 WHERE attempt_id = ?1",
                            params![event.subject, cleanup, sequence],
                        )?;
                    } else {
                        return Err(corrupt());
                    }
                }
                EventKind::ContextPackRecorded => {
                    let digest: String =
                        payload_field(&event.payload, DIGEST_KEY).ok_or_else(corrupt)?;
                    ContentDigest::try_new(&digest).map_err(|_| corrupt())?;
                    transaction.execute(
                        "UPDATE attempts SET context_pack = ?2, last_sequence = ?3 WHERE attempt_id = ?1",
                        params![event.subject, digest, sequence],
                    )?;
                }
                _ => {}
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// The projected state of `attempt`, if it was ever admitted.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::CorruptPayload`] if a stored value cannot be decoded, or
    /// [`StoreError::Sqlite`] if the query fails.
    pub fn attempt(&self, attempt: &AttemptId) -> Result<Option<AttemptRow>, StoreError> {
        let row: Option<(String, String, String, Option<String>, i64)> = self
            .connection
            .query_row(
                "SELECT task_id, execution, cleanup, context_pack, last_sequence
                 FROM attempts WHERE attempt_id = ?1",
                params![attempt.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((task, execution, cleanup, context_pack, sequence)) = row else {
            return Ok(None);
        };
        let corrupt = || StoreError::CorruptPayload {
            stored_sequence: sequence,
        };
        Ok(Some(AttemptRow {
            attempt: attempt.clone(),
            task: TaskId::try_new(&task).map_err(|_| corrupt())?,
            execution: Execution::from_code(&execution).ok_or_else(corrupt)?,
            cleanup: Cleanup::from_code(&cleanup).ok_or_else(corrupt)?,
            context_pack: context_pack
                .map(|digest| ContentDigest::try_new(&digest).map_err(|_| corrupt()))
                .transpose()?,
        }))
    }
}

/// The string value of `key` in a JSON object payload.
fn payload_field(payload: &str, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get(key)?
        .as_str()
        .map(str::to_owned)
}
