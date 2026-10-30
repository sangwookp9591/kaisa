//! Deterministic task grading from plan, impact and ledger signals (architecture §5.10).

use crate::grade::{Grade, GradeFloor};
use crate::plan::PlannedTask;
use crate::sandbox::Role;

/// More than ten impacted files needs standard reasoning about changes across files.
pub const IMPACTED_FILES_THRESHOLD: usize = 10;

/// More than five writable paths needs standard reasoning about a broader change surface.
pub const WRITE_SET_PATHS_THRESHOLD: usize = 5;

/// Two prior failures allow trying another option before a revision needs a stronger grade.
pub const REVISION_FAILURES_THRESHOLD: u32 = 2;

/// Measured inputs supplied by the caller from the plan, impact set and task ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskSignals {
    /// Number of paths in the task's write set.
    pub paths_in_write_set: usize,
    /// Number of files in the task's impact set.
    pub impacted_files: usize,
    /// Whether the change touches paths classified as security sensitive.
    pub touches_security_paths: bool,
    /// Whether this attempt revises an earlier attempt of the same task.
    pub is_revision: bool,
    /// Number of earlier failures of the same task.
    pub prior_failures: u32,
}

/// One applied grading rule, in evaluation order, for a replayable route decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradeReason {
    /// The role establishes the initial grade.
    RoleBaseline {
        /// Role from the plan.
        role: Role,
        /// Initial grade required by that role.
        grade: Grade,
    },
    /// A security review must never be routed below T3.
    SecurityFloor,
    /// The developer's impact set exceeds [`IMPACTED_FILES_THRESHOLD`], requiring at least T2.
    WideImpact {
        /// Observed number of impacted files.
        impacted_files: usize,
    },
    /// The developer's write set exceeds [`WRITE_SET_PATHS_THRESHOLD`], requiring at least T2.
    WideWriteSet {
        /// Observed number of writable paths.
        paths_in_write_set: usize,
    },
    /// The developer touches security paths, requiring T3.
    SecurityPaths,
    /// A revision meets [`REVISION_FAILURES_THRESHOLD`], escalating once, capped at T3.
    RepeatedFailures {
        /// Observed number of prior task failures.
        prior_failures: u32,
        /// Grade before escalation.
        from: Grade,
        /// Grade after escalation, including an unchanged T3 at the ceiling.
        to: Grade,
    },
}

/// A task's required grade, immutable routing floor and ordered explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GradeDecision {
    /// Required grade after all applicable grading rules.
    pub grade: Grade,
    /// A declared minimum the router must preserve regardless of budget.
    pub floor: Option<GradeFloor>,
    /// Applied rules, starting with the role baseline and ending with any escalation.
    pub reasons: Vec<GradeReason>,
}

