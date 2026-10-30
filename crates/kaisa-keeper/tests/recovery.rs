//! Recovery after the keeper is gone: only the exit record on disk is left.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use kaisa_domain::{AttemptId, AttemptState, Execution, ExitStatus};
use kaisa_keeper::{
    AgentCommand, Assignment, ExitRecord, exit_record_path, read_exit_record, run_to_exit,
};

/// Distinguishes directories created by tests running in parallel.
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
/// The exit code a typical CLI agent uses for "I failed".
const AGENT_FAILURE_CODE: i32 = 1;
/// The incarnation of a first start.
const FIRST_INCARNATION: u64 = 1;
/// The keeper's clock reading when the agent exits in these tests.
const EXIT_OBSERVED_AT_MS: u64 = 1_793_361_600_000;

/// An attempt directory in the temp folder, removed when dropped.
struct ScratchAttemptDirectory(PathBuf);

impl Drop for ScratchAttemptDirectory {
    fn drop(&mut self) {
        // Best effort: a leftover temp directory must not fail the test that used it.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn an_empty_attempt_directory() -> ScratchAttemptDirectory {
    let path: PathBuf = std::env::temp_dir().join(format!(
        "kaisa-keeper-recovery-{}-{}",
        std::process::id(),
        NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).expect("the temp directory is writable");
    ScratchAttemptDirectory(path)
}

/// An agent that does nothing and exits with `code`, on any operating system.
fn an_agent_that_exits_with(code: i32, working_directory: &Path) -> AgentCommand {
    let (program, args): (&str, Vec<String>) = if cfg!(windows) {
        ("cmd", vec!["/C".to_owned(), format!("exit {code}")])
    } else {
        ("sh", vec!["-c".to_owned(), format!("exit {code}")])
    };
    AgentCommand {
        program: PathBuf::from(program),
        args,
        working_directory: working_directory.to_owned(),
    }
}

fn the_first_start_of_attempt_41a(directory: &Path) -> Assignment {
    Assignment {
        attempt: AttemptId::try_new("att_41a").expect("a valid attempt id"),
        incarnation: FIRST_INCARNATION,
        attempt_directory: directory.to_owned(),
    }
}

/// A keeper runs the agent and then disappears (its process ends; only the disk remains).
fn a_keeper_ran_the_agent_and_is_gone(directory: &Path, exit_code: i32) {
    run_to_exit(
        &an_agent_that_exits_with(exit_code, directory),
        &the_first_start_of_attempt_41a(directory),
        || EXIT_OBSERVED_AT_MS,
    )
    .expect("the agent runs to completion");
}

/// The daemon's view of an attempt whose start it lost track of.
fn an_attempt_the_daemon_lost_track_of() -> AttemptState {
    let mut attempt = AttemptState::admitted();
    attempt.start().expect("an admitted attempt can start");
    attempt
        .lose_track()
        .expect("contact can be lost while starting");
    attempt
}

#[test]
fn an_exit_record_can_be_read_after_the_keeper_is_gone() {
    let directory = an_empty_attempt_directory();
    a_keeper_ran_the_agent_and_is_gone(&directory.0, AGENT_FAILURE_CODE);

    let record: ExitRecord = read_exit_record(&exit_record_path(&directory.0, FIRST_INCARNATION))
        .expect("the record survives the keeper");

    assert_eq!(
        record.status,
        ExitStatus::Code(AGENT_FAILURE_CODE),
        "the daemon learns how the agent ended without the keeper being alive"
    );
}

#[test]
fn an_exit_record_on_disk_resolves_an_attempt_the_daemon_lost_track_of() {
    let directory = an_empty_attempt_directory();
    a_keeper_ran_the_agent_and_is_gone(&directory.0, ExitStatus::SUCCESS_CODE);
    let mut attempt: AttemptState = an_attempt_the_daemon_lost_track_of();
    let record: ExitRecord = read_exit_record(&exit_record_path(&directory.0, FIRST_INCARNATION))
        .expect("the record survives the keeper");

    attempt
        .record_exit(record.status)
        .expect("an exit record is proof of exit");

    assert_eq!(
        attempt.execution(),
        Execution::Exited(record.status),
        "proof on disk is the only way out of Unknown, and it is enough"
    );
}
