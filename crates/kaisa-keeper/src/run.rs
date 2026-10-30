//! Running an agent process to completion and recording how it ended.
//!
//! The agent runs as a [`kaisa_sys::OwnedProcess`]: a process group on Unix (held by a pidfd on
//! Linux) and a Job Object on Windows, so the keeper owns everything the agent starts. PTYs
//! arrive in T12.

use std::fmt;
use std::path::PathBuf;

use kaisa_domain::{AttemptId, ExitStatus};
use kaisa_sys::{OwnedProcess, ProcessExit, ProcessSpec};

use crate::durable::{DurableError, write_exit_record};
use crate::exit_record::ExitRecord;

/// The program a keeper runs for an attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCommand {
    /// The executable to start.
    pub program: PathBuf,
    /// Arguments, passed as is (no shell).
    pub args: Vec<String>,
    /// Working directory, normally the attempt's worktree.
    pub working_directory: PathBuf,
}

/// Which attempt and which start of it a keeper is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// The attempt.
    pub attempt: AttemptId,
    /// The incarnation of this start.
    pub incarnation: u64,
    /// Where the attempt's records live.
    pub attempt_directory: PathBuf,
}

/// Why the keeper could not run the agent to completion.
#[derive(Debug)]
#[non_exhaustive]
pub enum KeeperError {
    /// The program could not be started. Nothing ran, so no exit record is written.
    Spawn(std::io::Error),
    /// Waiting for the process failed.
    Wait(std::io::Error),
    /// The exit record could not be written.
    Record(DurableError),
}

impl fmt::Display for KeeperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "could not start the agent: {error}"),
            Self::Wait(error) => write!(f, "could not wait for the agent: {error}"),
            Self::Record(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for KeeperError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) | Self::Wait(error) => Some(error),
            Self::Record(error) => Some(error),
        }
    }
}

