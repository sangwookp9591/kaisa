//! Deterministic DAG admission. Waves drain before advancing, so replay does not depend on
//! process completion races. A failed task never satisfies a dependency (ADR-0028/0029).

use std::collections::BTreeSet;
use std::fmt;

pub use kaisa_domain::OnFailure;
use kaisa_domain::{Plan, PlanError, PlannedTask, TaskId};

/// Admission failed before any worker was started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleError {
    /// The DAG or declared ownership is invalid.
    InvalidPlan(PlanError),
    /// A mission needs at least one execution slot.
    NoSlots,
    /// Runtime ownership disagrees with dependency readiness.
    Overlap(TaskId, TaskId),
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPlan(error) => write!(f, "invalid schedule: {error}"),
            Self::NoSlots => f.write_str("max_parallel must be positive"),
            Self::Overlap(first, second) => write!(f, "overlapping write sets: {first}, {second}"),
        }
    }
}

impl std::error::Error for ScheduleError {}

/// Run-local DAG progress. Ledger history from other missions never satisfies dependencies.
pub struct Scheduler {
    plan: Plan,
    max_parallel: usize,
    on_failure: OnFailure,
    finished: BTreeSet<TaskId>,
    failed: BTreeSet<TaskId>,
    running: BTreeSet<TaskId>,
}

impl Scheduler {
    /// Revalidates a plan, including unknown dependencies, cycles and actual write sets.
    ///
    /// # Errors
    /// Returns a typed validation error without starting anything.
    pub fn new(
        plan: &Plan,
        max_parallel: usize,
        on_failure: OnFailure,
    ) -> Result<Self, ScheduleError> {
        Self::from_tasks(plan.tasks().to_vec(), max_parallel, on_failure)
    }

    /// Builds a DAG from declared tasks, checking the same invariants as a validated plan.
    ///
    /// # Errors
    /// Returns a typed validation error without starting anything.
    pub fn from_tasks(
        tasks: Vec<PlannedTask>,
        max_parallel: usize,
        on_failure: OnFailure,
    ) -> Result<Self, ScheduleError> {
        if max_parallel == 0 {
            return Err(ScheduleError::NoSlots);
        }
        Ok(Self {
            plan: Plan::new(tasks).map_err(ScheduleError::InvalidPlan)?,
            max_parallel,
            on_failure,
            finished: BTreeSet::new(),
            failed: BTreeSet::new(),
            running: BTreeSet::new(),
        })
    }

    /// Admits the next deterministic wave in plan order, rechecking every pair's ownership.
    /// A wave must drain before another is admitted.
    ///
    /// # Errors
    /// Returns an overlap if actual ownership would allow concurrent writers.
    pub fn next_wave(&mut self) -> Result<Vec<PlannedTask>, ScheduleError> {
        if !self.running.is_empty()
            || (self.on_failure == OnFailure::Stop && !self.failed.is_empty())
        {
            return Ok(Vec::new());
        }
        let ready: Vec<_> = self
            .plan
            .tasks()
            .iter()
            .filter(|task| {
                !self.finished.contains(&task.id)
                    && !self.failed.contains(&task.id)
                    && task.depends_on.iter().all(|id| self.finished.contains(id))
            })
            .take(self.max_parallel)
            .cloned()
            .collect();
        ensure_disjoint(&ready)?;
        self.running
            .extend(ready.iter().map(|task| task.id.clone()));
        Ok(ready)
    }

    /// Settles a drained worker; failure is terminal and never releases its dependents.
    pub fn finish(&mut self, task: TaskId, succeeded: bool) {
        self.running.remove(&task);
        if succeeded {
            self.finished.insert(task);
        } else {
            self.failed.insert(task);
        }
    }
}

fn ensure_disjoint(tasks: &[PlannedTask]) -> Result<(), ScheduleError> {
    for (index, first) in tasks.iter().enumerate() {
        for second in &tasks[index + 1..] {
            if first.write_set.overlaps(&second.write_set) {
                return Err(ScheduleError::Overlap(first.id.clone(), second.id.clone()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{PlanError, PlannedTask, RepoPath, Role, TaskId, WriteSet};

    use super::{OnFailure, ScheduleError, Scheduler};
    const ONE_SLOT: usize = 1;
    const TWO_SLOTS: usize = 2;
    fn id(name: &str) -> TaskId {
        TaskId::try_new(name).expect("valid task identity")
    }
    fn writer(name: &str, path: &str, dependencies: &[&str]) -> PlannedTask {
        PlannedTask {
            id: id(name),
            role: Role::Developer,
            write_set: WriteSet::new(vec![RepoPath::try_new(path).expect("valid owned path")]),
            depends_on: dependencies.iter().map(|name| id(name)).collect(),
        }
    }
    #[test]
    fn a_cycle_is_rejected_before_any_worker_is_admitted() {
        let tasks = vec![
            writer("task_a", "src/a", &["task_b"]),
            writer("task_b", "src/b", &["task_a"]),
        ];
        assert!(
            matches!(
                Scheduler::from_tasks(tasks, TWO_SLOTS, OnFailure::Stop),
                Err(ScheduleError::InvalidPlan(PlanError::Cycle(_)))
            ),
            "a cyclic graph cannot run and must fail with a typed error"
        );
    }
    #[test]
    fn an_unknown_dependency_is_rejected_before_any_worker_is_admitted() {
        let tasks = vec![writer("task_a", "src/a", &["task_missing"])];
        assert!(
            matches!(
                Scheduler::from_tasks(tasks, ONE_SLOT, OnFailure::Stop),
                Err(ScheduleError::InvalidPlan(
                    PlanError::UnknownDependency { .. }
                ))
            ),
            "missing dependencies are typed admission failures"
        );
    }
    #[test]
    fn overlapping_actual_write_sets_are_rejected_before_parallel_admission() {
        let tasks = vec![
            writer("task_a", "src/auth", &[]),
            writer("task_b", "src/auth/login.rs", &[]),
        ];
        assert!(
            matches!(
                Scheduler::from_tasks(tasks, TWO_SLOTS, OnFailure::Stop),
                Err(ScheduleError::InvalidPlan(PlanError::ParallelOverlap(_, _)))
            ),
            "actual declared paths must be revalidated before any process starts"
        );
    }
    #[test]
    fn a_runtime_ownership_overlap_is_refused_even_after_initial_validation() {
        let wave = vec![
            writer("task_a", "src/auth", &[]),
            writer("task_b", "src/auth/login.rs", &[]),
        ];
        assert_eq!(
            super::ensure_disjoint(&wave),
            Err(ScheduleError::Overlap(id("task_a"), id("task_b"))),
            "admission rechecks the actual wave's declared paths independently of DAG validation"
        );
    }
    #[test]
    fn a_task_never_starts_until_all_parents_succeeded() {
        let mut scheduler = Scheduler::from_tasks(
            vec![
                writer("task_a", "src/a", &[]),
                writer("task_b", "src/b", &[]),
                writer("task_join", "src/join", &["task_a", "task_b"]),
            ],
            TWO_SLOTS,
            OnFailure::ContinueIndependent,
        )
        .expect("valid plan");
        let wave = scheduler.next_wave().expect("parents admitted");
        scheduler.finish(wave[0].id.clone(), true);
        assert!(
            scheduler
                .next_wave()
                .expect("wave still running")
                .is_empty(),
            "one parent is insufficient to release the join"
        );
        scheduler.finish(wave[1].id.clone(), false);
        assert!(
            scheduler
                .next_wave()
                .expect("failed parent blocks join")
                .is_empty(),
            "a failed parent never counts as successful completion"
        );
    }
}
