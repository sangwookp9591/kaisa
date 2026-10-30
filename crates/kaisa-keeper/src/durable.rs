//! Writing exit records so they survive crashes and are never overwritten.
//!
//! The record is written to a temporary file, flushed to disk, then published under its final
//! name with a hard link. Linking fails if the final name exists, so a second record for the
//! same incarnation can never replace the first, even if two writers race.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::exit_record::{ExitRecord, RecordError};

/// Why an exit record could not be written or read.
#[derive(Debug)]
#[non_exhaustive]
pub enum DurableError {
    /// A record for this incarnation already exists; it is never overwritten.
    AlreadyExists(PathBuf),
    /// The file system failed.
    Io(io::Error),
    /// The file exists but is not a valid record.
    Record(RecordError),
}

impl fmt::Display for DurableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists(path) => {
                write!(f, "exit record {} already exists", path.display())
            }
            Self::Io(error) => write!(f, "exit record I/O failed: {error}"),
            Self::Record(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DurableError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AlreadyExists(_) => None,
            Self::Io(error) => Some(error),
            Self::Record(error) => Some(error),
        }
    }
}

impl From<io::Error> for DurableError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Where the record of `incarnation` lives inside an attempt directory.
#[must_use]
pub fn exit_record_path(attempt_directory: &Path, incarnation: u64) -> PathBuf {
    attempt_directory.join(format!("exit-{incarnation}.record"))
}

/// Durably writes `record` into `attempt_directory` and returns its path.
///
/// # Errors
///
/// Returns [`DurableError::AlreadyExists`] if a record for the same incarnation exists (it is
/// left untouched), or [`DurableError::Io`] if writing fails.
pub fn write_exit_record(
    attempt_directory: &Path,
    record: &ExitRecord,
) -> Result<PathBuf, DurableError> {
    let final_path: PathBuf = exit_record_path(attempt_directory, record.incarnation);
    let temporary_path: PathBuf =
        attempt_directory.join(format!(".exit-{}.record.tmp", record.incarnation));
    let written: Result<(), DurableError> = write_and_publish(&temporary_path, &final_path, record);
    // The temporary file is never needed again, whether publishing worked or not.
    let _ = fs::remove_file(&temporary_path);
    written?;
    sync_directory(attempt_directory)?;
    Ok(final_path)
}

/// Reads the record at `path`.
///
/// # Errors
///
/// Returns [`DurableError::Io`] if it cannot be read, or [`DurableError::Record`] if it is not
/// a complete, valid record.
pub fn read_exit_record(path: &Path) -> Result<ExitRecord, DurableError> {
    let text: String = fs::read_to_string(path)?;
    ExitRecord::decode(&text).map_err(DurableError::Record)
}

fn write_and_publish(
    temporary_path: &Path,
    final_path: &Path,
    record: &ExitRecord,
) -> Result<(), DurableError> {
    if final_path.exists() {
        return Err(DurableError::AlreadyExists(final_path.to_owned()));
    }
    let mut file: File = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary_path)?;
    file.write_all(record.encode().as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::hard_link(temporary_path, final_path).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            DurableError::AlreadyExists(final_path.to_owned())
        } else {
            DurableError::Io(error)
        }
    })
}

/// Makes the new directory entry itself durable. Windows has no portable equivalent; NTFS
/// journals metadata changes, so the call is skipped there.
fn sync_directory(directory: &Path) -> io::Result<()> {
    if cfg!(unix) {
        File::open(directory)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kaisa_domain::{AttemptId, ExitStatus};

    use super::{DurableError, read_exit_record, write_exit_record};
    use crate::exit_record::ExitRecord;
    use crate::test_support::{ScratchAttemptDirectory, an_empty_attempt_directory};

    /// The first start of an attempt's process.
    const FIRST_INCARNATION: u64 = 1;
    /// 2026-10-30T12:00:00Z.
    const FINISHED_AT_MS: u64 = 1_793_361_600_000;
    /// The exit code a typical CLI agent uses for "I failed".
    const AGENT_FAILURE_CODE: i32 = 1;

    fn attempt_41a_exited_with(status: ExitStatus) -> ExitRecord {
        ExitRecord {
            attempt: AttemptId::try_new("att_41a").expect("a valid attempt id"),
            incarnation: FIRST_INCARNATION,
            status,
            finished_at_ms: FINISHED_AT_MS,
        }
    }

    fn files_in(directory: &ScratchAttemptDirectory) -> Vec<PathBuf> {
        std::fs::read_dir(directory.path())
            .expect("the attempt directory is readable")
            .map(|entry| entry.expect("directory entries are readable").path())
            .collect()
    }

    #[test]
    fn a_written_exit_record_reads_back_unchanged() {
        let directory: ScratchAttemptDirectory = an_empty_attempt_directory();
        let succeeded: ExitRecord =
            attempt_41a_exited_with(ExitStatus::Code(ExitStatus::SUCCESS_CODE));

        let path: PathBuf =
            write_exit_record(directory.path(), &succeeded).expect("write succeeds");

        assert_eq!(
            read_exit_record(&path).expect("the record reads back"),
            succeeded,
            "the record on disk is exactly what the keeper observed"
        );
    }

    #[test]
    fn writing_an_exit_record_leaves_no_temporary_file_behind() {
        let directory: ScratchAttemptDirectory = an_empty_attempt_directory();

        let path: PathBuf = write_exit_record(
            directory.path(),
            &attempt_41a_exited_with(ExitStatus::Terminated),
        )
        .expect("write succeeds");

        assert_eq!(
            files_in(&directory),
            [path],
            "only the published record remains; the temporary file is cleaned up"
        );
    }

    #[test]
    fn an_existing_exit_record_is_never_overwritten() {
        let directory: ScratchAttemptDirectory = an_empty_attempt_directory();
        write_exit_record(
            directory.path(),
            &attempt_41a_exited_with(ExitStatus::Terminated),
        )
        .expect("the first write succeeds");

        let second = write_exit_record(
            directory.path(),
            &attempt_41a_exited_with(ExitStatus::Code(AGENT_FAILURE_CODE)),
        );

        assert!(
            matches!(second, Err(DurableError::AlreadyExists(_))),
            "proof of exit is written once and never replaced, got {second:?}"
        );
    }

    #[test]
    fn the_existing_record_keeps_its_original_contents_after_a_refused_write() {
        let directory: ScratchAttemptDirectory = an_empty_attempt_directory();
        let original: ExitRecord = attempt_41a_exited_with(ExitStatus::Terminated);
        let path: PathBuf =
            write_exit_record(directory.path(), &original).expect("the first write succeeds");

        let _refused = write_exit_record(
            directory.path(),
            &attempt_41a_exited_with(ExitStatus::Code(AGENT_FAILURE_CODE)),
        );

        assert_eq!(
            read_exit_record(&path).expect("the record still reads"),
            original,
            "a refused write must not touch the first record"
        );
    }
}
