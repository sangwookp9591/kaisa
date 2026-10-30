//! Recovering attempts that a killed Kaisa left behind (T16.6b, architecture §5.1 recovery).
//!
//! An attempt is recovered only with proof of how its agent ended: an exit record, or the
//! recorded process identity showing the agent is gone or has just been ended by Kaisa. Then its
//! work is preserved on its branch and its worktree reclaimed, exactly as after a normal run.
//! Without proof nothing is touched and the attempt is reported for a person to decide.

use std::path::{Path, PathBuf};

use kaisa_domain::{AttemptId, AttemptState, Execution, ExitStatus};
use kaisa_keeper::{ExitRecord, exit_record_path, read_exit_record, write_exit_record};
use kaisa_ledger::{AttemptRow, EventStore};
use kaisa_supervisor::{Reclamation, reclaim};
use kaisa_sys::ProcessIdentity;
use kaisa_workspace::{IgnoredPolicy, Worktree, open_worktree};

use crate::goal::{
    AGENT_IDENTITY_FILE, BUILD_OUTPUT, GoalError, at, record_cleanup, record_execution,
};
use crate::ids::now_ms;

/// The start of an attempt that recovery resolves (Kaisa starts each attempt once in M0).
const FIRST_INCARNATION: u64 = 1;

/// What recovery did with one abandoned attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovered {
    /// The agent is proven gone; its work is on `branch` (at `commit`, if it changed anything)
    /// and its worktree is reclaimed. `ended` is true if Kaisa had to end the agent itself.
    Preserved {
        /// The attempt.
        attempt: AttemptId,
        /// Where its work is.
        branch: String,
        /// The WIP commit, if there was work.
        commit: Option<String>,
        /// Whether the agent was still running and Kaisa ended it.
        ended: bool,
        /// Whether the agent had started at all before the interruption.
        started: bool,
    },
    /// Left untouched for a person to decide, with the reason.
    Kept {
        /// The attempt.
        attempt: AttemptId,
        /// Why recovery could not act.
        reason: String,
    },
}

impl Recovered {
    /// One line for the user.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Preserved {
                attempt,
                branch,
                commit,
                ended,
                started,
            } => {
                let how: &str = if !*started {
                    "its agent had not started yet"
                } else if *ended {
                    "its agent was still running and was ended"
                } else {
                    "its agent had already stopped"
                };
                let work: String = commit.as_ref().map_or_else(
                    || "no changes to preserve".to_owned(),
                    |commit| {
                        format!(
                            "work preserved on {branch} at {}",
                            &commit[..12.min(commit.len())]
                        )
                    },
                );
                format!("recovered {attempt} left by an interrupted run: {how}; {work}")
            }
            Self::Kept { attempt, reason } => {
                format!("kept {attempt} untouched: {reason}")
            }
        }
    }
}

/// How the attempt's agent ended, if that can be proven.
fn proven_exit(attempt_directory: &Path) -> Result<Result<(ExitStatus, bool), String>, GoalError> {
    if let Ok(record) = read_exit_record(&exit_record_path(attempt_directory, FIRST_INCARNATION)) {
        return Ok(Ok((record.status, false)));
    }
    let line: String =
        std::fs::read_to_string(attempt_directory.join(AGENT_IDENTITY_FILE)).unwrap_or_default();
    let Some(identity) = ProcessIdentity::from_line(&line) else {
        return Ok(Err(
            "Kaisa cannot tell whether its agent is still running".to_owned()
        ));
    };
    #[cfg(unix)]
    let ended: bool = identity
        .end_tree()
        .map_err(at("ending the abandoned agent"))?;
    #[cfg(not(unix))]
    let ended: bool = {
        if identity
            .is_running()
            .map_err(at("inspecting the abandoned agent"))?
        {
            return Ok(Err("its agent is still running".to_owned()));
        }
        false
    };
    Ok(Ok((ExitStatus::Terminated, ended)))
}

/// Recovers every attempt in `state_directory` that a previous run left unfinished.
///
/// # Errors
///
/// Returns [`GoalError`] if the ledger cannot be read or a recovery step fails; attempts
/// recovered before the failure stay recovered.
pub fn recover_abandoned(
    repository: &Path,
    state_directory: &Path,
) -> Result<Vec<Recovered>, GoalError> {
    let database: PathBuf = state_directory.join("kaisa.db");
    if !database.exists() {
        return Ok(Vec::new());
    }
    let mut ledger: EventStore = EventStore::open(&database).map_err(at("opening the ledger"))?;
    let rows: Vec<AttemptRow> = ledger
        .unfinished_attempts()
        .map_err(at("listing unfinished attempts"))?;
    let mut recovered: Vec<Recovered> = Vec::new();
    for row in rows {
        if !matches!(
            row.execution,
            Execution::Admitted | Execution::Starting | Execution::Running | Execution::Exited(_)
        ) {
            continue;
        }
        recovered.push(recover_one(
            &mut ledger,
            repository,
            state_directory,
            &row.attempt,
            row.execution,
        )?);
    }
    Ok(recovered)
}

fn recover_one(
    ledger: &mut EventStore,
    repository: &Path,
    state_directory: &Path,
    attempt: &AttemptId,
    execution: Execution,
) -> Result<Recovered, GoalError> {
    let attempt_directory: PathBuf = state_directory.join("attempts").join(attempt.as_str());
    let kept = |reason: &str| Recovered::Kept {
        attempt: attempt.clone(),
        reason: reason.to_owned(),
    };
    // `run_goal` records `running` before it starts the agent, so an attempt still admitted or
    // starting never had an agent: there is nothing to end, only a worktree to tidy (T16.6).
    let never_started: bool = matches!(execution, Execution::Admitted | Execution::Starting);
    let (status, ended) = if never_started {
        (ExitStatus::Terminated, false)
    } else {
        match proven_exit(&attempt_directory)? {
            Ok(proof) => proof,
            Err(reason) => return Ok(kept(&reason)),
        }
    };
    let worktree_path: PathBuf = state_directory.join("worktrees").join(attempt.as_str());
    let Some(worktree): Option<Worktree> =
        open_worktree(repository, &worktree_path).map_err(at("finding the worktree"))?
    else {
        return Ok(kept(
            "its worktree is not a git worktree of this repository",
        ));
    };
    let _ = write_exit_record(
        &attempt_directory,
        &ExitRecord {
            attempt: attempt.clone(),
            incarnation: FIRST_INCARNATION,
            status,
            finished_at_ms: now_ms(),
        },
    );
    let mut state: AttemptState = AttemptState::admitted();
    state.start().map_err(at("replaying the attempt"))?;
    state
        .confirm_running()
        .map_err(at("replaying the attempt"))?;
    state
        .record_exit(status)
        .map_err(at("recording the exit"))?;
    record_execution(ledger, attempt, Execution::Exited(status))?;
    let reclamation: Reclamation = reclaim(
        &mut state,
        repository,
        &worktree,
        &IgnoredPolicy::with_build_output(&BUILD_OUTPUT),
    )
    .map_err(at("preserving and reclaiming"))?;
    record_cleanup(ledger, attempt, state.cleanup())?;
    Ok(match reclamation {
        Reclamation::Reclaimed(receipt) => Recovered::Preserved {
            attempt: attempt.clone(),
            branch: worktree.branch().to_owned(),
            commit: receipt.commit().map(str::to_owned),
            ended,
            started: !never_started,
        },
        Reclamation::Blocked { uncovered } => kept(&format!(
            "{} could not be preserved automatically",
            uncovered.join(", ")
        )),
        Reclamation::Retained => kept("a person asked to keep it"),
    })
}
