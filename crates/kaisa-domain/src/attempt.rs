//! The lifecycle of one attempt, modelled as four independent state axes.
//!
//! Architecture §5.1: execution, outcome, verification and cleanup move independently. Every
//! change goes through a method that checks the transition is legal, so invariants such as
//! "an attempt in `Unknown` is never restarted" and "nothing is reclaimed before it is
//! preserved" hold by construction.

use std::fmt;

/// Where the attempt's process is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Execution {
    /// Accepted by the scheduler; nothing started yet.
    Admitted,
    /// The keeper is starting the agent process.
    Starting,
    /// The agent process is running.
    Running,
    /// Running, but no activity has been observed for longer than the stall threshold.
    Stalled,
    /// The process exited with this status.
    Exited(ExitStatus),
    /// Kaisa lost track of whether the process started or stopped. Only proof resolves this.
    Unknown,
}

/// How a process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    /// The process returned an exit code.
    Code(i32),
    /// The process was ended by a signal (Unix) or terminated externally (Windows).
    Terminated,
}

impl ExitStatus {
    /// Exit code that conventionally means success.
    pub const SUCCESS_CODE: i32 = 0;

    /// True when the process reported success.
    #[must_use]
    pub fn is_success(self) -> bool {
        self == Self::Code(Self::SUCCESS_CODE)
    }
}

/// Why an attempt failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureReason {
    /// The agent process exited unsuccessfully.
    Agent,
    /// The process was terminated before it finished.
    Terminated,
}

/// What the attempt achieved, independent of whether the result was verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// Not finished yet.
    Pending,
    /// The agent finished its work. The result still has to pass verification.
    Completed,
    /// The attempt failed.
    Failed(FailureReason),
}

/// Where the attempt's result is in the Verify Gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verification {
    /// Verification has not run.
    NotStarted,
}

/// What has happened to the attempt's resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Cleanup {
    /// Resources are held by the attempt.
    Held,
    /// The preservation protocol is running.
    Preserving,
    /// Work is preserved; the receipt covers everything that will be deleted.
    Preserved,
    /// Resources are being removed.
    Reclaiming,
    /// Everything covered by the receipt has been removed.
    Reclaimed,
    /// A human asked to keep the resources; they are excluded from reclamation.
    Retained,
}

/// The state axis a rejected transition was about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// [`Execution`].
    Execution,
    /// [`Cleanup`].
    Cleanup,
}

/// A transition that the lifecycle rules do not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    /// The axis that refused the change.
    pub axis: Axis,
    /// The action that was attempted.
    pub action: &'static str,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not allowed in the current {:?} state",
            self.action, self.axis
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// The four state axes of one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptState {
    execution: Execution,
    outcome: Outcome,
    verification: Verification,
    cleanup: Cleanup,
}

impl Default for AttemptState {
    fn default() -> Self {
        Self::admitted()
    }
}

impl AttemptState {
    /// A freshly admitted attempt.
    #[must_use]
    pub fn admitted() -> Self {
        Self {
            execution: Execution::Admitted,
            outcome: Outcome::Pending,
            verification: Verification::NotStarted,
            cleanup: Cleanup::Held,
        }
    }

    /// The execution axis.
    #[must_use]
    pub fn execution(&self) -> Execution {
        self.execution
    }

    /// The outcome axis.
    #[must_use]
    pub fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// The verification axis.
    #[must_use]
    pub fn verification(&self) -> Verification {
        self.verification
    }

    /// The cleanup axis.
    #[must_use]
    pub fn cleanup(&self) -> Cleanup {
        self.cleanup
    }

    /// The keeper begins starting the process. Only an admitted attempt may start, so an
    /// exited or `Unknown` attempt can never be started a second time.
    ///
    /// # Errors
    ///
    /// Returns [`IllegalTransition`] unless the attempt is [`Execution::Admitted`].
    pub fn start(&mut self) -> Result<(), IllegalTransition> {
        self.execution_step("start", |execution| {
            matches!(execution, Execution::Admitted).then_some(Execution::Starting)
        })
    }

    /// The keeper confirmed the process is running.
    ///
    /// # Errors
    ///
    /// Returns [`IllegalTransition`] unless the attempt is [`Execution::Starting`].
    pub fn confirm_running(&mut self) -> Result<(), IllegalTransition> {
        self.execution_step("confirm_running", |execution| {
            matches!(execution, Execution::Starting).then_some(Execution::Running)
        })
    }

    /// No activity was observed for longer than the stall threshold.
    ///
    /// # Errors
    ///
    /// Returns [`IllegalTransition`] unless the attempt is [`Execution::Running`].
    pub fn mark_stalled(&mut self) -> Result<(), IllegalTransition> {
        self.execution_step("mark_stalled", |execution| {
            matches!(execution, Execution::Running).then_some(Execution::Stalled)
        })
    }

