//! The attempts projection: the current state of every attempt, rebuilt from the event log
//! (architecture §5.4, §7). The events are the authority; this table is a cache that can always
//! be thrown away and rebuilt, which is what recovery after a crash does.

use kaisa_domain::{AttemptId, Cleanup, ContentDigest, EventKind, Execution, Sequence, TaskId};
use rusqlite::{OptionalExtension, params};

use super::error::StoreError;
use super::event_store::{EventStore, read_from_connection};

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
        let transaction = self.connection.transaction()?;
        let events = read_from_connection(&transaction, Sequence::FIRST)?;
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

    /// Every attempt, folded from the event log in memory in the order its state last changed.
    /// Unlike [`EventStore::all_attempts`] this writes nothing, so a read-only ledger can use it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::CorruptPayload`] if an attempt event cannot be read, or another
    /// [`StoreError`] if reading fails.
    pub fn attempts_from_log(&self) -> Result<Vec<AttemptRow>, StoreError> {
        let mut rows: Vec<(i64, AttemptRow)> = Vec::new();
        for chained in self.read_from(Sequence::FIRST)? {
            let recorded = chained.recorded;
            let sequence: i64 = super::rows::stored_sequence(recorded.sequence);
            let event = recorded.event;
            let corrupt = || StoreError::CorruptPayload {
                stored_sequence: sequence,
            };
            let position = rows
                .iter()
                .position(|(_, row)| row.attempt.as_str() == event.subject);
            match event.kind {
                EventKind::AttemptAdmitted => {
                    let task: String =
                        payload_field(&event.payload, TASK_KEY).ok_or_else(corrupt)?;
                    if position.is_some() {
                        return Err(corrupt());
                    }
                    rows.push((
                        sequence,
                        AttemptRow {
                            attempt: AttemptId::try_new(&event.subject).map_err(|_| corrupt())?,
                            task: TaskId::try_new(&task).map_err(|_| corrupt())?,
                            execution: Execution::Admitted,
                            cleanup: Cleanup::Held,
                            context_pack: None,
                        },
                    ));
                }
                EventKind::AttemptStateChanged => {
                    let execution = payload_field(&event.payload, EXECUTION_KEY);
                    let cleanup = payload_field(&event.payload, CLEANUP_KEY);
                    if execution.is_none() && cleanup.is_none() {
                        return Err(corrupt());
                    }
                    let parsed_execution = execution
                        .map(|code| Execution::from_code(&code).ok_or_else(corrupt))
                        .transpose()?;
                    let parsed_cleanup = match (&parsed_execution, cleanup) {
                        (None, Some(code)) => Some(Cleanup::from_code(&code).ok_or_else(corrupt)?),
                        _ => None,
                    };
                    if let Some(index) = position {
                        let (last, row) = &mut rows[index];
                        *last = sequence;
                        if let Some(execution) = parsed_execution {
                            row.execution = execution;
                        }
                        if let Some(cleanup) = parsed_cleanup {
                            row.cleanup = cleanup;
                        }
                    }
                }
                EventKind::ContextPackRecorded => {
                    let digest: String =
                        payload_field(&event.payload, DIGEST_KEY).ok_or_else(corrupt)?;
                    let digest = ContentDigest::try_new(&digest).map_err(|_| corrupt())?;
                    if let Some(index) = position {
                        let (last, row) = &mut rows[index];
                        *last = sequence;
                        row.context_pack = Some(digest);
                    }
                }
                _ => {}
            }
        }
        rows.sort_by_key(|(last, _)| *last);
        Ok(rows.into_iter().map(|(_, row)| row).collect())
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

impl EventStore {
    /// Attempts whose resources are still held: neither reclaimed nor deliberately retained.
    /// After a crash these are what recovery must look at, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::CorruptPayload`] if a stored value cannot be decoded, or
    /// [`StoreError::Sqlite`] if the projection cannot be rebuilt or queried.
    pub fn unfinished_attempts(&mut self) -> Result<Vec<AttemptRow>, StoreError> {
        Ok(self
            .all_attempts()?
            .into_iter()
            .filter(|row| !matches!(row.cleanup, Cleanup::Reclaimed | Cleanup::Retained))
            .collect())
    }

    /// Every attempt the ledger knows, in the order its state last changed (T30a).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::CorruptPayload`] if a stored value cannot be decoded, or
    /// [`StoreError::Sqlite`] if the projection cannot be rebuilt or queried.
    pub fn all_attempts(&mut self) -> Result<Vec<AttemptRow>, StoreError> {
        self.rebuild_attempts()?;
        let ids: Vec<String> = {
            let mut statement = self
                .connection
                .prepare("SELECT attempt_id FROM attempts ORDER BY last_sequence")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut all: Vec<AttemptRow> = Vec::new();
        for id in ids {
            let attempt = AttemptId::try_new(&id)
                .map_err(|_| StoreError::CorruptPayload { stored_sequence: 0 })?;
            all.extend(self.attempt(&attempt)?);
        }
        Ok(all)
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
