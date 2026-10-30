//! The per-attempt egress broker (architecture §5.7 "Egress broker", review M4).
//!
//! Sandboxed agents can reach only this proxy. It tunnels HTTPS (`CONNECT`) to hosts on the
//! attempt's allow-list, and refuses any name that resolves to an address on the host's own
//! networks — loopback, private, link-local — so no allow-listed name can be pointed back at
//! the control endpoint, the web console or another attempt (DNS rebinding).

mod address;
mod allow_list;
mod broker;

pub use address::is_host_internal;
pub use allow_list::AllowList;
pub use broker::{Decision, EgressBroker, Network, SystemNetwork, decide};