    /// Kaisa lost track of the process — contact was lost while starting, or the keeper of a
    /// running attempt is gone without an exit record — so whether it runs is unknown until
    /// proven (architecture §5.1, "Keeper loss").
    ///
    /// # Errors
    ///
    /// Returns [`IllegalTransition`] unless the attempt is starting, running or stalled.
    pub fn lose_track(&mut self) -> Result<(), IllegalTransition> {
        self.execution_step("lose_track", |execution| {
            matches!(
                execution,
                Execution::Starting | Execution::Running | Execution::Stalled
            )
            .then_some(Execution::Unknown)
        })
    }

    /// The keeper observed the process exit. The outcome follows the exit status; verification
    /// is untouched, because exiting successfully is not the same as passing verification.
    ///
    /// Also resolves [`Execution::Unknown`], because an exit record is proof.
    ///
    /// # Errors
    ///
    /// Returns [`IllegalTransition`] if the process was never started or already exited.
    pub fn record_exit(&mut self, status: ExitStatus) -> Result<(), IllegalTransition> {
        self.execution_step("record_exit", |execution| {
            matches!(
                execution,
                Execution::Running | Execution::Stalled | Execution::Starting | Execution::Unknown
            )
            .then_some(Execution::Exited(status))
        })?;
        self.outcome = match status {
            ExitStatus::Code(_) if status.is_success() => Outcome::Completed,
            ExitStatus::Code(_) => Outcome::Failed(FailureReason::Agent),
            ExitStatus::Terminated => Outcome::Failed(FailureReason::Terminated),
        };
        Ok(())
    }

    /// The preservation protocol begins. No writer may be active, so the process must have
    /// exited.
    ///
    /// # Errors
    ///
    /// Returns [`IllegalTransition`] unless the process exited and resources are still held.
    pub fn begin_preservation(&mut self) -> Result<(), IllegalTransition> {
        if !matches!(self.execution, Execution::Exited(_)) {
            return Err(IllegalTransition {
                axis: Axis::Execution,
                action: "begin_preservation",
            });
        }
        self.cleanup_step("begin_preservation", Cleanup::Held, Cleanup::Preserving)
    }

    /// A preservation receipt covering every resource to be deleted was written and verified.
    ///
    /// # Errors
    ///
    /// Returns [`IllegalTransition`] unless preservation is in progress.
    pub fn record_preservation(&mut self) -> Result<(), IllegalTransition> {
        self.cleanup_step(
            "record_preservation",
            Cleanup::Preserving,
            Cleanup::Preserved,
        )
    }

    /// Reclamation of preserved resources begins.
    ///
    /// # Errors
    ///
    /// Returns [`IllegalTransition`] unless the work is preserved.
    pub fn begin_reclaim(&mut self) -> Result<(), IllegalTransition> {
        self.cleanup_step("begin_reclaim", Cleanup::Preserved, Cleanup::Reclaiming)
    }

    /// Every resource covered by the receipt was removed.
    ///
    /// # Errors
    ///
    /// Returns [`IllegalTransition`] unless reclamation is in progress.
    pub fn finish_reclaim(&mut self) -> Result<(), IllegalTransition> {
        self.cleanup_step("finish_reclaim", Cleanup::Reclaiming, Cleanup::Reclaimed)
    }

    /// A human asked to keep the resources.
    ///
    /// # Errors
    ///
    /// Returns [`IllegalTransition`] once reclamation has started.
    pub fn retain(&mut self) -> Result<(), IllegalTransition> {
        if matches!(self.cleanup, Cleanup::Reclaiming | Cleanup::Reclaimed) {
            return Err(IllegalTransition {
                axis: Axis::Cleanup,
                action: "retain",
            });
        }
        self.cleanup = Cleanup::Retained;
        Ok(())
    }

    fn execution_step(
        &mut self,
        action: &'static str,
        next: impl FnOnce(Execution) -> Option<Execution>,
    ) -> Result<(), IllegalTransition> {
        let next: Execution = next(self.execution).ok_or(IllegalTransition {
            axis: Axis::Execution,
            action,
        })?;
        self.execution = next;
        Ok(())
    }

    fn cleanup_step(
        &mut self,
        action: &'static str,
        from: Cleanup,
        to: Cleanup,
    ) -> Result<(), IllegalTransition> {
        if self.cleanup != from {
            return Err(IllegalTransition {
                axis: Axis::Cleanup,
                action,
            });
        }
        self.cleanup = to;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttemptState, Axis, Cleanup, Execution, ExitStatus, FailureReason, IllegalTransition,
        Outcome, Verification,
    };

    /// The exit code a typical CLI agent uses for "I failed".
    const AGENT_FAILURE_CODE: i32 = 1;

    fn a_running_attempt() -> AttemptState {
        let mut attempt = AttemptState::admitted();
        attempt.start().expect("an admitted attempt can start");
        attempt
            .confirm_running()
            .expect("a starting attempt can be confirmed running");
        attempt
    }

    fn an_attempt_that_exited(status: ExitStatus) -> AttemptState {
        let mut attempt = a_running_attempt();
        attempt
            .record_exit(status)
            .expect("a running attempt can exit");
        attempt
    }

    fn a_successful_exit() -> ExitStatus {
        ExitStatus::Code(ExitStatus::SUCCESS_CODE)
    }

