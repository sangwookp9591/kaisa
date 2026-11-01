//! Replayable joins of route, attempt, usage and verification receipts.

use std::collections::BTreeMap;

use kaisa_domain::{EventKind, Sequence};
use serde_json::Value;

use super::{EventStore, StoreError};

/// One routed attempt, with unknown measurements preserved explicitly.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingObservation {
    /// Attempt identifier.
    pub attempt: String,
    /// Logical task identifier shared by revisions.
    pub task: String,
    /// Recorded route, including agent, model, effort and task class.
    pub route: Value,
    /// Delivery gate result; absent until verification finishes.
    pub delivered: Option<bool>,
    /// A verification or process failure was observed.
    pub failed: bool,
    /// Recorded wall time in milliseconds, never inferred from event sequence.
    pub latency_ms: Option<u64>,
    /// Last usage receipt, if available.
    pub usage: Option<super::AttemptUsage>,
}

impl EventStore {
    /// Joins existing receipts deterministically, without writing a projection.
    /// Legacy attempts without route identity cannot be attributed to a model.
    ///
    /// # Errors
    /// Returns an error for unreadable events or invalid JSON in relevant receipts.
    pub fn routing_observations(&self) -> Result<Vec<RoutingObservation>, StoreError> {
        let mut routes = BTreeMap::new();
        let mut attempts: Vec<RoutingObservation> = Vec::new();
        let mut trees: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for chained in self.read_from(Sequence::FIRST)? {
            let recorded = chained.recorded;
            let event = recorded.event;
            if !matches!(
                event.kind,
                EventKind::RouteDecided
                    | EventKind::AttemptAdmitted
                    | EventKind::AttemptStateChanged
                    | EventKind::UsageRecorded
                    | EventKind::CandidateFrozen
                    | EventKind::ReviewVerdict
                    | EventKind::CheckEvidence
                    | EventKind::GateDecided
            ) {
                continue;
            }
            let payload: Value =
                serde_json::from_str(&event.payload).map_err(|_| StoreError::CorruptPayload {
                    stored_sequence: super::rows::stored_sequence(recorded.sequence),
                })?;
            match event.kind {
                EventKind::RouteDecided => {
                    routes.insert(event.subject.clone(), payload.clone());
                    // The spawn path records its route after admission, before process start.
                    for attempt in attempts.iter_mut().filter(|a| a.task == event.subject) {
                        attempt.route = payload.clone();
                        attempt.task = payload["routing_task"]
                            .as_str()
                            .unwrap_or(&event.subject)
                            .to_owned();
                    }
                }
                EventKind::AttemptAdmitted => {
                    if let Some(task) = payload["task"].as_str() {
                        let route = routes.get(task).cloned().unwrap_or(Value::Null);
                        attempts.push(RoutingObservation {
                            attempt: event.subject,
                            task: route["routing_task"].as_str().unwrap_or(task).to_owned(),
                            route,
                            delivered: None,
                            failed: false,
                            latency_ms: None,
                            usage: None,
                        });
                    }
                }
                EventKind::CandidateFrozen => {
                    if let Some(attempt) = payload["developer_attempt"].as_str() {
                        trees.insert(tree_key(&payload, &event.subject), vec![attempt.to_owned()]);
                    }
                }
                EventKind::ReviewVerdict => {
                    trees
                        .entry(tree_key(&payload, ""))
                        .or_default()
                        .push(event.subject.clone());
                    if payload["valid"] == false || payload["approve"] == false {
                        mark_failed(&mut attempts, &event.subject);
                    }
                }
                EventKind::CheckEvidence => {
                    if payload["exit"]["code"] != 0 {
                        for id in trees
                            .get(&tree_key(&payload, &event.subject))
                            .into_iter()
                            .flatten()
                        {
                            mark_failed(&mut attempts, id);
                        }
                    }
                }
                EventKind::GateDecided => {
                    let delivered = payload["decision"] == "Deliver";
                    let mut ids = trees
                        .get(&tree_key(&payload, &event.subject))
                        .cloned()
                        .unwrap_or_default();
                    if let Some(id) = payload["attempt"].as_str() {
                        ids.push(id.to_owned());
                    }
                    for attempt in attempts.iter_mut().filter(|a| ids.contains(&a.attempt)) {
                        attempt.delivered = Some(delivered);
                        attempt.failed |= !delivered;
                    }
                }
                EventKind::AttemptStateChanged | EventKind::UsageRecorded => {
                    if let Some(attempt) = attempts.iter_mut().find(|a| a.attempt == event.subject)
                    {
                        if event.kind == EventKind::UsageRecorded {
                            attempt.usage = payload["input_tokens"]
                                .as_u64()
                                .zip(payload["output_tokens"].as_u64())
                                .map(|(input_tokens, output_tokens)| super::AttemptUsage {
                                    input_tokens,
                                    output_tokens,
                                    cost_usd: payload["cost_usd"]
                                        .as_f64()
                                        .filter(|v| v.is_finite() && *v >= 0.0),
                                });
                        } else {
                            if let Some(latency) = payload["elapsed_ms"].as_u64() {
                                attempt.latency_ms = Some(latency);
                            }
                            if let Some(execution) = payload["execution"].as_str()
                                && execution.starts_with("exited:")
                                && execution != "exited:0"
                            {
                                attempt.failed = true;
                                attempt.delivered = Some(false);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(attempts
            .into_iter()
            .filter(|a| !a.route.is_null())
            .collect())
    }

    /// The most recent recorded explanation for a task, or the last route overall.
    ///
    /// # Errors
    /// Returns an error if the event log cannot be read or the explanation is invalid JSON.
    pub fn route_explanation(&self, task: &str) -> Result<Option<Value>, StoreError> {
        let mut last = None;
        for chained in self.read_from(Sequence::FIRST)? {
            let recorded = chained.recorded;
            if recorded.event.kind != EventKind::RouteDecided {
                continue;
            }
            let payload: Value = serde_json::from_str(&recorded.event.payload).map_err(|_| {
                StoreError::CorruptPayload {
                    stored_sequence: super::rows::stored_sequence(recorded.sequence),
                }
            })?;
            if task == "last" || recorded.event.subject == task || payload["routing_task"] == task {
                last = Some(payload);
            }
        }
        Ok(last)
    }
}

fn tree_key(payload: &Value, fallback: &str) -> String {
    format!(
        "{}:{}",
        payload["run_id"].as_str().unwrap_or(""),
        payload["candidate_tree"].as_str().unwrap_or(fallback)
    )
}

fn mark_failed(attempts: &mut [RoutingObservation], id: &str) {
    if let Some(attempt) = attempts.iter_mut().find(|a| a.attempt == id) {
        attempt.failed = true;
    }
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{Event, EventKind};
    use serde_json::{Value, json};

    use super::EventStore;

    const ATTEMPT: &str = "att_route";
    const TASK: &str = "task_route";
    const WALL_MS: u64 = 250;
    const COST_USD: f64 = 0.25;

    fn append(ledger: &mut EventStore, kind: EventKind, subject: &str, payload: Value) {
        ledger
            .append(Event {
                kind,
                subject: subject.to_owned(),
                payload_version: 1,
                payload: payload.to_string(),
            })
            .expect("valid receipt");
    }

    fn routed_attempt(ledger: &mut EventStore) {
        append(
            ledger,
            EventKind::AttemptAdmitted,
            ATTEMPT,
            json!({"task":TASK}),
        );
        append(
            ledger,
            EventKind::RouteDecided,
            TASK,
            json!({"agent":"claude","model":"haiku","effort":"medium","task_class":"T1:Developer","routing_task":"task_logical","strategy":"measured"}),
        );
        append(
            ledger,
            EventKind::AttemptStateChanged,
            ATTEMPT,
            json!({"execution":"exited:0","elapsed_ms":WALL_MS}),
        );
        append(
            ledger,
            EventKind::UsageRecorded,
            ATTEMPT,
            json!({"input_tokens":100,"output_tokens":50,"cost_usd":COST_USD}),
        );
        append(
            ledger,
            EventKind::CandidateFrozen,
            "tree",
            json!({"run_id":"run_route","candidate_tree":"tree","developer_attempt":ATTEMPT}),
        );
    }

    #[test]
    fn a_failed_verification_updates_the_actual_routed_attempt_and_replays_identically() {
        let directory =
            std::env::temp_dir().join(format!("kaisa-routing-replay-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("temp directory");
        let database = directory.join("ledger.db");
        let _ = std::fs::remove_file(&database);
        let mut ledger = EventStore::open(&database).expect("opens");
        routed_attempt(&mut ledger);
        append(
            &mut ledger,
            EventKind::CheckEvidence,
            "tree",
            json!({"run_id":"run_route","candidate_tree":"tree","exit":{"code":1}}),
        );
        append(
            &mut ledger,
            EventKind::GateDecided,
            "tree",
            json!({"run_id":"run_route","candidate_tree":"tree","decision":"revision"}),
        );
        let before = ledger.routing_observations().expect("joined receipts");
        let attempt = before.first().expect("one attributed attempt");
        assert!(
            attempt.failed && attempt.delivered == Some(false),
            "verification failure is charged to the model which produced the candidate"
        );
        assert_eq!(
            attempt.latency_ms,
            Some(WALL_MS),
            "latency comes from recorded wall time"
        );
        assert_eq!(
            attempt.usage.as_ref().and_then(|u| u.cost_usd),
            Some(COST_USD),
            "cost is joined from the attempt receipt"
        );
        assert_eq!(
            attempt.task, "task_logical",
            "revisions retain logical task identity"
        );
        ledger.rebuild_attempts().expect("rebuild projection");
        assert_eq!(
            ledger.routing_observations().expect("replay"),
            before,
            "routing never depends on a mutable projection"
        );
        drop(ledger);
        let reopened = EventStore::open_read_only(&database).expect("read only replay");
        assert_eq!(
            reopened.routing_observations().expect("replay"),
            before,
            "restart replays the same observations in the same order"
        );
        drop(reopened);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_successful_process_without_a_delivery_gate_has_unknown_delivery() {
        let directory =
            std::env::temp_dir().join(format!("kaisa-routing-pending-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("temp directory");
        let database = directory.join("ledger.db");
        let _ = std::fs::remove_file(&database);
        let mut ledger = EventStore::open(&database).expect("opens");
        routed_attempt(&mut ledger);
        let observations = ledger.routing_observations().expect("joined receipts");
        assert_eq!(
            observations.first().expect("one pending attempt").delivered,
            None,
            "process exit success alone is not verified delivery"
        );
        drop(ledger);
        let _ = std::fs::remove_dir_all(directory);
    }
}
