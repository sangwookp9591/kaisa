//! Home reads existing state only; opening it never initializes a ledger.
use std::collections::BTreeMap;
use std::io::{self, Write};

use kaisa_domain::{EventKind, Sequence};
use kaisa_ledger::EventStore;

pub(crate) fn run(out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    let state = match crate::doctor::state_directory() {
        Ok(state) => state,
        Err(error) => {
            writeln!(
                err,
                "kaisa: {error}; set KAISA_STATE_DIR to a writable directory"
            )?;
            return Ok(crate::EXIT_FAILURE);
        }
    };
    let database = state.join("kaisa.db");
    if !database.exists() {
        writeln!(out, "Welcome to Kaisa.\n")?;
        let mut diagnosis = Vec::new();
        crate::doctor::diagnose(&mut diagnosis)?;
        // Home keeps the status and fix lines; doctor also lists every detected command.
        for line in String::from_utf8_lossy(&diagnosis).lines() {
            if !line.starts_with("Profile:") && !line.starts_with("  ") {
                writeln!(out, "{line}")?;
            }
        }
        writeln!(out, "Details: kaisa doctor")?;
        writeln!(
            out,
            "\nHow Kaisa will work for you:\n  Claude Code develops and reviews by default; --agent codex uses Codex to develop.\n  Work happens in separate copies of your repository; checks run in a sandbox.\n  Kaisa asks only when a person must decide.\n\nTry it now: kaisa \"fix the flaky date test\""
        )?;
    } else {
        let ledger = match EventStore::open_read_only(&database) {
            Ok(ledger) => ledger,
            Err(error) => {
                writeln!(err, "kaisa: the ledger cannot be read: {error}")?;
                return Ok(crate::EXIT_FAILURE);
            }
        };
        let read = ledger
            .read_from(Sequence::FIRST)
            .and_then(|events| Ok((events, ledger.attempts_from_log()?)));
        let (events, attempts) = match read {
            Ok(read) => read,
            Err(error) => {
                writeln!(err, "kaisa: the ledger cannot be read: {error}")?;
                return Ok(crate::EXIT_FAILURE);
            }
        };
        writeln!(out, "Kaisa — recent work")?;
        let mut missions = BTreeMap::<String, String>::new();
        let mut goals = BTreeMap::<String, (u64, String, String, String)>::new();
        for chained in events {
            let event = chained.recorded.event;
            let payload: serde_json::Value =
                serde_json::from_str(&event.payload).unwrap_or_default();
            if event.kind == EventKind::MissionCreated {
                if let Some(goal) = payload.get("goal").and_then(serde_json::Value::as_str) {
                    missions.insert(event.subject, goal.to_owned());
                }
                continue;
            }
            if event.kind == EventKind::RunStarted {
                // Verified parent runs carry run_id; developer/reviewer attempt runs do not.
                // Legacy synthetic records may carry goal directly.
                if payload.get("run_id").is_none() && payload.get("goal").is_none() {
                    continue;
                }
                let goal = payload
                    .get("goal")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| {
                        payload
                            .get("mission")
                            .and_then(serde_json::Value::as_str)
                            .and_then(|mission| missions.get(mission).map(String::as_str))
                    })
                    .unwrap_or("goal");
                goals.insert(
                    event.subject,
                    (
                        chained.recorded.sequence.value(),
                        goal.to_owned(),
                        "started".to_owned(),
                        String::new(),
                    ),
                );
            } else if event.kind == EventKind::GateDecided {
                let run = payload
                    .get("run_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&event.subject);
                let goal = goals.entry(run.to_owned()).or_insert_with(|| {
                    (
                        chained.recorded.sequence.value(),
                        payload
                            .get("goal")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("goal")
                            .to_owned(),
                        String::new(),
                        String::new(),
                    )
                });
                goal.0 = chained.recorded.sequence.value();
                goal.2 = payload
                    .get("decision")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned();
                if goal.2 == "Deliver"
                    && let Some(branch) = payload["result_branch"].as_str()
                {
                    goal.2.push_str(&format!(
                        "; Result: {branch}; retrieve with git merge --ff-only {branch}"
                    ));
                }
                goal.3 = payload
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
            }
        }
        let mut recent = goals.iter().collect::<Vec<_>>();
        recent.sort_by_key(|(_, goal)| std::cmp::Reverse(goal.0));
        for (run, (_, goal, decision, _)) in recent.into_iter().take(5) {
            writeln!(out, "  {run}: {} — {}", plain(goal), plain(decision))?;
        }
        for attempt in attempts.iter().rev().take(3) {
            writeln!(
                out,
                "  {}: {} / {}",
                attempt.attempt,
                attempt.execution.code(),
                attempt.cleanup.code()
            )?;
        }
        let waiting = goals
            .iter()
            .filter(|(_, goal)| goal.2 == "NeedsPerson")
            .collect::<Vec<_>>();
        if waiting.is_empty() {
            writeln!(out, "Nothing waiting for you.")?;
        }
        for (run, (_, goal, _, reason)) in waiting {
            writeln!(
                out,
                "Waiting for you: {run}: {} — {}",
                plain(goal),
                plain(reason)
            )?;
        }
    }
    writeln!(
        out,
        "\nDaily commands:\n  kaisa \"<goal>\"  run a goal\n  kaisa watch     see attempts\n  kaisa review    review results (planned; use the result branch instructions today)\nSetup: kaisa doctor · kaisa init · kaisa --help"
    )?;
    Ok(crate::EXIT_SUCCESS)
}

fn plain(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(240)
        .collect()
}