/// Starts `command`, waits for it to exit, durably records how it ended and returns the
/// record. `now_ms` supplies the finish time, so tests control the clock.
///
/// # Errors
///
/// Returns [`KeeperError::Spawn`] if the program cannot be started, [`KeeperError::Wait`] if
/// waiting fails, or [`KeeperError::Record`] if the record cannot be written.
pub fn run_to_exit(
    command: &AgentCommand,
    assignment: &Assignment,
    now_ms: impl Fn() -> u64,
) -> Result<ExitRecord, KeeperError> {
    let mut agent: OwnedProcess = OwnedProcess::spawn(&ProcessSpec {
        program: command.program.clone(),
        args: command.args.clone(),
        working_directory: command.working_directory.clone(),
        env: Vec::new(),
    })
    .map_err(KeeperError::Spawn)?;
    let exit: ProcessExit = agent.wait().map_err(KeeperError::Wait)?;
    let record = ExitRecord {
        attempt: assignment.attempt.clone(),
        incarnation: assignment.incarnation,
        status: match exit {
            ProcessExit::Code(code) => ExitStatus::Code(code),
            ProcessExit::Terminated => ExitStatus::Terminated,
        },
        finished_at_ms: now_ms(),
    };
    write_exit_record(&assignment.attempt_directory, &record).map_err(KeeperError::Record)?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kaisa_domain::{AttemptId, ExitStatus};

    use super::{AgentCommand, Assignment, KeeperError, run_to_exit};
    use crate::durable::{exit_record_path, read_exit_record};
    use crate::exit_record::ExitRecord;
    use crate::test_support::{ScratchAttemptDirectory, an_empty_attempt_directory};

    /// The exit code a typical CLI agent uses for "I failed".
    const AGENT_FAILURE_CODE: i32 = 1;
    /// The incarnation of a first start.
    const FIRST_INCARNATION: u64 = 1;
    /// The keeper's clock reading when the agent exits in these tests.
    const EXIT_OBSERVED_AT_MS: u64 = 1_793_361_600_000;

    fn a_frozen_clock() -> u64 {
        EXIT_OBSERVED_AT_MS
    }

    /// An agent that does nothing and exits with `code`, on any operating system.
    fn an_agent_that_exits_with(code: i32, directory: &ScratchAttemptDirectory) -> AgentCommand {
        let (program, args): (&str, Vec<String>) = if cfg!(windows) {
            ("cmd", vec!["/C".to_owned(), format!("exit {code}")])
        } else {
            ("sh", vec!["-c".to_owned(), format!("exit {code}")])
        };
        AgentCommand {
            program: PathBuf::from(program),
            args,
            working_directory: directory.path().to_owned(),
        }
    }

    /// A command whose program does not exist anywhere.
    fn an_agent_that_is_not_installed(directory: &ScratchAttemptDirectory) -> AgentCommand {
        AgentCommand {
            program: PathBuf::from("kaisa-test-agent-that-does-not-exist"),
            args: Vec::new(),
            working_directory: directory.path().to_owned(),
        }
    }

    fn the_first_start_of_attempt_41a(directory: &ScratchAttemptDirectory) -> Assignment {
        Assignment {
            attempt: AttemptId::try_new("att_41a").expect("a valid attempt id"),
            incarnation: FIRST_INCARNATION,
            attempt_directory: directory.path().to_owned(),
        }
    }

    fn run(command: &AgentCommand, directory: &ScratchAttemptDirectory) -> ExitRecord {
        run_to_exit(
            command,
            &the_first_start_of_attempt_41a(directory),
            a_frozen_clock,
        )
        .expect("the agent runs to completion")
    }

    #[test]
    fn a_child_that_exits_successfully_leaves_a_success_exit_record() {
        let directory: ScratchAttemptDirectory = an_empty_attempt_directory();

        let record: ExitRecord = run(
            &an_agent_that_exits_with(ExitStatus::SUCCESS_CODE, &directory),
            &directory,
        );

        assert!(
            record.status.is_success(),
            "a zero exit is recorded as success"
        );
    }

    #[test]
    fn a_child_that_fails_leaves_its_failure_code_in_the_exit_record() {
        let directory: ScratchAttemptDirectory = an_empty_attempt_directory();

        let record: ExitRecord = run(
            &an_agent_that_exits_with(AGENT_FAILURE_CODE, &directory),
            &directory,
        );

        assert_eq!(
            record.status,
            ExitStatus::Code(AGENT_FAILURE_CODE),
            "the exact code is kept so failures can be told apart"
        );
    }

    #[test]
    fn the_returned_record_is_the_one_written_to_disk() {
        let directory: ScratchAttemptDirectory = an_empty_attempt_directory();
        let assignment: Assignment = the_first_start_of_attempt_41a(&directory);

        let record: ExitRecord = run(
            &an_agent_that_exits_with(AGENT_FAILURE_CODE, &directory),
            &directory,
        );

        assert_eq!(
            read_exit_record(&exit_record_path(directory.path(), assignment.incarnation))
                .expect("the record is on disk"),
            record,
            "callers and recovery see the same facts"
        );
    }

    #[test]
    fn the_exit_record_names_the_attempt_incarnation_and_finish_time() {
        let directory: ScratchAttemptDirectory = an_empty_attempt_directory();
        let assignment: Assignment = the_first_start_of_attempt_41a(&directory);

        let record: ExitRecord = run(
            &an_agent_that_exits_with(ExitStatus::SUCCESS_CODE, &directory),
            &directory,
        );

        assert_eq!(
            record.attempt, assignment.attempt,
            "the record belongs to its attempt"
        );
        assert_eq!(
            record.incarnation, assignment.incarnation,
            "and to this start of it"
        );
        assert_eq!(
            record.finished_at_ms, EXIT_OBSERVED_AT_MS,
            "stamped by the keeper's clock"
        );
    }

    #[test]
    fn a_program_that_cannot_be_started_leaves_no_exit_record() {
        let directory: ScratchAttemptDirectory = an_empty_attempt_directory();
        let assignment: Assignment = the_first_start_of_attempt_41a(&directory);

        let result = run_to_exit(
            &an_agent_that_is_not_installed(&directory),
            &assignment,
            a_frozen_clock,
        );

        assert!(
            matches!(result, Err(KeeperError::Spawn(_))),
            "starting failed, got {result:?}"
        );
        assert!(
            !exit_record_path(directory.path(), assignment.incarnation).exists(),
            "nothing ran, so nothing may claim that it exited"
        );
    }
}
