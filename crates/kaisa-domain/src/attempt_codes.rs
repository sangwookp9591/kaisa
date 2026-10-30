//! Stable text codes for attempt state, as stored in events and projections.
//!
//! Codes never change once released: a ledger written today must replay the same way later.

use crate::attempt::{Cleanup, Execution, ExitStatus};

/// Prefix of the code of an execution that exited.
const EXITED_PREFIX: &str = "exited:";
/// The part after [`EXITED_PREFIX`] for a process ended from outside.
const TERMINATED: &str = "terminated";

impl Execution {
    /// Stable code: `admitted`, `starting`, `running`, `stalled`, `unknown`, or `exited:<code>`
    /// and `exited:terminated` for an exit.
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::Admitted => "admitted".to_owned(),
            Self::Starting => "starting".to_owned(),
            Self::Running => "running".to_owned(),
            Self::Stalled => "stalled".to_owned(),
            Self::Unknown => "unknown".to_owned(),
            Self::Exited(ExitStatus::Code(code)) => format!("{EXITED_PREFIX}{code}"),
            Self::Exited(ExitStatus::Terminated) => format!("{EXITED_PREFIX}{TERMINATED}"),
        }
    }

    /// The execution state a stored code names.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "admitted" => Some(Self::Admitted),
            "starting" => Some(Self::Starting),
            "running" => Some(Self::Running),
            "stalled" => Some(Self::Stalled),
            "unknown" => Some(Self::Unknown),
            other => {
                let exit: &str = other.strip_prefix(EXITED_PREFIX)?;
                if exit == TERMINATED {
                    Some(Self::Exited(ExitStatus::Terminated))
                } else {
                    exit.parse()
                        .ok()
                        .map(|code| Self::Exited(ExitStatus::Code(code)))
                }
            }
        }
    }
}

impl Cleanup {
    /// Every cleanup state, for exhaustive lookups.
    pub const ALL: [Self; 6] = [
        Self::Held,
        Self::Preserving,
        Self::Preserved,
        Self::Reclaiming,
        Self::Reclaimed,
        Self::Retained,
    ];

    /// Stable code.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Preserving => "preserving",
            Self::Preserved => "preserved",
            Self::Reclaiming => "reclaiming",
            Self::Reclaimed => "reclaimed",
            Self::Retained => "retained",
        }
    }

    /// The cleanup state a stored code names.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.code() == code)
    }
}

#[cfg(test)]
mod tests {
    use crate::attempt::{Cleanup, Execution, ExitStatus};

    /// The exit code a typical CLI agent uses for "I failed".
    const AGENT_FAILURE_CODE: i32 = 1;

    #[test]
    fn every_execution_state_round_trips_through_its_code() {
        for execution in [
            Execution::Admitted,
            Execution::Starting,
            Execution::Running,
            Execution::Stalled,
            Execution::Unknown,
            Execution::Exited(ExitStatus::Code(ExitStatus::SUCCESS_CODE)),
            Execution::Exited(ExitStatus::Code(AGENT_FAILURE_CODE)),
            Execution::Exited(ExitStatus::Terminated),
        ] {
            assert_eq!(
                Execution::from_code(&execution.code()),
                Some(execution),
                "a replayed ledger must restore exactly the stored execution state"
            );
        }
    }

    #[test]
    fn every_cleanup_state_round_trips_through_its_code() {
        for cleanup in Cleanup::ALL {
            assert_eq!(
                Cleanup::from_code(cleanup.code()),
                Some(cleanup),
                "a replayed ledger must restore exactly the stored cleanup state"
            );
        }
    }

    #[test]
    fn a_malformed_exit_code_is_not_read_as_a_state() {
        assert_eq!(
            Execution::from_code("exited:soon"),
            None,
            "a corrupt row must be reported, never guessed"
        );
    }
}
