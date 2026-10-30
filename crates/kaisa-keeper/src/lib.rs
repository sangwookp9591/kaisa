//! Per-attempt keeper: PTY master, process handle, exit record and execution broker.
//!
//! Design: `docs/architecture.md` §5.1.

mod exit_record;

pub use exit_record::{ExitRecord, RecordError};
