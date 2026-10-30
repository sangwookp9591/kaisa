//! Event log, projections, outbox and signed checkpoints behind a single writer.
//!
//! Design: `docs/architecture.md` §5.4.

mod chain;
mod json;
mod store;

pub use chain::{BreakReason, ChainBreak, ChainHash, ChainedEvent, build_chain, verify_chain};
pub use store::{
    AttemptRow, Completion, CorruptReason, EventStore, Intent, PendingIntent, StoreError,
};
