//! The SQLite-backed event store: the single writer of the ledger (architecture §5.4).

mod error;
mod event_store;
mod rows;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

pub use error::{CorruptReason, StoreError};
pub use event_store::EventStore;