    fn an_attempt_whose_start_was_lost() -> AttemptState {
        let mut attempt = AttemptState::admitted();
        attempt.start().expect("an admitted attempt can start");
        attempt
            .lose_track()
            .expect("contact can be lost while starting");
        attempt
    }

    #[test]
    fn an_admitted_attempt_can_start_running() {
        let attempt: AttemptState = a_running_attempt();

        assert_eq!(
            attempt.execution(),
            Execution::Running,
            "admitted → starting → running"
        );
    }

    #[test]
    fn an_exited_attempt_cannot_start_running_again() {
        let mut attempt: AttemptState = an_attempt_that_exited(a_successful_exit());

        assert_eq!(
            attempt.start(),
            Err(IllegalTransition {
                axis: Axis::Execution,
                action: "start"
            }),
            "a finished attempt is never restarted; a new attempt is created instead"
        );
    }

    #[test]
    fn an_attempt_in_unknown_state_cannot_be_restarted_without_proof() {
        let mut attempt: AttemptState = an_attempt_whose_start_was_lost();

        assert_eq!(
            attempt.start(),
            Err(IllegalTransition {
                axis: Axis::Execution,
                action: "start"
            }),
            "restarting an attempt that may still be running would create a duplicate agent"
        );
        assert_eq!(
            attempt.execution(),
            Execution::Unknown,
            "the unknown state is kept"
        );
    }

    #[test]
    fn a_running_attempt_whose_keeper_is_lost_becomes_unknown() {
        let mut attempt: AttemptState = a_running_attempt();

        attempt
            .lose_track()
            .expect("a running attempt can lose its keeper");

        assert_eq!(
            attempt.execution(),
            Execution::Unknown,
            "a lost keeper is never read as an exit or a failure, only as not knowing"
        );
    }

    #[test]
    fn an_exit_record_resolves_an_unknown_attempt() {
        let mut attempt: AttemptState = an_attempt_whose_start_was_lost();

        attempt
            .record_exit(a_successful_exit())
            .expect("an exit record is proof");

        assert_eq!(
            attempt.execution(),
            Execution::Exited(a_successful_exit()),
            "proof of exit is the only way out of the unknown state"
        );
    }

    #[test]
    fn a_zero_exit_completes_the_outcome_but_leaves_verification_not_started() {
        let attempt: AttemptState = an_attempt_that_exited(a_successful_exit());

        assert_eq!(
            attempt.outcome(),
            Outcome::Completed,
            "the agent finished its work"
        );
        assert_eq!(
            attempt.verification(),
            Verification::NotStarted,
            "exiting successfully is not the same as passing verification"
        );
    }

    #[test]
    fn a_failing_exit_marks_the_outcome_as_an_agent_failure() {
        let attempt: AttemptState = an_attempt_that_exited(ExitStatus::Code(AGENT_FAILURE_CODE));

        assert_eq!(
            attempt.outcome(),
            Outcome::Failed(FailureReason::Agent),
            "a non-zero exit code is the agent reporting failure"
        );
    }

    #[test]
    fn a_running_attempt_cannot_be_preserved_while_its_process_may_still_write() {
        let mut attempt: AttemptState = a_running_attempt();

        assert_eq!(
            attempt.begin_preservation(),
            Err(IllegalTransition {
                axis: Axis::Execution,
                action: "begin_preservation"
            }),
            "preserving while a writer is active could miss its last changes"
        );
    }

    #[test]
    fn cleanup_cannot_reclaim_before_preservation_has_a_receipt() {
        let mut attempt: AttemptState = an_attempt_that_exited(a_successful_exit());
        attempt
            .begin_preservation()
            .expect("an exited attempt can be preserved");

        assert_eq!(
            attempt.begin_reclaim(),
            Err(IllegalTransition {
                axis: Axis::Cleanup,
                action: "begin_reclaim"
            }),
            "deleting before the receipt exists could lose work"
        );
    }

    #[test]
    fn a_preserved_attempt_can_be_fully_reclaimed() {
        let mut attempt: AttemptState = an_attempt_that_exited(a_successful_exit());

        attempt
            .begin_preservation()
            .expect("an exited attempt can be preserved");
        attempt
            .record_preservation()
            .expect("a receipt completes preservation");
        attempt
            .begin_reclaim()
            .expect("preserved work can be reclaimed");
        attempt.finish_reclaim().expect("reclamation can finish");

        assert_eq!(
            attempt.cleanup(),
            Cleanup::Reclaimed,
            "the full cleanup path is allowed"
        );
    }

    #[test]
    fn a_retained_attempt_is_excluded_from_reclamation() {
        let mut attempt: AttemptState = an_attempt_that_exited(a_successful_exit());
        attempt
            .retain()
            .expect("a human may keep resources before reclamation");

        assert_eq!(
            attempt.begin_reclaim(),
            Err(IllegalTransition {
                axis: Axis::Cleanup,
                action: "begin_reclaim"
            }),
            "retained resources are never reclaimed"
        );
    }
}
