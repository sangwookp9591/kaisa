//! What happens when an attempt's lease runs out (architecture §5.1).
//!
//! An expired lease on a live process means "no sign of progress", not "dead". Kaisa marks the
//! attempt stalled and tells a human; it never kills it for being quiet, because a quiet agent
//! may be in a long build. There is deliberately no "kill" action here.

use kaisa_domain::Execution;

/// What the supervisor does when an attempt's lease has expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryAction {
    /// The process is alive but showed no progress: mark the attempt stalled.
    MarkStalled,
    /// Already stalled and still alive: keep it, and keep the human informed.
    KeepStalled,
    /// The process is gone: decide from proof what happened (see `decide_recovery`).
    Recover,
    /// The attempt holds no process; the lease only guards its resources.
    Nothing,
}

/// Decides what an expired lease means for an attempt in `execution`.
#[must_use]
pub fn on_lease_expired(execution: Execution, process_alive: bool) -> ExpiryAction {
    match (execution, process_alive) {
        (Execution::Running, true) => ExpiryAction::MarkStalled,
        (Execution::Stalled, true) => ExpiryAction::KeepStalled,
        (Execution::Starting | Execution::Running | Execution::Stalled, false) => {
            ExpiryAction::Recover
        }
        _ => ExpiryAction::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use kaisa_domain::Execution;

    use super::{ExpiryAction, on_lease_expired};

    /// The agent's process is observably running.
    const ALIVE: bool = true;

    #[test]
    fn an_expired_lease_on_a_live_attempt_marks_it_stalled() {
        assert_eq!(
            on_lease_expired(Execution::Running, ALIVE),
            ExpiryAction::MarkStalled,
            "silence is reported as a stall, never treated as death"
        );
    }

    #[test]
    fn a_stalled_attempt_keeps_its_process() {
        assert_eq!(
            on_lease_expired(Execution::Stalled, ALIVE),
            ExpiryAction::KeepStalled,
            "a long build must not be killed for being quiet; a human decides"
        );
    }
}
