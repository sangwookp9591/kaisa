//! What the supervisor concludes about an attempt after a daemon restart (architecture §5.1).
//!
//! The decision uses only proof: the keeper's durable exit record, and whether the process is
//! observably alive. Anything else leaves the attempt `Unknown` rather than guessing.

use kaisa_domain::{Execution, ExitStatus};

/// The supervisor's conclusion about one attempt after a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// The exit record proves how the process ended.
    Exited(ExitStatus),
    /// The process is alive; keep supervising it.
    StillRunning,
    /// No record and no process: Kaisa no longer knows what happened.
    LostTrack,
    /// Already `Unknown` and still without proof: a human decides. Recovery never restarts or
    /// retries such an attempt (architecture P8), because it may still be changing things.
    NeedsDecision,
    /// The attempt was not in flight; nothing to recover.
    Unchanged,
}

/// Decides how an attempt in state `execution` recovers, given its exit record if one is on
/// disk and whether its process is alive.
#[must_use]
pub fn decide_recovery(
    execution: Execution,
    exit_record: Option<ExitStatus>,
    process_alive: bool,
) -> Recovery {
    let in_flight: bool = matches!(
        execution,
        Execution::Starting | Execution::Running | Execution::Stalled
    );
    if execution == Execution::Unknown {
        // Only an exit record is proof. A process that looks alive may be a reused PID.
        return exit_record.map_or(Recovery::NeedsDecision, Recovery::Exited);
    }
    if !in_flight {
        return Recovery::Unchanged;
    }
    match (exit_record, process_alive) {
        (Some(status), _) => Recovery::Exited(status),
        (None, true) => Recovery::StillRunning,
        (None, false) => Recovery::LostTrack,
    }
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{Execution, ExitStatus};

    use super::{Recovery, decide_recovery};

    /// The exit code a typical CLI agent uses for "I failed".
    const AGENT_FAILURE_CODE: i32 = 1;
    /// What the keeper wrote before the daemon went down.
    const RECORDED_FAILURE: ExitStatus = ExitStatus::Code(AGENT_FAILURE_CODE);
    /// The process is observably running.
    const ALIVE: bool = true;
    /// No process could be found.
    const GONE: bool = false;

    #[test]
    fn an_attempt_with_an_exit_record_is_resolved_after_a_restart() {
        assert_eq!(
            decide_recovery(Execution::Running, Some(RECORDED_FAILURE), GONE),
            Recovery::Exited(RECORDED_FAILURE),
            "the keeper's durable record is proof of how the agent ended"
        );
    }

    #[test]
    fn an_attempt_with_no_record_and_no_process_becomes_unknown() {
        assert_eq!(
            decide_recovery(Execution::Running, None, GONE),
            Recovery::LostTrack,
            "with no proof either way, Kaisa says it does not know"
        );
    }

    #[test]
    fn a_live_attempt_keeps_running_after_a_restart() {
        assert_eq!(
            decide_recovery(Execution::Running, None, ALIVE),
            Recovery::StillRunning,
            "a daemon restart must not disturb agents that are still working"
        );
    }

    #[test]
    fn an_unknown_attempt_is_never_restarted_by_recovery() {
        for process_seems_alive in [ALIVE, GONE] {
            assert_eq!(
                decide_recovery(Execution::Unknown, None, process_seems_alive),
                Recovery::NeedsDecision,
                "an attempt that may still be writing is never started twice; a human decides"
            );
        }
    }

    #[test]
    fn an_exit_record_still_resolves_an_unknown_attempt() {
        assert_eq!(
            decide_recovery(Execution::Unknown, Some(RECORDED_FAILURE), GONE),
            Recovery::Exited(RECORDED_FAILURE),
            "proof that arrives late still settles the attempt"
        );
    }
}
