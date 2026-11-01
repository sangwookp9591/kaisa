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
    let read = EventStore::open_read_only(&database).and_then(|ledger| {
        // Watching only reads: the attempts are folded in memory, never written back (NFR-03).
        let rows: Vec<AttemptRow> = ledger.attempts_from_log()?;
        let usage: Vec<Option<kaisa_ledger::AttemptUsage>> = rows
            .iter()
            .map(|row| ledger.usage_for(&row.attempt))
            .collect::<Result<_, _>>()?;
        Ok((rows, usage, ledger.task_progress()?))
    });
    let (rows, usage, progress) = match read {
        Ok(read) => read,
        Err(error) => {
            writeln!(err, "kaisa: the ledger cannot be read: {error}")?;
            return Ok(crate::EXIT_FAILURE);
        }
    };
    if !progress.running.is_empty() {
        writeln!(
            out,
            "Running mission tasks: {}",
            progress
                .running
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )?;
    }
    let attempt_width = rows
        .iter()
        .map(|row| row.attempt.as_str().len())
        .max()
        .unwrap_or_default()
        .max("ATTEMPT".len());
    let task_width = rows
        .iter()
        .map(|row| row.task.as_str().len())
        .max()
        .unwrap_or_default()
        .max("TASK".len());
    writeln!(
        out,
        "{:<attempt_width$} {:<task_width$} {:<16} {:<12} {:<14} COST",
        "ATTEMPT", "TASK", "EXECUTION", "CLEANUP", "TOKENS"
    )?;
    let total: f64 = usage
        .iter()
        .flatten()
        .filter_map(|usage| usage.cost_usd)
        .fold(0.0, |total, cost| total + cost);
    let excluded = usage
        .iter()
        .filter(|usage| usage.as_ref().is_none_or(|usage| usage.cost_usd.is_none()))
        .count();
    for (row, usage) in rows.iter().zip(usage) {
        let (tokens, cost) = match usage {
            Some(usage) => (
                format!("{}/{}", usage.input_tokens, usage.output_tokens),
                usage.cost_usd.map_or_else(
                    || "n/a (provider gives no cost; excluded from total)".to_owned(),
                    |cost| format!("${cost:.4}"),
                ),
            ),
            None => (
                "-".to_owned(),
                "n/a (usage not reported; excluded from total)".to_owned(),
            ),
        };
        writeln!(
            out,
            "{:<attempt_width$} {:<task_width$} {:<16} {:<12} {:<14} {}",
            row.attempt.as_str(),
            row.task.as_str(),
            row.execution.code(),
            row.cleanup.code(),
            tokens,
            cost
        )?;
    }
    writeln!(
        out,
        "Reported cost total: ${total:.4}; {excluded} attempt(s) with unavailable cost excluded"
    )?;
    Ok(crate::EXIT_SUCCESS)
}
