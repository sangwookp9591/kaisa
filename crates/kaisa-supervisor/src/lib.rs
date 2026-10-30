//! Attempt state axes, leases, keeper reconnection and resource reclamation.
//!
//! Design: `docs/architecture.md` §5.1 and §5.5.

mod lease;

pub use lease::Lease;
