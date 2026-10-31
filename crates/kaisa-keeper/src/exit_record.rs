//! The durable record a keeper leaves when its agent process exits (architecture §5.1).
//!
//! The record is a small line-based text file. It ends with an explicit `end` line, so a file
//! cut off by a crash mid-write is detected instead of being read as a valid record.

use std::fmt;

use kaisa_domain::{AttemptId, ExitStatus};

/// First line of every record; a new format gets a new header.
const HEADER: &str = "kaisa-exit-record v1";
/// Last line of every complete record.
const END_MARKER: &str = "end";
/// Text of [`ExitStatus::Terminated`] in the file.
const TERMINATED: &str = "terminated";
/// A proven non-start, distinct from a process exit code or external termination.
const FAILED_TO_START: &str = "failed-to-start";
/// Prefix of an exit code in the file, as in `code:0`.
const CODE_PREFIX: &str = "code:";

/// What a keeper observed when its agent exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitRecord {
    /// The attempt the process belonged to.
    pub attempt: AttemptId,
    /// Which start of the attempt's process this was (restarts get a new incarnation).
    pub incarnation: u64,
    /// How the process ended.
    pub status: ExitStatus,
    /// When the exit was observed, in milliseconds since the Unix epoch.
    pub finished_at_ms: u64,
}

/// Why a record file could not be read as an exit record.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecordError {
    /// The file was written by a different format version.
    UnknownFormat(String),
    /// The file ends before its `end` line: the write did not finish.
    Truncated,
    /// A line is missing, out of order or has an invalid value.
    Malformed(String),
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFormat(header) => write!(f, "unknown exit record format: {header:?}"),
            Self::Truncated => write!(f, "exit record is incomplete"),
            Self::Malformed(line) => write!(f, "malformed exit record line: {line:?}"),
        }
    }
}

impl std::error::Error for RecordError {}

impl ExitRecord {
    /// The record as file contents.
    #[must_use]
    pub fn encode(&self) -> String {
        let status: String = match self.status {
            ExitStatus::Code(code) => format!("{CODE_PREFIX}{code}"),
            ExitStatus::Terminated => TERMINATED.to_owned(),
            ExitStatus::FailedToStart => FAILED_TO_START.to_owned(),
        };
        format!(
            "{HEADER}\nattempt={}\nincarnation={}\nstatus={status}\nfinished_at_ms={}\n{END_MARKER}\n",
            self.attempt, self.incarnation, self.finished_at_ms
        )
    }

    /// Reads a record from file contents.
    ///
    /// # Errors
    ///
    /// Returns [`RecordError::UnknownFormat`] for another format version,
    /// [`RecordError::Truncated`] if the `end` line is missing, or [`RecordError::Malformed`]
    /// for any invalid line.
    pub fn decode(text: &str) -> Result<Self, RecordError> {
        let mut lines = text.lines();
        let header: &str = lines.next().ok_or(RecordError::Truncated)?;
        if header != HEADER {
            return Err(RecordError::UnknownFormat(header.to_owned()));
        }
        let mut field = |name: &str| -> Result<String, RecordError> {
            let line: &str = lines.next().ok_or(RecordError::Truncated)?;
            line.strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('='))
                .map(str::to_owned)
                .ok_or_else(|| RecordError::Malformed(line.to_owned()))
        };
        let attempt_text: String = field("attempt")?;
        let incarnation_text: String = field("incarnation")?;
        let status_text: String = field("status")?;
        let finished_text: String = field("finished_at_ms")?;
        match lines.next() {
            Some(END_MARKER) => {}
            Some(other) => return Err(RecordError::Malformed(other.to_owned())),
            None => return Err(RecordError::Truncated),
        }
        Ok(Self {
            attempt: AttemptId::try_new(&attempt_text)
                .map_err(|_| RecordError::Malformed(attempt_text.clone()))?,
            incarnation: parse_number(&incarnation_text)?,
            status: parse_status(&status_text)?,
            finished_at_ms: parse_number(&finished_text)?,
        })
    }
}

