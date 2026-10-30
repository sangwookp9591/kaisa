//! `kaisa watch`: every attempt and where it stands, read from the ledger (T30a). The ledger
//! is the authority; this view never infers state on its own.

use std::io::{self, Write};

use kaisa_ledger::{AttemptRow, EventStore};

/// Prints every attempt's task, execution and cleanup state, oldest first.
pub(crate) fn run(out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    let state = match kaisa_sys::state_location(|name| std::env::var_os(name)) {
        Ok(location) => location.directory,
        Err(error) => {
            writeln!(err, "kaisa: {error}")?;
            return Ok(crate::EXIT_FAILURE);
        }
    };
    let database = state.join("kaisa.db");
    if !database.exists() {
        writeln!(out, "nothing has run yet")?;
        return Ok(crate::EXIT_SUCCESS);
    }
    let rows: Vec<AttemptRow> =
        match EventStore::open(&database).and_then(|mut ledger| ledger.all_attempts()) {
            Ok(rows) => rows,
            Err(error) => {
                writeln!(err, "kaisa: the ledger cannot be read: {error}")?;
                return Ok(crate::EXIT_FAILURE);
            }
        };
    writeln!(
        out,
        "{:<24} {:<24} {:<16} CLEANUP",
        "ATTEMPT", "TASK", "EXECUTION"
    )?;
    for row in rows {
        writeln!(
            out,
            "{:<24} {:<24} {:<16} {}",
            row.attempt.as_str(),
            row.task.as_str(),
            row.execution.code(),
            row.cleanup.code()
        )?;
    }
    Ok(crate::EXIT_SUCCESS)
}
