//! The OS sandbox an agent's own process runs in (architecture §5.6, T16.5e3): it writes only
//! its scratch and the paths its CLI needs, reaches only its tool broker's socket, and reaches
//! the network only through its attempt's egress broker.

#[cfg(any(unix, windows))]
use std::path::{Path, PathBuf};

use kaisa_sys::Confinement;

/// The OS sandbox the agent's own process runs in.
pub(crate) enum AgentSandbox {
    Unconfined,
    #[cfg(target_os = "macos")]
    Seatbelt(kaisa_sys::SeatbeltPolicy),
    #[cfg(target_os = "linux")]
    Landlock(kaisa_sys::LandlockRuleset),
    #[cfg(windows)]
    AppContainer(ContainedAttempt),
}

/// A Windows attempt's AppContainer and the paths granted to it, released after the run.
#[cfg(windows)]
pub(crate) struct ContainedAttempt {
    profile: kaisa_sys::AppContainerProfile,
    programs: std::sync::Arc<kaisa_sys::CapabilitySid>,
    granted: Vec<PathBuf>,
}

/// The capability every Kaisa container holds; shared program directories are granted to it
/// once, so attempts never rewrite a shared directory's DACL per run (T15.5e).
#[cfg(windows)]
const AGENT_PROGRAMS_CAPABILITY: &str = "kaisaAgentPrograms";

#[cfg(windows)]
impl ContainedAttempt {
    /// Creates the attempt's container and records its SID in `attempt_directory`, so the
    /// broker created next admits it (T15.5b2).
    pub(crate) fn create(attempt_directory: &Path, name: &str) -> std::io::Result<Self> {
        let programs =
            std::sync::Arc::new(kaisa_sys::CapabilitySid::derive(AGENT_PROGRAMS_CAPABILITY)?);
        let profile = kaisa_sys::AppContainerProfile::ensure(name)?
            .with_capability(std::sync::Arc::clone(&programs));
        std::fs::write(
            attempt_directory.join(kaisa_mcp::CONTAINER_SID_FILE),
            profile.sid_string()?,
        )?;
        Ok(Self {
            profile,
            programs,
            granted: Vec::new(),
        })
    }

    fn grant(&mut self, path: &Path, access: kaisa_sys::ContainerAccess) -> std::io::Result<()> {
        self.profile.grant(path, access)?;
        self.granted.push(path.to_owned());
        Ok(())
    }

    /// Grants what the agent needs and nothing more: read its worktree, its own programs and
    /// its attempt directory (MCP config, token, broker name); change only its scratch. Writes
    /// to the worktree go through the broker.
    pub(crate) fn grant_for_agent(
        &mut self,
        worktree: &Path,
        attempt_directory: &Path,
        scratch: &Path,
        programs: &[&Path],
    ) -> std::io::Result<()> {
        use kaisa_sys::ContainerAccess::{Modify, ReadExecute};
        self.grant(worktree, ReadExecute)?;
        self.grant(attempt_directory, ReadExecute)?;
        self.grant(scratch, Modify)?;
        for program in programs {
            // A bare program name ("claude") is found on PATH; there is no directory to grant.
            if let Some(directory) = program.parent().filter(|path| !path.as_os_str().is_empty()) {
                self.grant(directory, ReadExecute)?;
            }
        }
        Ok(())
    }

    /// Revokes every grant and deletes the container.
    pub(crate) fn release(self) {
        for path in &self.granted {
            let _ = self.profile.revoke(path);
        }
        let _ = self.profile.delete();
    }
}

impl AgentSandbox {
    pub(crate) fn confinement(&self) -> Confinement<'_> {
        match self {
            Self::Unconfined => Confinement::Unconfined,
            #[cfg(target_os = "macos")]
            Self::Seatbelt(policy) => Confinement::Seatbelt(policy),
            #[cfg(target_os = "linux")]
            Self::Landlock(ruleset) => Confinement::Landlock(ruleset),
            #[cfg(windows)]
            Self::AppContainer(contained) => Confinement::AppContainer(&contained.profile),
        }
    }

    /// Releases what the sandbox granted on the way out (Windows ACEs and profile).
    pub(crate) fn release(self) {
        #[cfg(windows)]
        if let Self::AppContainer(contained) = self {
            contained.release();
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
