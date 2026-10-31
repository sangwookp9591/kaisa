//! The OS sandbox an agent's own process runs in (architecture §5.6, T16.5e3): it writes only
//! its scratch and the paths its CLI needs, reaches only its tool broker's socket, and reaches
//! the network only through its attempt's egress broker.

#[cfg(any(unix, windows))]
use std::path::{Path, PathBuf};

use kaisa_domain::ContainmentCapability;
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
                // Granted to the shared capability and never revoked: idempotent, and no
                // other attempt's access can be lost to it.
                self.programs.grant(directory, ReadExecute)?;
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

    /// What this sandbox cannot guarantee. Unix PPID scans cannot discover descendants
    /// already reparented outside the tree, so descendant containment is always reduced.
    pub(crate) fn missing(&self) -> Vec<ContainmentCapability> {
        match self {
            Self::Unconfined => ContainmentCapability::ALL.to_vec(),
            #[cfg(target_os = "macos")]
            Self::Seatbelt(_) => vec![ContainmentCapability::DescendantContainment],
            #[cfg(target_os = "linux")]
            Self::Landlock(ruleset) => {
                kaisa_domain::landlock_missing(Some(ruleset.abi), ruleset.udp_confined())
            }
            #[cfg(windows)]
            Self::AppContainer(_) => Vec::new(),
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
    state_directory: &Path,
    worktree: &Path,
    scratch: &Path,
    agent_writable: &[PathBuf],
    broker_socket: &Path,
    egress_port: Option<u16>,
    needs_keychain_login: bool,
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
        let attempt = scratch.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "scratch has no attempt directory",
            )
        })?;
        let mut readable = vec![worktree.canonicalize()?, scratch.canonicalize()?];
        for file in [
            "mcp.json",
            kaisa_mcp::BRIDGE_CONFIG_FILE,
            kaisa_mcp::TOKEN_FILE,
            kaisa_mcp::BROKER_ENDPOINT_FILE,
        ] {
            readable.push(attempt.join(file));
        }
        let mut policy = kaisa_sys::SeatbeltPolicy {
            readable,
            writable,
            needs_keychain_login,
            broker_socket: Some(socket),
            broker_port: egress_port,
            ..kaisa_sys::SeatbeltPolicy::default()
        };
        policy.protect_state(state_directory)?;
        Ok(AgentSandbox::Seatbelt(policy))
    }
    #[cfg(target_os = "linux")]
    {
        let _ = (
            state_directory,
            worktree,
            broker_socket,
            needs_keychain_login,
        );
        let existing: Vec<PathBuf> = writable.into_iter().filter(|path| path.exists()).collect();
        Ok(match kaisa_sys::write_ruleset(&existing, egress_port)? {
            Some(ruleset) => AgentSandbox::Landlock(ruleset),
            None => AgentSandbox::Unconfined,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (
            state_directory,
            worktree,
            writable,
            broker_socket,
            egress_port,
            needs_keychain_login,
        );
        Ok(AgentSandbox::Unconfined)
    }
}

/// What an attempt will be missing before its sandbox is built: everything if it is not to be
/// confined, otherwise what this machine's sandbox cannot give. The sandbox actually built is
/// checked again before the agent starts ([`AgentSandbox::missing`]).
pub fn expected_missing(confine: bool) -> Vec<ContainmentCapability> {
    if !confine {
        return ContainmentCapability::ALL.to_vec();
    }
    #[cfg(target_os = "linux")]
    {
        kaisa_domain::landlock_missing(kaisa_sys::landlock_abi(), kaisa_sys::udp_filter_supported())
    }
    #[cfg(target_os = "macos")]
    {
        vec![ContainmentCapability::DescendantContainment]
    }
    #[cfg(windows)]
    {
        Vec::new()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        ContainmentCapability::ALL.to_vec()
    }
}

/// What `actual` lacks beyond what the attempt was admitted without: a sandbox that turned out
/// weaker than admitted must not start (D02).
pub(crate) fn beyond_admitted(
    actual: &[ContainmentCapability],
    admitted: &[ContainmentCapability],
) -> Vec<ContainmentCapability> {
    actual
        .iter()
        .copied()
        .filter(|capability| !admitted.contains(capability))
        .collect()
}

#[cfg(test)]
mod tests {
    use kaisa_domain::ContainmentCapability;

    use super::{AgentSandbox, beyond_admitted, expected_missing};

    /// Commands whose logical path resolution walks the protected state ancestors.
    #[cfg(target_os = "macos")]
    fn commands_walking_state_ancestors(
        directory: &std::path::Path,
    ) -> Vec<kaisa_sys::ProcessSpec> {
        std::fs::create_dir_all(directory.join("sub")).expect("nested checkout directory");
        let mut commands = vec![kaisa_sys::ProcessSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "cd sub && pwd".into()],
            working_directory: directory.to_owned(),
            env: Vec::new(),
        }];
        // Node is optional on developer hosts; the shell regression always runs.
        if let Some(node) = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|path| path.join("node"))
                .find(|path| path.is_file())
        }) {
            commands.push(kaisa_sys::ProcessSpec {
                program: node,
                args: vec!["-e".into(), "require('fs').realpathSync('.')".into()],
                working_directory: directory.to_owned(),
                env: Vec::new(),
            });
        }
        commands
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_confined_agent_reads_its_bridge_and_worktree_but_not_daemon_or_other_attempt_secrets() {
        const SUCCESS: kaisa_sys::ProcessExit = kaisa_sys::ProcessExit::Code(0);
        use kaisa_sys::{ProcessSpec, run_captured};
        let root =
            std::env::temp_dir().join(format!("kaisa-agent-read-isolation-{}", std::process::id()));
        let state = root.join("state");
        let attempt = state.join("attempts/own");
        let other = state.join("attempts/other");
        let scratch = attempt.join("scratch");
        let worktree = state.join("worktrees/own");
        for directory in [&scratch, &worktree, &other] {
            std::fs::create_dir_all(directory).expect("isolated state fixture");
        }
        let allowed = [
            attempt.join("bridge.token"),
            attempt.join("bridge.json"),
            attempt.join("mcp.json"),
            attempt.join("broker.endpoint"),
            worktree.join("source.rs"),
        ];
        let denied = [
            state.join("kaisa.db"),
            other.join("bridge.token"),
            attempt.join("private.log"),
        ];
        for file in allowed.iter().chain(&denied) {
            std::fs::write(file, "fixture contents").expect("planted fixture");
        }
        let sandbox = super::agent_sandbox(
            &state,
            &worktree,
            &scratch,
            &[],
            &attempt.join("broker.sock"),
            None,
            false,
        )
        .expect("agent sandbox");
        let read = |path: &std::path::Path| {
            let spec = ProcessSpec {
                program: "/bin/cat".into(),
                args: vec![path.to_string_lossy().into_owned()],
                working_directory: worktree.clone(),
                env: Vec::new(),
            };
            run_captured(
                &spec,
                &sandbox.confinement(),
                b"",
                std::time::Duration::from_secs(10),
                4096,
            )
            .expect("confined cat exits")
            .exit
            .expect("cat exits before deadline")
        };
        for secret in &denied {
            assert_ne!(
                read(secret),
                SUCCESS,
                "agent constructor must hide {secret:?}"
            );
        }
        for file in allowed
            .iter()
            .chain([&std::path::PathBuf::from("/etc/hosts")])
        {
            assert_eq!(
                read(file),
                SUCCESS,
                "the relay and agent still need {file:?}"
            );
        }
        let claude_sandbox = super::agent_sandbox(
            &state,
            &worktree,
            &scratch,
            &[],
            &attempt.join("broker.sock"),
            None,
            true,
        )
        .expect("Claude sandbox");
        let super::AgentSandbox::Seatbelt(claude_policy) = &claude_sandbox else {
            panic!("macOS uses Seatbelt");
        };
        assert!(
            claude_policy.needs_keychain_login,
            "only the adapter-declared Claude agent receives login keychain access"
        );
        let profile = kaisa_sys::seatbelt_profile(claude_policy);
        assert!(
            !profile.contains("(deny mach-lookup") && !profile.contains("Library/Keychains"),
            "Claude login must retain keychain services and file reads"
        );
        let secret_read = ProcessSpec {
            program: "/bin/cat".into(),
            args: vec![denied[0].to_string_lossy().into_owned()],
            working_directory: worktree.clone(),
            env: Vec::new(),
        };
        let result = run_captured(
            &secret_read,
            &claude_sandbox.confinement(),
            b"",
            std::time::Duration::from_secs(10),
            4096,
        )
        .expect("confined secret read");
        assert_ne!(
            result.exit,
            Some(SUCCESS),
            "Claude login access must not reopen Kaisa state"
        );
        let alias = worktree.join("stolen-token");
        std::os::unix::fs::symlink(&denied[1], &alias).expect("hostile symlink");
        assert_ne!(
            read(&alias),
            SUCCESS,
            "a worktree symlink must not reopen state"
        );
        for command in commands_walking_state_ancestors(&worktree) {
            let result = kaisa_sys::run_captured(
                &command,
                &sandbox.confinement(),
                b"",
                std::time::Duration::from_secs(10),
                4096,
            )
            .expect("path-walking command exits");
            assert_eq!(
                result.exit,
                Some(SUCCESS),
                "ancestor metadata must permit {:?}: {}",
                command.args,
                String::from_utf8_lossy(&result.output)
            );
        }
        std::fs::remove_dir_all(root).expect("remove isolated state");
    }

    #[test]
    fn an_unconfined_sandbox_is_missing_every_capability() {
        assert_eq!(
            AgentSandbox::Unconfined.missing(),
            ContainmentCapability::ALL.to_vec(),
            "a run without an OS sandbox must never be labelled confined"
        );
    }

    #[test]
    fn an_attempt_not_to_be_confined_expects_to_miss_every_capability() {
        assert_eq!(
            expected_missing(false),
            ContainmentCapability::ALL.to_vec(),
            "admission sees the whole gap before the agent starts"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_seatbelt_sandbox_reports_the_same_descendant_limit_before_and_after_construction() {
        let actual = AgentSandbox::Seatbelt(kaisa_sys::SeatbeltPolicy::default()).missing();
        assert_eq!(
            actual,
            vec![ContainmentCapability::DescendantContainment],
            "PPID scans cannot promise cleanup of already reparented daemons"
        );
        assert_eq!(
            expected_missing(true),
            actual,
            "admission must disclose exactly the capabilities the sandbox lacks"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_job_objects_do_not_report_the_unix_descendant_limit() {
        assert!(
            !expected_missing(true).contains(&ContainmentCapability::DescendantContainment),
            "Job Object breakaway is denied, unlike Unix process groups"
        );
    }

    #[test]
    fn a_sandbox_weaker_than_admitted_shows_what_it_lacks_beyond_the_admission() {
        let admitted = vec![ContainmentCapability::UdpEgress];
        let actual = vec![
            ContainmentCapability::TcpEgress,
            ContainmentCapability::UdpEgress,
        ];

        assert_eq!(
            beyond_admitted(&actual, &admitted),
            vec![ContainmentCapability::TcpEgress],
            "only what nobody accepted stops the start"
        );
    }
}