/// Grades a task without I/O or model calls, using only its role and supplied measurements.
///
/// Developer breadth and security rules apply before revision escalation. Coordinator work
/// starts at T2 because it decides the plan; security review alone declares a T3 routing floor.
#[must_use]
pub fn grade_task(task: &PlannedTask, signals: &TaskSignals) -> GradeDecision {
    let mut grade: Grade = match task.role {
        Role::Security => Grade::T3,
        Role::Reviewer | Role::Architect | Role::Coordinator => Grade::T2,
        Role::Developer | Role::Tester | Role::Researcher | Role::Documentation => Grade::T1,
    };
    let mut reasons: Vec<GradeReason> = vec![GradeReason::RoleBaseline {
        role: task.role,
        grade,
    }];
    let floor: Option<GradeFloor> = if task.role == Role::Security {
        reasons.push(GradeReason::SecurityFloor);
        Some(GradeFloor::at(Grade::T3))
    } else {
        None
    };

    if task.role == Role::Developer {
        if signals.impacted_files > IMPACTED_FILES_THRESHOLD {
            grade = grade.max(Grade::T2);
            reasons.push(GradeReason::WideImpact {
                impacted_files: signals.impacted_files,
            });
        }
        if signals.paths_in_write_set > WRITE_SET_PATHS_THRESHOLD {
            grade = grade.max(Grade::T2);
            reasons.push(GradeReason::WideWriteSet {
                paths_in_write_set: signals.paths_in_write_set,
            });
        }
        if signals.touches_security_paths {
            grade = Grade::T3;
            reasons.push(GradeReason::SecurityPaths);
        }
    }

    if signals.is_revision && signals.prior_failures >= REVISION_FAILURES_THRESHOLD {
        let from: Grade = grade;
        grade = match grade {
            Grade::T0 => Grade::T1,
            Grade::T1 => Grade::T2,
            Grade::T2 | Grade::T3 => Grade::T3,
        };
        reasons.push(GradeReason::RepeatedFailures {
            prior_failures: signals.prior_failures,
            from,
            to: grade,
        });
    }

    GradeDecision {
        grade,
        floor,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GradeReason, IMPACTED_FILES_THRESHOLD, REVISION_FAILURES_THRESHOLD, TaskSignals,
        WRITE_SET_PATHS_THRESHOLD, grade_task,
    };
    use crate::grade::{Grade, GradeFloor};
    use crate::id::TaskId;
    use crate::plan::PlannedTask;
    use crate::sandbox::Role;
    use crate::write_set::WriteSet;

    fn a_task_for(role: Role) -> PlannedTask {
        PlannedTask {
            id: TaskId::try_new("task_grading").expect("a valid grading task id"),
            role,
            write_set: WriteSet::new(Vec::new()),
            depends_on: Vec::new(),
        }
    }

    fn narrow_change() -> TaskSignals {
        TaskSignals {
            paths_in_write_set: WRITE_SET_PATHS_THRESHOLD,
            impacted_files: IMPACTED_FILES_THRESHOLD,
            touches_security_paths: false,
            is_revision: false,
            prior_failures: 0,
        }
    }

    fn repeated_revision() -> TaskSignals {
        TaskSignals {
            is_revision: true,
            prior_failures: REVISION_FAILURES_THRESHOLD,
            ..narrow_change()
        }
    }

    #[test]
    fn the_same_task_always_gets_the_same_grade() {
        for role in Role::ALL {
            let task = a_task_for(role);
            for signals in [narrow_change(), repeated_revision()] {
                assert_eq!(
                    grade_task(&task, &signals),
                    grade_task(&task, &signals),
                    "identical plan and ledger inputs must replay the full decision"
                );
            }
        }
    }

    #[test]
    fn a_security_review_is_never_graded_below_its_floor() {
        let security_review = a_task_for(Role::Security);
        for signals in [narrow_change(), repeated_revision()] {
            let decision = grade_task(&security_review, &signals);

            assert_eq!(
                decision.floor,
                Some(GradeFloor::at(Grade::T3)),
                "security review must declare its heavy-model routing floor"
            );
            let floor = decision.floor.expect("security review has a floor");
            assert_eq!(
                floor.admit(decision.grade),
                Ok(Grade::T3),
                "grading must satisfy the declared floor even after failures"
            );
            assert!(
                floor.admit(Grade::T2).is_err(),
                "the declared floor must reject weaker downstream routes"
            );
        }
    }

    #[test]
    fn a_wide_change_is_graded_above_a_narrow_one() {
        let developer = a_task_for(Role::Developer);
        let narrow = grade_task(&developer, &narrow_change());
        assert_eq!(narrow.grade, Grade::T1, "thresholds are exclusive");

        for signals in [
            TaskSignals {
                impacted_files: IMPACTED_FILES_THRESHOLD + 1,
                ..narrow_change()
            },
            TaskSignals {
                paths_in_write_set: WRITE_SET_PATHS_THRESHOLD + 1,
                ..narrow_change()
            },
        ] {
            let wide = grade_task(&developer, &signals);
            assert_eq!(wide.grade, Grade::T2, "either breadth rule needs T2");
            assert!(
                wide.grade > narrow.grade,
                "a wider developer change needs a stronger grade"
            );
        }
    }

    #[test]
    fn repeated_failures_escalate_one_grade_and_never_beyond_the_top() {
        for (role, expected) in [
            (Role::Developer, Grade::T2),
            (Role::Tester, Grade::T2),
            (Role::Researcher, Grade::T2),
            (Role::Documentation, Grade::T2),
            (Role::Reviewer, Grade::T3),
            (Role::Architect, Grade::T3),
            (Role::Coordinator, Grade::T3),
            (Role::Security, Grade::T3),
        ] {
            for prior_failures in [REVISION_FAILURES_THRESHOLD, u32::MAX] {
                let signals = TaskSignals {
                    prior_failures,
                    ..repeated_revision()
                };
                assert_eq!(
                    grade_task(&a_task_for(role), &signals).grade,
                    expected,
                    "a revision escalates once regardless of excess failures, capped at T3"
                );
            }
        }
        let broad_revision = TaskSignals {
            impacted_files: IMPACTED_FILES_THRESHOLD + 1,
            ..repeated_revision()
        };
        assert_eq!(
            grade_task(&a_task_for(Role::Developer), &broad_revision).grade,
            Grade::T3,
            "revision escalation follows the breadth rules"
        );
    }

    #[test]
    fn every_grade_decision_explains_itself() {
        for role in Role::ALL {
            let decision = grade_task(&a_task_for(role), &narrow_change());
            assert_eq!(
                decision.reasons.first(),
                Some(&GradeReason::RoleBaseline {
                    role,
                    grade: decision.grade,
                }),
                "even a task with no escalation must explain its role baseline"
            );
        }
        let signals = TaskSignals {
            impacted_files: IMPACTED_FILES_THRESHOLD + 1,
            paths_in_write_set: WRITE_SET_PATHS_THRESHOLD + 1,
            touches_security_paths: true,
            ..repeated_revision()
        };
        assert_eq!(
            grade_task(&a_task_for(Role::Developer), &signals).reasons,
            vec![
                GradeReason::RoleBaseline {
                    role: Role::Developer,
                    grade: Grade::T1,
                },
                GradeReason::WideImpact {
                    impacted_files: signals.impacted_files,
                },
                GradeReason::WideWriteSet {
                    paths_in_write_set: signals.paths_in_write_set,
                },
                GradeReason::SecurityPaths,
                GradeReason::RepeatedFailures {
                    prior_failures: signals.prior_failures,
                    from: Grade::T3,
                    to: Grade::T3,
                },
            ],
            "every applicable rule is recorded in replay order, even at the grade ceiling"
        );
        assert!(
            grade_task(&a_task_for(Role::Security), &signals)
                .reasons
                .contains(&GradeReason::SecurityFloor),
            "a routing floor must have an explicit explanation"
        );
    }

    #[test]
    fn failures_below_the_threshold_or_without_a_revision_keep_the_base_grade() {
        let developer = a_task_for(Role::Developer);
        for signals in [
            TaskSignals {
                prior_failures: REVISION_FAILURES_THRESHOLD - 1,
                ..repeated_revision()
            },
            TaskSignals {
                is_revision: false,
                ..repeated_revision()
            },
        ] {
            assert_eq!(
                grade_task(&developer, &signals).grade,
                Grade::T1,
                "both a revision and enough prior failures are needed for escalation"
            );
        }
    }

    #[test]
    fn each_role_starts_at_its_required_grade() {
        for (role, expected) in [
            (Role::Security, Grade::T3),
            (Role::Reviewer, Grade::T2),
            (Role::Architect, Grade::T2),
            (Role::Coordinator, Grade::T2),
            (Role::Developer, Grade::T1),
            (Role::Tester, Grade::T1),
            (Role::Researcher, Grade::T1),
            (Role::Documentation, Grade::T1),
        ] {
            let decision = grade_task(&a_task_for(role), &narrow_change());
            assert_eq!(decision.grade, expected, "the role sets the baseline");
            if role != Role::Security {
                assert_eq!(
                    decision.floor, None,
                    "only security review declares a floor"
                );
            }
        }
    }

    #[test]
    fn a_developer_touching_security_paths_requires_the_top_grade() {
        let security_change = TaskSignals {
            touches_security_paths: true,
            ..narrow_change()
        };
        assert_eq!(
            grade_task(&a_task_for(Role::Developer), &security_change).grade,
            Grade::T3,
            "security-sensitive development needs T3 even with a narrow impact set"
        );
    }
}
