//! A mission's plan: tasks with a role, a write set and dependencies (ADR-0028, ADR-0029).
//!
//! Write-set ownership is a scheduler invariant: two tasks that may run at the same time must
//! not be able to change the same files. A plan that breaks it is rejected when it is made, so
//! the scheduler never has to decide which of two writers wins.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::id::TaskId;
use crate::sandbox::Role;
use crate::write_set::WriteSet;

/// One task of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedTask {
    /// The task.
    pub id: TaskId,
    /// Who does it.
    pub role: Role,
    /// The paths it may change.
    pub write_set: WriteSet,
    /// Tasks that must finish before it starts.
    pub depends_on: Vec<TaskId>,
}

/// Why a plan was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// Two tasks share an id.
    DuplicateTask(TaskId),
    /// A task depends on a task that is not in the plan.
    UnknownDependency {
        /// The task with the dependency.
        task: TaskId,
        /// The task it names.
        missing: TaskId,
    },
    /// The dependencies form a cycle through this task, so it could never start.
    Cycle(TaskId),
    /// Two tasks that may run at the same time could change the same files.
    ParallelOverlap(TaskId, TaskId),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateTask(task) => write!(f, "task {task} appears twice"),
            Self::UnknownDependency { task, missing } => {
                write!(
                    f,
                    "task {task} depends on {missing}, which is not in the plan"
                )
            }
            Self::Cycle(task) => write!(f, "task {task} depends on itself through others"),
            Self::ParallelOverlap(first, second) => write!(
                f,
                "tasks {first} and {second} may run at once and could change the same files"
            ),
        }
    }
}

impl std::error::Error for PlanError {}

/// A validated plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    tasks: Vec<PlannedTask>,
}

impl Plan {
    /// Validates `tasks` as a plan.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError`] for a duplicate id, a dependency outside the plan, a cycle, or two
    /// tasks that may run at once with overlapping write sets.
    pub fn new(tasks: Vec<PlannedTask>) -> Result<Self, PlanError> {
        let mut known: BTreeSet<&TaskId> = BTreeSet::new();
        for task in &tasks {
            if !known.insert(&task.id) {
                return Err(PlanError::DuplicateTask(task.id.clone()));
            }
        }
        for task in &tasks {
            if let Some(missing) = task.depends_on.iter().find(|id| !known.contains(id)) {
                return Err(PlanError::UnknownDependency {
                    task: task.id.clone(),
                    missing: missing.clone(),
                });
            }
        }
        let before: BTreeMap<&TaskId, BTreeSet<&TaskId>> = ancestors(&tasks)?;
        for (index, first) in tasks.iter().enumerate() {
            for second in &tasks[index + 1..] {
                let ordered: bool = before[&first.id].contains(&second.id)
                    || before[&second.id].contains(&first.id);
                if !ordered && first.write_set.overlaps(&second.write_set) {
                    return Err(PlanError::ParallelOverlap(
                        first.id.clone(),
                        second.id.clone(),
                    ));
                }
            }
        }
        Ok(Self { tasks })
    }

    /// Tasks that may start now, in plan order: not finished or running, every dependency
    /// finished, and no write-set overlap with a running task or with one picked before it.
    /// The last rule holds even for a plan's ordered tasks, because a dependency is only
    /// "done" once finished, and the scheduler must never run two writers of the same files.
    #[must_use]
    pub fn next_runnable(
        &self,
        finished: &BTreeSet<TaskId>,
        running: &BTreeSet<TaskId>,
    ) -> Vec<&PlannedTask> {
        let mut busy: Vec<&WriteSet> = self
            .tasks
            .iter()
            .filter(|task| running.contains(&task.id))
            .map(|task| &task.write_set)
            .collect();
        let mut ready: Vec<&PlannedTask> = Vec::new();
        for task in &self.tasks {
            let waiting: bool = finished.contains(&task.id)
                || running.contains(&task.id)
                || !task
                    .depends_on
                    .iter()
                    .all(|dependency| finished.contains(dependency))
                || busy.iter().any(|owned| owned.overlaps(&task.write_set));
            if !waiting {
                busy.push(&task.write_set);
                ready.push(task);
            }
        }
        ready
    }

    /// The tasks, in the order they were planned.
    #[must_use]
    pub fn tasks(&self) -> &[PlannedTask] {
        &self.tasks
    }
}

