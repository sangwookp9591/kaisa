//! Recovering attempts that a killed Kaisa left behind (T16.6b, architecture §5.1 recovery).
//!
//! An attempt is recovered only with proof of how its agent ended: an exit record, or the
//! recorded process identity showing the agent is gone or has just been ended by Kaisa. Then its
//! work is preserved on its branch and its worktree reclaimed, exactly as after a normal run.
//! Without proof nothing is touched and the attempt is reported for a person to decide.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use kaisa_domain::{AttemptId, AttemptState, Cleanup, EventKind, Execution, ExitStatus};
use kaisa_keeper::{ExitRecord, exit_record_path, read_exit_record, write_exit_record};
use kaisa_ledger::{AttemptRow, EventStore};
use kaisa_supervisor::{Reclamation, reclaim};
use kaisa_sys::ProcessIdentity;
use kaisa_workspace::{IgnoredPolicy, Worktree, open_worktree};

use crate::goal::{
    AGENT_IDENTITY_FILE, BUILD_OUTPUT, GoalError, at, record, record_cleanup, record_execution,
    record_identity,
};
use crate::ids::now_ms;

/// The start of an attempt that recovery resolves (Kaisa starts each attempt once in M0).
const FIRST_INCARNATION: u64 = 1;

/// This file is never unlinked: all owners and reclaimers must lock the same inode.
/// The OS releases the lock on process death; child processes do not inherit its handle.
pub(crate) const OWNER_LOCK_FILE: &str = "owner.lock";
/// Kept separate from the locked bytes: Windows byte-range locks also prevent reads.
pub(crate) const OWNER_IDENTITY_FILE: &str = "owner.identity";
/// Give the OS time to release a killed agent's handles; never wait for a live owner.
const LOCK_RELEASE_PATIENCE: Duration = Duration::from_secs(2);
const LOCK_RELEASE_POLL: Duration = Duration::from_millis(10);

/// Windows grants read access to the entire attempt directory. Keep owner evidence outside
/// that grant so a contained agent cannot open the lock at all. Landlock does not deny reads,
/// so Unix additionally verifies the recorded identity when a lock is held.
pub(crate) fn owner_directory(state_directory: &Path, attempt: &AttemptId) -> PathBuf {
    #[cfg(windows)]
    let parent = "owners";
    #[cfg(not(windows))]
    let parent = "attempts";
    state_directory.join(parent).join(attempt.as_str())
}

