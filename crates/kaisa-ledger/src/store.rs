//! The SQLite-backed event store: the single writer of the ledger (architecture §5.4).

use std::fmt;
use std::path::Path;

use rusqlite::Connection;

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
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(f, "ledger database error: {error}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
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
    use super::EventStore;
    use super::test_support::{ScratchLedger, a_ledger_path_that_does_not_exist_yet};

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
}
