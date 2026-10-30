//! The OS sandbox an agent's own process runs in (architecture §5.6, T16.5e3): it writes only
//! its scratch and the paths its CLI needs, reaches only its tool broker's socket, and reaches
//! the network only through its attempt's egress broker.

use std::path::{Path, PathBuf};

use kaisa_sys::Confinement;

/// The OS sandbox the agent's own process runs in.
pub(crate) enum AgentSandbox {
    Unconfined,
    #[cfg(target_os = "macos")]
    Seatbelt(kaisa_sys::SeatbeltPolicy),
    #[cfg(target_os = "linux")]
    Landlock(kaisa_sys::LandlockRuleset),
}

impl AgentSandbox {
    pub(crate) fn confinement(&self) -> Confinement<'_> {
        match self {
            Self::Unconfined => Confinement::Unconfined,
            #[cfg(target_os = "macos")]
            Self::Seatbelt(policy) => Confinement::Seatbelt(policy),
            #[cfg(target_os = "linux")]
            Self::Landlock(ruleset) => Confinement::Landlock(ruleset),
        }
    }
}

/// The agent writes nothing but its scratch, reaches nothing but its tool broker's socket, and
/// has no network: every change it makes goes through the broker.
#[cfg(unix)]
pub(crate) fn agent_sandbox(
    scratch: &Path,
    agent_writable: &[PathBuf],
    broker_socket: &Path,
    egress_port: Option<u16>,
) -> std::io::Result<AgentSandbox> {
    let mut writable: Vec<PathBuf> = vec![scratch.canonicalize()?];
    // A path the agent may create (a session file) need not exist yet.
    writable.extend(
        agent_writable
            .iter()
            .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone())),
    );
    #[cfg(target_os = "macos")]
    {
        // Seatbelt matches canonical paths; the socket itself may not resolve, its directory does.
        let socket: PathBuf = match (broker_socket.parent(), broker_socket.file_name()) {
            (Some(directory), Some(name)) => directory.canonicalize()?.join(name),
            _ => broker_socket.to_owned(),
        };
        Ok(AgentSandbox::Seatbelt(kaisa_sys::SeatbeltPolicy {
            writable,
            broker_socket: Some(socket),
            broker_port: egress_port,
            ..kaisa_sys::SeatbeltPolicy::default()
        }))
    }
    #[cfg(target_os = "linux")]
    {
        let _ = broker_socket;
        let existing: Vec<PathBuf> = writable.into_iter().filter(|path| path.exists()).collect();
        Ok(match kaisa_sys::write_ruleset(&existing, egress_port)? {
            Some(ruleset) => AgentSandbox::Landlock(ruleset),
            None => AgentSandbox::Unconfined,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (writable, broker_socket, egress_port);
        Ok(AgentSandbox::Unconfined)
    }
}
