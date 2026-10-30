//! Per-attempt keeper: PTY master, process handle, exit record and execution broker.
//!
//! Design: `docs/architecture.md` §5.1.

mod durable;
mod exit_record;
mod run;
#[cfg(test)]
mod test_support;

pub use durable::{DurableError, exit_record_path, read_exit_record, write_exit_record};
pub use exit_record::{ExitRecord, RecordError};
pub use run::{AgentCommand, Assignment, KeeperError, run_to_exit};
