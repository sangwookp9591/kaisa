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
    let read = EventStore::open(&database).and_then(|mut ledger| {
        let rows: Vec<AttemptRow> = ledger.all_attempts()?;
        let usage: Vec<Option<kaisa_ledger::AttemptUsage>> = rows
            .iter()
            .map(|row| ledger.usage_for(&row.attempt))
            .collect::<Result<_, _>>()?;
        Ok((rows, usage))
    });
    let (rows, usage) = match read {
        Ok(read) => read,
        Err(error) => {
            writeln!(err, "kaisa: the ledger cannot be read: {error}")?;
            return Ok(crate::EXIT_FAILURE);
        }
    };
    writeln!(
        out,
        "{:<24} {:<24} {:<16} {:<12} {:<14} COST",
        "ATTEMPT", "TASK", "EXECUTION", "CLEANUP", "TOKENS"
    )?;
    for (row, usage) in rows.iter().zip(usage) {
        let (tokens, cost) = match usage {
            Some(usage) => (
                format!("{}/{}", usage.input_tokens, usage.output_tokens),
                usage
                    .cost_usd
                    .map_or_else(|| "-".to_owned(), |cost| format!("${cost:.4}")),
            ),
            None => ("-".to_owned(), "-".to_owned()),
        };
        writeln!(
            out,
            "{:<24} {:<24} {:<16} {:<12} {:<14} {}",
            row.attempt.as_str(),
            row.task.as_str(),
            row.execution.code(),
            row.cleanup.code(),
            tokens,
            cost
        )?;
    }
    Ok(crate::EXIT_SUCCESS)
}
