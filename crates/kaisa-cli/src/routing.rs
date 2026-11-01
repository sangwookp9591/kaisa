//! Read-only routing explanations and measured cells.

use std::io::{self, Write};
use std::path::Path;

use kaisa_ledger::EventStore;

pub(crate) fn run(
    args: &[String],
    stats: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<u8> {
    let valid = if stats {
        matches!(args, [name] if name == "routing")
    } else {
        matches!(args, [name, _] if name == "explain")
    };
    if !valid {
        writeln!(
            err,
            "usage: kaisa route explain <task|last>\n       kaisa stats routing"
        )?;
        return Ok(crate::EXIT_USAGE);
    }
    let state = kaisa_sys::state_location(|name| std::env::var_os(name))
        .map_err(io::Error::other)?
        .directory;
    match run_in_state(args, stats, &state, out) {
        Ok(()) => Ok(crate::EXIT_SUCCESS),
        Err(error) => {
            writeln!(err, "kaisa routing: {error}")?;
            Ok(crate::EXIT_FAILURE)
        }
    }
}

fn run_in_state(args: &[String], stats: bool, state: &Path, out: &mut dyn Write) -> io::Result<()> {
    let database = state.join("kaisa.db");
    if !database.exists() {
        return writeln!(out, "no recorded routing decisions (measurements unknown)");
    }
    let ledger = EventStore::open_read_only(&database).map_err(io::Error::other)?;
    if !stats {
        let explanation = ledger
            .route_explanation(&args[1])
            .map_err(io::Error::other)?;
        return match explanation {
            Some(value) => writeln!(
                out,
                "{}",
                serde_json::to_string_pretty(&value).map_err(io::Error::other)?
            ),
            None => writeln!(out, "no recorded route for {}", args[1]),
        };
    }
    let policy = crate::policy::load(state)?;
    let observations = ledger.routing_observations().map_err(io::Error::other)?;
    writeln!(
        out,
        "AGENT MODEL EFFORT CLASS SAMPLES ATTEMPTS FIRST-PASS DELIVERED FAILURE REVISIONS P50(ms) P90(ms) MEAN-COST"
    )?;
    for cell in kaisa_daemon::routing_cells(&observations, policy.routing.window) {
        let cost = cell
            .mean_cost_usd
            .map(|v| format!("${v:.6}"))
            .or_else(|| {
                cell.mean_tokens
                    .map(|v| format!("{v:.1} tokens (USD unknown)"))
            })
            .unwrap_or_else(|| "unknown".to_owned());
        let latency = |v: Option<u64>| v.map_or_else(|| "unknown".to_owned(), |v| v.to_string());
        writeln!(
            out,
            "{} {} {} {} {} {} {:.3} {:.3} {:.3} {} {} {} {}",
            cell.agent,
            cell.model,
            cell.effort,
            cell.task_class,
            cell.samples,
            cell.attempts,
            cell.first_pass_rate,
            cell.delivered_rate,
            cell.failure_rate,
            cell.revisions,
            latency(cell.p50_ms),
            latency(cell.p90_ms),
            cost
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::run_in_state;

    #[test]
    fn an_absent_ledger_reports_unknown_measurements_without_creating_state() {
        let absent =
            std::env::temp_dir().join(format!("kaisa-routing-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&absent);
        let mut output = Vec::new();
        run_in_state(&["routing".to_owned()], true, &absent, &mut output).expect("read only stats");
        assert!(
            String::from_utf8(output).expect("UTF8").contains("unknown"),
            "missing history must not imply zero cost or perfect accuracy"
        );
        assert!(
            !absent.exists(),
            "a read only stats command creates no state"
        );
    }
    #[test]
    fn recorded_explanations_and_unknown_cost_cells_are_printed_without_rerouting() {
        use kaisa_domain::{Event, EventKind};
        use serde_json::json;
        let state =
            std::env::temp_dir().join(format!("kaisa-routing-table-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(&state).expect("isolated state");
        let mut ledger = kaisa_ledger::EventStore::open(&state.join("kaisa.db")).expect("ledger");
        for (kind, subject, payload) in [
            (
                EventKind::RouteDecided,
                "task_stats",
                json!({"agent":"claude","model":"haiku","effort":"medium","strategy":"cold_start","task_class":"T1:Developer","candidates":[{"excluded_by":"insufficient_samples"}]}),
            ),
            (
                EventKind::AttemptAdmitted,
                "att_stats",
                json!({"task":"task_stats"}),
            ),
            (
                EventKind::GateDecided,
                "att_stats",
                json!({"attempt":"att_stats","decision":"Deliver"}),
            ),
        ] {
            ledger
                .append(Event {
                    kind,
                    subject: subject.to_owned(),
                    payload_version: 1,
                    payload: payload.to_string(),
                })
                .expect("receipt");
        }
        drop(ledger);
        let mut output = Vec::new();
        run_in_state(&["routing".to_owned()], true, &state, &mut output).expect("stats");
        let table = String::from_utf8(output).expect("UTF8");
        assert!(
            table.contains("haiku") && table.contains("T1:Developer") && table.contains("unknown"),
            "stats print attributable cells while preserving missing measurements: {table}"
        );
        let mut output = Vec::new();
        run_in_state(
            &["explain".to_owned(), "task_stats".to_owned()],
            false,
            &state,
            &mut output,
        )
        .expect("explain");
        let explanation = String::from_utf8(output).expect("UTF8");
        assert!(
            explanation.contains("cold_start")
                && explanation.contains("candidates")
                && explanation.contains("insufficient_samples"),
            "explain prints the recorded strategy and constraints: {explanation}"
        );
        let _ = std::fs::remove_dir_all(state);
    }
}
