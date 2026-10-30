//! Attempt state axes, leases, keeper reconnection and resource reclamation.
//!
//! Design: `docs/architecture.md` §5.1 and §5.5.

mod lease;
mod reclaim;
mod recovery;
mod stall;

pub use lease::Lease;
pub use reclaim::{ReclaimError, Reclamation, reclaim};
pub use recovery::{Recovery, decide_recovery};
pub use stall::{ExpiryAction, on_lease_expired};
