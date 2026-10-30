//! Attempt state axes, leases, keeper reconnection and resource reclamation.
//!
//! Design: `docs/architecture.md` §5.1 and §5.5.

mod lease;
mod recovery;

pub use lease::Lease;
pub use recovery::{Recovery, decide_recovery};
