//! A pipeline's progress, folded from `task.started` and `task.finished` events (T20c). The
//! scheduler reads only this to decide what runs next: no model call advances a pipeline.

use std::collections::{BTreeMap, BTreeSet};

use kaisa_domain::{EventKind, Sequence, TaskId};

use super::error::StoreError;
use super::event_store::EventStore;

/// Which tasks are running and which are finished, according to the ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskProgress {
    /// Tasks handed to an attempt and not finished yet.
    pub running: BTreeSet<TaskId>,
    /// Tasks that are done.
    pub finished: BTreeSet<TaskId>,
    /// Settled failures, which never satisfy a dependency.
    pub failed: BTreeSet<TaskId>,
}

impl EventStore {
    /// The progress of every task the ledger has seen.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the log cannot be read or names an invalid task.
    pub fn task_progress(&self) -> Result<TaskProgress, StoreError> {
        let mut progress = TaskProgress::default();
        let mut tasks = BTreeMap::new();
        for chained in self.read_from(Sequence::FIRST)? {
            let recorded = chained.recorded;
            let kind: EventKind = recorded.event.kind;
            if !matches!(kind, EventKind::TaskStarted | EventKind::TaskFinished) {
                continue;
            }
            let task = TaskId::try_new(&recorded.event.subject).map_err(|_| {
                StoreError::CorruptPayload {
                    stored_sequence: super::rows::stored_sequence(recorded.sequence),
                }
            })?;
            let detail: serde_json::Value =
                serde_json::from_str(&recorded.event.payload).map_err(|_| {
                    StoreError::CorruptPayload {
                        stored_sequence: super::rows::stored_sequence(recorded.sequence),
                    }
                })?;
            let mission = detail
                .get("mission_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let state = if kind == EventKind::TaskStarted {
                None
            } else {
                Some(match detail.get("succeeded") {
                    None => true, // Legacy task.finished meant success.
                    Some(value) => value.as_bool().ok_or(StoreError::CorruptPayload {
                        stored_sequence: super::rows::stored_sequence(recorded.sequence),
                    })?,
                })
            };
            tasks.insert((mission, task), state);
        }
        for ((_, task), state) in tasks {
            match state {
                None => {
                    progress.running.insert(task);
                }
                Some(true) => {
                    progress.finished.insert(task);
                }
                Some(false) => {
                    progress.failed.insert(task);
                }
            }
        }
        Ok(progress)
    }
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{Event, EventKind, Plan, PlannedTask, RepoPath, Role, TaskId, WriteSet};

    use super::super::event_store::EventStore;
    use super::super::test_support::a_ledger_path_that_does_not_exist_yet;

    /// Payload schema version of the task events in these tests.
    const PAYLOAD_VERSION: u16 = 1;

    fn task(id: &str) -> TaskId {
        TaskId::try_new(id).expect("a valid task id")
    }

    fn owning(path: &str) -> WriteSet {
        WriteSet::new(vec![RepoPath::try_new(path).expect("a valid path")])
    }

    fn record(store: &mut EventStore, kind: EventKind, subject: &str) {
        store
            .append(Event {
                kind,
                subject: subject.to_owned(),
                payload_version: PAYLOAD_VERSION,
                payload: "{}".to_owned(),
            })
            .expect("recorded");
    }

    #[test]
    fn the_pipeline_advances_from_recorded_events_only() {
        let scratch = a_ledger_path_that_does_not_exist_yet();
        let mut store = EventStore::open(&scratch.path()).expect("a new ledger opens");
        let plan = Plan::new(vec![
            PlannedTask {
                id: task("task_develop"),
                role: Role::Developer,
                write_set: owning("src/auth"),
                depends_on: Vec::new(),
            },
            PlannedTask {
                id: task("task_review"),
                role: Role::Reviewer,
                write_set: WriteSet::default(),
                depends_on: vec![task("task_develop")],
            },
        ])
        .expect("a valid plan");
        record(&mut store, EventKind::TaskStarted, "task_develop");
        let while_developing = store.task_progress().expect("readable");
        record(&mut store, EventKind::TaskFinished, "task_develop");
        let after_developing = store.task_progress().expect("readable");

        let next_while_developing: Vec<TaskId> = plan
            .next_runnable(&while_developing.finished, &while_developing.running)
            .iter()
            .map(|planned| planned.id.clone())
            .collect();
        let next_after_developing: Vec<TaskId> = plan
            .next_runnable(&after_developing.finished, &after_developing.running)
            .iter()
            .map(|planned| planned.id.clone())
            .collect();

        assert!(
            next_while_developing.is_empty(),
            "the review waits while the ledger says development is running"
        );
        assert_eq!(
            next_after_developing,
            vec![task("task_review")],
            "the recorded finish alone lets the review start"
        );
    }
    #[test]
    fn a_failed_task_settles_without_satisfying_its_dependencies() {
        let scratch = a_ledger_path_that_does_not_exist_yet();
        let mut store = EventStore::open(&scratch.path()).expect("new ledger");
        record(&mut store, EventKind::TaskStarted, "task_develop");
        store
            .append(Event {
                kind: EventKind::TaskFinished,
                subject: "task_develop".into(),
                payload_version: PAYLOAD_VERSION,
                payload: serde_json::json!({"succeeded":false}).to_string(),
            })
            .expect("failure recorded");
        let progress = store.task_progress().expect("readable progress");
        assert!(progress.running.is_empty(), "a failed task has settled");
        assert!(
            progress.finished.is_empty(),
            "failure is not success for dependency admission"
        );
        assert!(
            progress.failed.contains(&task("task_develop")),
            "the failed identity remains visible"
        );
    }

    #[test]
    fn one_missions_finish_does_not_hide_another_missions_running_task() {
        let scratch = a_ledger_path_that_does_not_exist_yet();
        let mut store = EventStore::open(&scratch.path()).expect("new ledger");
        for (mission, kind) in [
            ("mis_first", EventKind::TaskStarted),
            ("mis_second", EventKind::TaskStarted),
            ("mis_first", EventKind::TaskFinished),
        ] {
            store
                .append(Event {
                    kind,
                    subject: "task_shared".into(),
                    payload_version: PAYLOAD_VERSION,
                    payload: serde_json::json!({"mission_id":mission,"succeeded":true}).to_string(),
                })
                .expect("scoped event");
        }
        assert!(
            store
                .task_progress()
                .expect("progress")
                .running
                .contains(&task("task_shared")),
            "mission-scoped folding retains the still-running owner"
        );
    }
}