fn wait_for_released_lock(owner: &File) -> Result<bool, GoalError> {
    let deadline = Instant::now() + LOCK_RELEASE_PATIENCE;
    loop {
        match owner.try_lock() {
            Ok(()) => return Ok(true),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(LOCK_RELEASE_POLL);
            }
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Error(error)) => return Err(at("locking the attempt owner")(error)),
        }
    }
}

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
        /// An agent held the lock even though its recorded owner was proven dead.
        unowned_lock: bool,
    },
    /// The user removed the worktree and kept its branch; no Git mutation was needed.
    RemovedByUser {
        /// The attempt whose resource cleanup is now terminal.
        attempt: AttemptId,
        /// The branch kept exactly as the user left it.
        branch: String,
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
                unowned_lock,
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
                let ownership = if *unowned_lock {
                    "its recorded owner was dead despite the held lock; "
                } else {
                    ""
                };
                format!("recovered {attempt} left by an interrupted run: {ownership}{how}; {work}")
            }
            Self::RemovedByUser { attempt, branch } => {
                format!(
                    "reclaimed {attempt}: its worktree was removed by the user; branch {branch} kept untouched"
                )
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

/// Quotes a path for a Unix shell, or PowerShell on Windows, in manual cleanup guidance.
fn quoted_path(path: &Path) -> String {
    #[cfg(not(windows))]
    let escaped = path.to_string_lossy().replace('\'', "'\"'\"'");
    #[cfg(windows)]
    let escaped = path.to_string_lossy().replace('\'', "''");
    format!("'{escaped}'")
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
        let owner_directory = owner_directory(state_directory, &row.attempt);
        let kept = |reason: String| Recovered::Kept {
            attempt: row.attempt.clone(),
            reason,
        };
        let owner = match OpenOptions::new()
            .read(true)
            .write(true)
            .open(owner_directory.join(OWNER_LOCK_FILE))
        {
            Ok(owner) => owner,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Removing the advised worktree is the user's terminal cleanup decision.
                if let Some(done) =
                    recognise_user_removal(&mut ledger, repository, state_directory, &row.attempt)?
                {
                    recovered.push(done);
                    continue;
                }
                recovered.push(kept(format!(
                    "recorded by an older Kaisa; Kaisa cannot prove its owner is gone (no owner lock). \
                     After confirming its owner and agent have stopped, inspect worktree {} on \
                     branch kaisa/{} and commit any uncommitted changes there; committed changes \
                     stay on that branch. Then remove the worktree manually: git -C {} worktree remove {}. \
                     The next run recognises the removed worktree and records cleanup without changing the branch",
                    state_directory.join("worktrees").join(row.attempt.as_str()).display(),
                    row.attempt,
                    quoted_path(repository),
                    quoted_path(&state_directory.join("worktrees").join(row.attempt.as_str())),
                )));
                continue;
            }
            Err(error) => return Err(at("opening the attempt owner lock")(error)),
        };
        let mut earlier_exit = None;
        match owner.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let identity = std::fs::read_to_string(owner_directory.join(OWNER_IDENTITY_FILE))
                    .ok()
                    .and_then(|line| ProcessIdentity::from_line(&line));
                let Some(identity) = identity else {
                    recovered.push(kept(
                        "its owner lock is held; Kaisa cannot verify the owner's identity"
                            .to_owned(),
                    ));
                    continue;
                };
                match identity.is_running() {
                    Ok(true) => {
                        recovered.push(kept(format!(
                            "in use by another running Kaisa or recovery (owner pid {})",
                            identity.pid
                        )));
                        continue;
                    }
                    Err(error) => {
                        recovered.push(kept(format!("its owner lock is held and the recorded owner cannot be inspected: {error}")));
                        continue;
                    }
                    Ok(false) => {}
                }
                // An advisory lock alone is not ownership proof: a read-only confined agent
                // can take it after the real owner dies. End only its recorded agent tree,
                // then reacquire the same lock before preserving or mutating any state.
                let directory = state_directory.join("attempts").join(row.attempt.as_str());
                match proven_exit(&directory)? {
                    Ok(proof) => earlier_exit = Some(proof),
                    Err(reason) => {
                        recovered.push(kept(format!(
                            "its recorded owner was dead despite the held lock; {reason}"
                        )));
                        continue;
                    }
                }
                if !wait_for_released_lock(&owner)? {
                    recovered.push(kept("its recorded owner was dead; its agent was checked and ended if running, but the lock is still held by another recovery or an unidentified holder".to_owned()));
                    continue;
                }
            }
            Err(TryLockError::Error(error)) => return Err(at("locking the attempt owner")(error)),
        }
        // Publish this reclaimer while holding the lock, so peers also recognise its life.
        record_identity(
            &owner_directory.join(OWNER_IDENTITY_FILE),
            ProcessIdentity::of(std::process::id())
                .map_err(at("identifying the recovery owner"))?,
        )
        .map_err(at("recording the recovery owner"))?;
        // The snapshot may predate another owner's/reclaimer's completion. Refresh while
        // exclusively owning this attempt before looking at its agent or worktree.
        ledger
            .rebuild_attempts()
            .map_err(at("refreshing the attempts"))?;
        let Some(row) = ledger
            .attempt(&row.attempt)
            .map_err(at("refreshing the attempt"))?
        else {
            recovered.push(kept(
                "another recovery removed its ledger projection".to_owned(),
            ));
            continue;
        };
        if matches!(row.cleanup, Cleanup::Reclaimed | Cleanup::Retained) {
            recovered.push(Recovered::Kept {
                attempt: row.attempt.clone(),
                reason: "another owner or recovery already completed its cleanup".to_owned(),
            });
            continue;
        }
        if !matches!(
            row.execution,
            Execution::Admitted | Execution::Starting | Execution::Running | Execution::Exited(_)
        ) {
            recovered.push(Recovered::Kept {
                attempt: row.attempt.clone(),
                reason: "its execution state does not prove it can be recovered".to_owned(),
            });
            continue;
        }
        recovered.push(recover_one(
            &mut ledger,
            repository,
            state_directory,
            &row.attempt,
            row.execution,
            earlier_exit,
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
    earlier_exit: Option<(ExitStatus, bool)>,
) -> Result<Recovered, GoalError> {
    let attempt_directory: PathBuf = state_directory.join("attempts").join(attempt.as_str());
    let kept = |reason: &str| Recovered::Kept {
        attempt: attempt.clone(),
        reason: reason.to_owned(),
    };
    // `run_goal` records `running` before it starts the agent, so an attempt still admitted or
    // starting never had an agent: there is nothing to end, only a worktree to tidy (T16.6).
    let never_started: bool = matches!(execution, Execution::Admitted | Execution::Starting);
    let (status, ended) = if let Some(proof) = earlier_exit {
        proof
    } else if never_started {
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
        if let Some(done) = recognise_user_removal(ledger, repository, state_directory, attempt)? {
            return Ok(done);
        }
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
            started: !never_started && status != ExitStatus::FailedToStart,
            unowned_lock: earlier_exit.is_some(),
        },
        Reclamation::Blocked { uncovered } => kept(&format!(
            "{} could not be preserved automatically",
            uncovered.join(", ")
        )),
        Reclamation::Retained => kept("a person asked to keep it"),
    })
}

/// Recognises completed manual cleanup, never creates/removes a branch or a worktree.
fn recognise_user_removal(
    ledger: &mut EventStore,
    repository: &Path,
    state_directory: &Path,
    attempt: &AttemptId,
) -> Result<Option<Recovered>, GoalError> {
    let path = state_directory.join("worktrees").join(attempt.as_str());
    if path
        .try_exists()
        .map_err(at("inspecting the removed worktree"))?
    {
        return Ok(None);
    }
    let branch = format!("kaisa/{attempt}");
    let reference = format!("refs/heads/{branch}");
    let exists = Command::new("git")
        .args(["show-ref", "--verify", "--quiet", &reference])
        .current_dir(repository)
        .output()
        .map_err(at("finding the preserved branch"))?;
    if !exists.status.success() {
        return Ok(None);
    }
    let listing = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repository)
        .output()
        .map_err(at("listing remaining worktrees"))?;
    if !listing.status.success() {
        return Err(at("listing remaining worktrees")(String::from_utf8_lossy(
            &listing.stderr,
        )));
    }
    // A still-registered checkout may be in preparation or only temporarily unavailable.
    if String::from_utf8_lossy(&listing.stdout)
        .lines()
        .any(|line| line == format!("branch {reference}"))
    {
        return Ok(None);
    }
    record(
        ledger,
        EventKind::AttemptStateChanged,
        attempt.as_str(),
        serde_json::json!({
            "cleanup": Cleanup::Reclaimed.code(), "reason": "worktree-removed-by-user", "branch": branch,
        }),
    )?;
    Ok(Some(Recovered::RemovedByUser {
        attempt: attempt.clone(),
        branch,
    }))
}