/// Every task's transitive dependencies; fails on a cycle.
fn ancestors(tasks: &[PlannedTask]) -> Result<BTreeMap<&TaskId, BTreeSet<&TaskId>>, PlanError> {
    let direct: BTreeMap<&TaskId, &Vec<TaskId>> = tasks
        .iter()
        .map(|task| (&task.id, &task.depends_on))
        .collect();
    let mut all: BTreeMap<&TaskId, BTreeSet<&TaskId>> = BTreeMap::new();
    for task in tasks {
        let mut seen: BTreeSet<&TaskId> = BTreeSet::new();
        let mut pending: Vec<&TaskId> = task.depends_on.iter().collect();
        while let Some(next) = pending.pop() {
            if next == &task.id {
                return Err(PlanError::Cycle(task.id.clone()));
            }
            if seen.insert(next) {
                pending.extend(direct[next].iter());
            }
        }
        all.insert(&task.id, seen);
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Plan, PlanError, PlannedTask};
    use crate::id::TaskId;
    use crate::repo_path::RepoPath;
    use crate::sandbox::Role;
    use crate::write_set::WriteSet;

    fn task(id: &str) -> TaskId {
        TaskId::try_new(id).expect("a valid task id")
    }

    fn owning(paths: &[&str]) -> WriteSet {
        WriteSet::new(
            paths
                .iter()
                .map(|path| RepoPath::try_new(path).expect("a valid repository path"))
                .collect(),
        )
    }

    fn a_developer(id: &str, paths: &[&str], depends_on: &[&str]) -> PlannedTask {
        PlannedTask {
            id: task(id),
            role: Role::Developer,
            write_set: owning(paths),
            depends_on: depends_on
                .iter()
                .map(|dependency| task(dependency))
                .collect(),
        }
    }

    #[test]
    fn a_plan_with_overlapping_parallel_write_sets_is_rejected() {
        let login_fix = a_developer("task_login", &["src/auth"], &[]);
        let session_fix = a_developer("task_session", &["src/auth/session.rs"], &[]);

        let plan = Plan::new(vec![login_fix, session_fix]);

        assert_eq!(
            plan,
            Err(PlanError::ParallelOverlap(
                task("task_login"),
                task("task_session")
            )),
            "two developers who could run at once must never own the same files"
        );
    }

    #[test]
    fn dependent_tasks_may_share_a_write_set() {
        let login_fix = a_developer("task_login", &["src/auth"], &[]);
        let follow_up = a_developer("task_followup", &["src/auth"], &["task_login"]);

        let plan = Plan::new(vec![login_fix, follow_up]);

        assert!(
            plan.is_ok(),
            "a task that runs only after another may change the same files: {plan:?}"
        );
    }

    #[test]
    fn tasks_ordered_through_a_middle_task_may_share_a_write_set() {
        let schema = a_developer("task_schema", &["src/db"], &[]);
        let api = a_developer("task_api", &["src/api"], &["task_schema"]);
        let migration = a_developer("task_migration", &["src/db"], &["task_api"]);

        let plan = Plan::new(vec![schema, api, migration]);

        assert!(
            plan.is_ok(),
            "ordering through another task is still ordering: {plan:?}"
        );
    }

    #[test]
    fn a_plan_whose_tasks_wait_on_each_other_is_rejected() {
        let first = a_developer("task_first", &["src/a"], &["task_second"]);
        let second = a_developer("task_second", &["src/b"], &["task_first"]);

        let plan = Plan::new(vec![first, second]);

        assert!(
            matches!(plan, Err(PlanError::Cycle(_))),
            "a task that waits on itself could never start: {plan:?}"
        );
    }

    #[test]
    fn a_dependency_outside_the_plan_is_rejected() {
        let orphan = a_developer("task_orphan", &["src/a"], &["task_missing"]);

        let plan = Plan::new(vec![orphan]);

        assert_eq!(
            plan,
            Err(PlanError::UnknownDependency {
                task: task("task_orphan"),
                missing: task("task_missing"),
            }),
            "a task must not wait for something that will never run"
        );
    }

    #[test]
    fn a_task_listed_twice_is_rejected() {
        let once = a_developer("task_login", &["src/a"], &[]);
        let twice = a_developer("task_login", &["src/b"], &[]);

        let plan = Plan::new(vec![once, twice]);

        assert_eq!(
            plan,
            Err(PlanError::DuplicateTask(task("task_login"))),
            "every task has one identity"
        );
    }

    fn ids(tasks: &[&PlannedTask]) -> Vec<TaskId> {
        tasks.iter().map(|task| task.id.clone()).collect()
    }

    fn done(names: &[&str]) -> BTreeSet<TaskId> {
        names.iter().map(|name| task(name)).collect()
    }

    #[test]
    fn a_task_waits_until_its_dependencies_are_done() {
        let plan = Plan::new(vec![
            a_developer("task_schema", &["src/db"], &[]),
            a_developer("task_api", &["src/api"], &["task_schema"]),
        ])
        .expect("a valid plan");

        let at_start = ids(&plan.next_runnable(&done(&[]), &done(&[])));
        let after_schema = ids(&plan.next_runnable(&done(&["task_schema"]), &done(&[])));

        assert_eq!(
            at_start,
            vec![task("task_schema")],
            "the API waits for the schema"
        );
        assert_eq!(
            after_schema,
            vec![task("task_api")],
            "once the schema is finished the API may start"
        );
    }

    #[test]
    fn independent_tasks_start_together() {
        let plan = Plan::new(vec![
            a_developer("task_login", &["src/auth"], &[]),
            a_developer("task_billing", &["src/billing"], &[]),
        ])
        .expect("a valid plan");

        let ready = ids(&plan.next_runnable(&done(&[]), &done(&[])));

        assert_eq!(
            ready,
            vec![task("task_login"), task("task_billing")],
            "tasks owning different files run in parallel, in plan order"
        );
    }

    #[test]
    fn two_tasks_touching_the_same_files_never_run_at_once() {
        let plan = Plan::new(vec![
            a_developer("task_login", &["src/auth"], &[]),
            a_developer("task_review", &["src/auth"], &["task_login"]),
        ])
        .expect("a valid plan");

        let while_login_runs = ids(&plan.next_runnable(&done(&[]), &done(&["task_login"])));

        assert!(
            while_login_runs.is_empty(),
            "nothing else that owns src/auth starts while the login fix runs"
        );
    }

    #[test]
    fn a_finished_or_running_task_is_never_offered_again() {
        let plan = Plan::new(vec![
            a_developer("task_login", &["src/auth"], &[]),
            a_developer("task_billing", &["src/billing"], &[]),
        ])
        .expect("a valid plan");

        let ready = ids(&plan.next_runnable(&done(&["task_login"]), &done(&["task_billing"])));

        assert!(ready.is_empty(), "each task runs once");
    }
}
