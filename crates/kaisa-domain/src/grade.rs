//! Task grades and the `never_downgrade` floor (architecture §5.10).
//!
//! A grade says how strong a model a Task needs. The router may pick any eligible option at or
//! above a Task's floor; it may never route below it, whatever the budget says.

use std::fmt;

/// How strong a model a Task needs, from "no model at all" to "heavy".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Grade {
    /// No model: a script or codemod does the work.
    T0,
    /// A light model: small, checkable work.
    T1,
    /// A standard model: ordinary feature, bug and review work.
    T2,
    /// A heavy model: architecture, hard debugging, security review, final output.
    T3,
}

/// The lowest grade a step may be routed to. Steps such as security review carry one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GradeFloor(Grade);

/// A route was below the step's floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BelowFloor {
    /// The grade the router tried to use.
    pub route: Grade,
    /// The lowest grade the step allows.
    pub floor: Grade,
}

impl fmt::Display for BelowFloor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "this step is never routed below {:?}, but {:?} was chosen",
            self.floor, self.route
        )
    }
}

impl std::error::Error for BelowFloor {}

impl GradeFloor {
    /// A floor at `grade`.
    #[must_use]
    pub fn at(grade: Grade) -> Self {
        Self(grade)
    }

    /// Accepts `route` if it is at or above the floor.
    ///
    /// # Errors
    ///
    /// Returns [`BelowFloor`] if `route` is weaker than the floor.
    pub fn admit(self, route: Grade) -> Result<Grade, BelowFloor> {
        if route < self.0 {
            Err(BelowFloor {
                route,
                floor: self.0,
            })
        } else {
            Ok(route)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BelowFloor, Grade, GradeFloor};

    /// Security review is never routed below a heavy model.
    fn the_security_review_floor() -> GradeFloor {
        GradeFloor::at(Grade::T3)
    }

    #[test]
    fn grades_are_ordered_from_no_model_to_heavy() {
        let mut shuffled: Vec<Grade> = vec![Grade::T2, Grade::T0, Grade::T3, Grade::T1];

        shuffled.sort();

        assert_eq!(
            shuffled,
            vec![Grade::T0, Grade::T1, Grade::T2, Grade::T3],
            "escalation means moving to a later grade"
        );
    }

    #[test]
    fn a_route_below_the_floor_is_refused() {
        assert_eq!(
            the_security_review_floor().admit(Grade::T1),
            Err(BelowFloor {
                route: Grade::T1,
                floor: Grade::T3
            }),
            "a tight budget must never buy a weaker security review"
        );
    }

    #[test]
    fn a_route_at_or_above_the_floor_is_allowed() {
        let floor: GradeFloor = GradeFloor::at(Grade::T2);

        assert_eq!(
            (floor.admit(Grade::T2), floor.admit(Grade::T3)),
            (Ok(Grade::T2), Ok(Grade::T3)),
            "the floor limits how low a route goes, never how high"
        );
    }
}
