//! The SQLite-backed event store: the single writer of the ledger (architecture §5.4).

mod attempts;
#[cfg(test)]
mod attempts_tests;
mod context_packs;
mod error;
mod event_store;
mod knowledge;
mod outbox;
#[cfg(test)]
mod outbox_tests;
mod policy;
mod routing;
mod rows;
/// Content-addressed specifications and approval receipts.
pub mod spec;
mod tasks;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
mod usage;

pub use attempts::AttemptRow;
pub use error::{CorruptReason, StoreError};
pub use event_store::EventStore;
pub use knowledge::TrustedKnowledge;
pub use outbox::{Completion, Intent, PendingIntent};
pub use policy::AdoptedPolicy;
pub use routing::RoutingObservation;
pub use tasks::TaskProgress;
pub use usage::AttemptUsage;