fn parse_number(text: &str) -> Result<u64, RecordError> {
    text.parse()
        .map_err(|_| RecordError::Malformed(text.to_owned()))
}

fn parse_status(text: &str) -> Result<ExitStatus, RecordError> {
    if text == TERMINATED {
        return Ok(ExitStatus::Terminated);
    }
    if text == FAILED_TO_START {
        return Ok(ExitStatus::FailedToStart);
    }
    text.strip_prefix(CODE_PREFIX)
        .and_then(|code| code.parse().ok())
        .map(ExitStatus::Code)
        .ok_or_else(|| RecordError::Malformed(text.to_owned()))
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{AttemptId, ExitStatus};

    use super::{ExitRecord, RecordError};

    /// The exit code a typical CLI agent uses for "I failed".
    const AGENT_FAILURE_CODE: i32 = 1;
    /// The first start of an attempt's process.
    const FIRST_INCARNATION: u64 = 1;
    /// 2026-10-30T12:00:00Z, the moment attempt 41a finished.
    const ATTEMPT_41A_FINISHED_AT_MS: u64 = 1_793_361_600_000;

    fn attempt_41a() -> AttemptId {
        AttemptId::try_new("att_41a").expect("a valid attempt id")
    }

    fn attempt_41a_exited_with(status: ExitStatus) -> ExitRecord {
        ExitRecord {
            attempt: attempt_41a(),
            incarnation: FIRST_INCARNATION,
            status,
            finished_at_ms: ATTEMPT_41A_FINISHED_AT_MS,
        }
    }

    /// What is left of a record file when the machine lost power before the last line.
    fn cut_off_before_its_end_line(record: &ExitRecord) -> String {
        record.encode().replace("end\n", "")
    }

    #[test]
    fn an_exit_record_round_trips_through_its_file_format() {
        let failed: ExitRecord = attempt_41a_exited_with(ExitStatus::Code(AGENT_FAILURE_CODE));

        assert_eq!(
            ExitRecord::decode(&failed.encode()),
            Ok(failed),
            "what the keeper writes the daemon must read back unchanged"
        );
    }

    #[test]
    fn a_proven_non_start_round_trips_without_inventing_a_process_exit_code() {
        let not_started = attempt_41a_exited_with(ExitStatus::FailedToStart);
        assert_eq!(
            ExitRecord::decode(&not_started.encode()),
            Ok(not_started),
            "recovery must retain proof that process creation failed"
        );
    }

    #[test]
    fn a_terminated_exit_round_trips_through_its_file_format() {
        let killed: ExitRecord = attempt_41a_exited_with(ExitStatus::Terminated);

        assert_eq!(
            ExitRecord::decode(&killed.encode()),
            Ok(killed),
            "termination by signal is recorded as such, not as an exit code"
        );
    }

    #[test]
    fn a_truncated_exit_record_file_is_reported_as_corrupt() {
        let partial: String =
            cut_off_before_its_end_line(&attempt_41a_exited_with(ExitStatus::Terminated));

        assert_eq!(
            ExitRecord::decode(&partial),
            Err(RecordError::Truncated),
            "a half-written record is not proof of exit"
        );
    }

    #[test]
    fn a_record_from_an_unknown_format_version_is_rejected() {
        let future_format: String = attempt_41a_exited_with(ExitStatus::Terminated)
            .encode()
            .replace("kaisa-exit-record v1", "kaisa-exit-record v9");

        assert!(
            matches!(
                ExitRecord::decode(&future_format),
                Err(RecordError::UnknownFormat(_))
            ),
            "a record this build cannot interpret must not be guessed at"
        );
    }

    #[test]
    fn a_record_with_an_invalid_attempt_id_is_reported_as_malformed() {
        let forged: String = attempt_41a_exited_with(ExitStatus::Terminated)
            .encode()
            .replace("attempt=att_41a", "attempt=../../etc");

        assert!(
            matches!(ExitRecord::decode(&forged), Err(RecordError::Malformed(_))),
            "only valid attempt ids are accepted from a file on disk"
        );
    }
}
