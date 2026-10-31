//! The bridge for one attempt, configured from its attempt directory (architecture §3: "per-attempt
//! stdio MCP bridge", launched by the agent from its MCP config).
//!
//! Kaisa writes `bridge.json` into the attempt directory before starting the agent; the agent's
//! MCP config runs `kaisa mcp --attempt <dir>`, which reads it and serves only that attempt's
//! tools, in that attempt's sandbox.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use kaisa_domain::{RepoPath, Role, WriteSet, sandbox_profile};
use serde_json::{Value, json};

use crate::broker::{ExecSandbox, FileBroker};
use crate::server::McpServer;
use crate::tools::tools_for;

/// The file in an attempt directory that configures its bridge.
pub const BRIDGE_CONFIG_FILE: &str = "bridge.json";

/// What one attempt's bridge serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfig {
    /// The attempt's worktree.
    pub worktree: PathBuf,
    /// The paths the attempt may change.
    pub write_set: WriteSet,
    /// The attempt's role, which decides its tools and sandbox.
    pub role: Role,
    /// Where build output and caches may go, outside the worktree.
    pub scratch: Option<PathBuf>,
    /// The loopback port of the attempt's egress broker.
    pub broker_port: Option<u16>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{BRIDGE_CONFIG_FILE}: {message}"),
    )
}

impl BridgeConfig {
    /// Writes the config into `attempt_directory`.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the file cannot be written.
    pub fn write_to(&self, attempt_directory: &Path) -> io::Result<()> {
        let config: Value = json!({
            "worktree": self.worktree.to_string_lossy(),
            "write_set": self.write_set.entries().iter().map(RepoPath::as_str).collect::<Vec<_>>(),
            "role": self.role.code(),
            "scratch": self.scratch.as_ref().map(|path| path.to_string_lossy().into_owned()),
            "broker_port": self.broker_port,
        });
        std::fs::write(
            attempt_directory.join(BRIDGE_CONFIG_FILE),
            config.to_string(),
        )
    }

    /// Reads the config from `attempt_directory`.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the file is missing, or `InvalidData` if it is malformed.
    pub fn load(attempt_directory: &Path) -> io::Result<Self> {
        let text: String = std::fs::read_to_string(attempt_directory.join(BRIDGE_CONFIG_FILE))?;
        let config: Value = serde_json::from_str(&text).map_err(|_| invalid("not JSON"))?;
        let worktree: PathBuf = config["worktree"]
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| invalid("missing worktree"))?;
        let role: Role = config["role"]
            .as_str()
            .and_then(Role::from_code)
            .ok_or_else(|| invalid("unknown role"))?;
        let entries: Vec<RepoPath> = config["write_set"]
            .as_array()
            .ok_or_else(|| invalid("missing write_set"))?
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .and_then(|text| RepoPath::try_new(text).ok())
                    .ok_or_else(|| invalid("bad write_set entry"))
            })
            .collect::<io::Result<_>>()?;
        Ok(Self {
            worktree,
            write_set: WriteSet::new(entries),
            role,
            scratch: config["scratch"].as_str().map(PathBuf::from),
            broker_port: config["broker_port"]
                .as_u64()
                .and_then(|port| u16::try_from(port).ok()),
        })
    }

    /// Canonical grants must remain in this worktree. Additive backends cannot carve
    /// root controls out of a root grant; Seatbelt instead uses an overriding deny.
    fn exec_writable(&self) -> io::Result<Vec<PathBuf>> {
        self.exec_writable_with_root_deny(cfg!(target_os = "macos"))
    }

    fn exec_writable_with_root_deny(
        &self,
        can_deny_root_controls: bool,
    ) -> io::Result<Vec<PathBuf>> {
        let root = self.worktree.canonicalize()?;
        let resolver = FileBroker::new(&root, self.write_set.clone())?;
        let mut writable = Vec::new();
        for path in sandbox_profile(self.role, &self.write_set).writable {
            let target = resolver
                .resolve(&path)
                .map_err(|reason| io::Error::new(io::ErrorKind::PermissionDenied, reason))?;
            let relative = target
                .strip_prefix(&root)
                .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error))?;
            let root_control = relative
                .components()
                .next()
                .and_then(|name| name.as_os_str().to_str())
                .is_some_and(|name| {
                    [".git", ".gitattributes", ".gitmodules"]
                        .iter()
                        .any(|control| name.eq_ignore_ascii_case(control))
                });
            if root_control
                || (!can_deny_root_controls && target == root)
                || (target.is_file()
                    && crate::broker::existing_link_count(&target)?.is_none_or(|links| links > 1))
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "exec cannot safely exclude root Git controls from this write grant; use fs_write",
                ));
            }
            writable.push(target);
        }
        if let Some(scratch) = &self.scratch {
            let scratch = scratch.canonicalize()?;
            if scratch.starts_with(&root) || root.starts_with(&scratch) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "exec scratch must be outside and must not contain the worktree",
                ));
            }
            writable.push(scratch);
        }
        Ok(writable)
    }

    /// The exec sandbox this attempt gets on this OS.
    #[cfg(not(windows))]
    fn exec_sandbox(&self) -> io::Result<ExecSandbox> {
        let writable = match self.exec_writable() {
            Ok(writable) => writable,
            Err(error) => return Ok(ExecSandbox::Unavailable(error.to_string())),
        };
        #[cfg(target_os = "macos")]
        {
            Ok(ExecSandbox::Seatbelt(kaisa_sys::SeatbeltPolicy {
                writable,
                git_control_root: Some(self.worktree.canonicalize()?),
                broker_port: self.broker_port,
                ..kaisa_sys::SeatbeltPolicy::default()
            }))
        }
        #[cfg(target_os = "linux")]
        {
            Ok(
                match kaisa_sys::write_ruleset(&writable, self.broker_port) {
                    Ok(Some(ruleset)) => ExecSandbox::Landlock(ruleset),
                    Ok(None) => unavailable_exec("Linux Landlock is not supported"),
                    Err(error) => {
                        unavailable_exec(&format!("Linux Landlock setup failed: {error}"))
                    }
                },
            )
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = writable;
            Ok(unavailable_exec("this OS has no supported exec sandbox"))
        }
    }
}

#[cfg(any(test, not(any(target_os = "macos", windows))))]
pub(crate) fn unavailable_exec(reason: &str) -> ExecSandbox {
    ExecSandbox::Unavailable(format!("exec requires OS sandbox isolation: {reason}"))
}

/// Serves the bridge of the attempt in `attempt_directory` from `input` to `output` until the
/// agent closes its end.
///
/// # Errors
///
/// Returns the I/O error if the config cannot be read or the stream breaks.
pub fn serve_attempt(
    attempt_directory: &Path,
    input: impl BufRead,
    output: &mut dyn Write,
) -> io::Result<()> {
    attempt_server(
        &BridgeConfig::load(attempt_directory)?,
        #[cfg(windows)]
        verified_container_sid(attempt_directory)?.as_deref(),
    )?
    .serve(input, output)
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}")))
}

/// Reject a valid package SID belonging to a different attempt or application.
#[cfg(any(test, windows))]
fn require_attempt_sid(recorded: &str, expected: &str) -> io::Result<()> {
    if recorded != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "recorded AppContainer SID does not belong to this attempt",
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn verified_container_sid(attempt_directory: &Path) -> io::Result<Option<String>> {
    let recorded = match std::fs::read_to_string(attempt_directory.join(crate::CONTAINER_SID_FILE))
    {
        Ok(recorded) => recorded,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let attempt = attempt_directory
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| kaisa_domain::AttemptId::try_new(name).ok())
        .ok_or_else(|| invalid("invalid attempt directory identity"))?;
    let profile = kaisa_sys::AppContainerProfile::from_sid_string(recorded.trim())?;
    let expected = kaisa_sys::AppContainerProfile::derive_identity(&format!("kaisa.{attempt}"))?;
    let sid = profile.sid_string()?;
    require_attempt_sid(&sid, &expected.sid_string()?)?;
    Ok(Some(sid))
}

/// What `kaisa mcp --attempt` does: relay to the attempt's broker when one is listening outside
/// the sandbox, otherwise serve the attempt's tools in this process. Windows exec still requires its container.
///
/// # Errors
///
/// Returns the I/O error from relaying or serving.
pub fn bridge(attempt_directory: &Path, output: &mut dyn Write) -> io::Result<()> {
    bridge_from(attempt_directory, io::stdin(), output)
}

/// [`bridge`] with its input given.
fn bridge_from(
    attempt_directory: &Path,
    input: impl io::Read + Send + 'static,
    output: &mut dyn Write,
) -> io::Result<()> {
    if attempt_directory.join(crate::BROKER_ENDPOINT_FILE).exists() {
        return crate::relay(attempt_directory, input, output);
    }
    serve_attempt(attempt_directory, io::BufReader::new(input), output)
}

/// The MCP server holding the attempt's tools and its broker.
pub(crate) fn attempt_server(
    config: &BridgeConfig,
    #[cfg(windows)] container_sid: Option<&str>,
) -> io::Result<McpServer<FileBroker>> {
    let mut broker: FileBroker = FileBroker::new(&config.worktree, config.write_set.clone())?;
    if tools_for(config.role)
        .iter()
        .any(|tool| tool.name == "exec")
    {
        #[cfg(not(windows))]
        {
            broker = broker.with_exec(config.exec_sandbox()?);
        }
        #[cfg(windows)]
        {
            if let Err(error) = config.exec_writable() {
                return Ok(McpServer::new(
                    tools_for(config.role),
                    broker.with_exec(ExecSandbox::Unavailable(error.to_string())),
                ));
            }
            let profile = container_sid
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "the attempt has no AppContainer SID",
                    )
                })
                .and_then(|sid| kaisa_sys::AppContainerProfile::from_sid_string(sid.trim()));
            let sandbox = match profile {
                Ok(profile) => match kaisa_sys::CapabilitySid::derive("kaisaAgentPrograms") {
                    Ok(programs) => ExecSandbox::AppContainer(
                        profile.with_capability(std::sync::Arc::new(programs)),
                    ),
                    Err(error) => ExecSandbox::Unavailable(format!(
                        "exec requires the attempt's program capability: {error}"
                    )),
                },
                Err(error) => ExecSandbox::Unavailable(format!(
                    "exec requires the attempt's AppContainer: {error}"
                )),
            };
            broker = broker.with_exec(sandbox);
        }
    }
    Ok(McpServer::new(tools_for(config.role), broker))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kaisa_domain::{RepoPath, Role, WriteSet};
    use serde_json::json;

    #[cfg(unix)]
    use super::bridge_from;
    use super::{BridgeConfig, serve_attempt};

    /// An attempt whose write grants can be exercised without starting an agent.
    fn an_exec_attempt(name: &str) -> BridgeConfig {
        let worktree =
            std::env::temp_dir().join(format!("kaisa-exec-grants-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&worktree);
        std::fs::create_dir_all(worktree.join("src")).expect("worktree");
        std::fs::write(worktree.join("src/code.rs"), "source").expect("source file");
        BridgeConfig {
            worktree,
            write_set: WriteSet::new(vec![RepoPath::try_new("src/code.rs").expect("source path")]),
            role: Role::Developer,
            scratch: None,
            broker_port: None,
        }
    }

    #[test]
    fn a_source_directory_keeps_its_exec_write_grant() {
        let mut config = an_exec_attempt("directory");
        config.write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("source directory")]);
        let outcome = config.exec_writable();
        let _ = std::fs::remove_dir_all(&config.worktree);
        assert!(
            outcome.is_ok(),
            "ordinary subdirectory grants must preserve build and test execution: {outcome:?}"
        );
    }

    #[test]
    fn existing_git_control_files_refuse_exec_even_when_owned() {
        let mut config = an_exec_attempt("controls");
        for control in [".git", ".gitattributes", ".gitmodules"] {
            std::fs::write(config.worktree.join(control), "control").expect("control file");
            config.write_set =
                WriteSet::new(vec![RepoPath::try_new(control).expect("control path")]);
            assert!(
                config.exec_writable().is_err(),
                "exec must never grant {control}"
            );
        }
        let _ = std::fs::remove_dir_all(&config.worktree);
    }

    #[cfg(unix)]
    #[test]
    fn an_alias_to_git_controls_refuses_the_exec_grant() {
        let mut config = an_exec_attempt("alias");
        std::fs::write(config.worktree.join(".git"), "control").expect("control file");
        std::os::unix::fs::symlink(".git", config.worktree.join("alias")).expect("alias");
        config.write_set = WriteSet::new(vec![RepoPath::try_new("alias").expect("alias path")]);
        let outcome = config.exec_writable();
        let _ = std::fs::remove_dir_all(&config.worktree);
        assert!(
            outcome.is_err(),
            "canonical targets must also exclude Git controls: {outcome:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_hardlinked_source_file_refuses_the_exec_grant() {
        let config = an_exec_attempt("hardlink");
        std::fs::hard_link(
            config.worktree.join("src/code.rs"),
            config.worktree.join("other.rs"),
        )
        .expect("hardlink");
        let outcome = config.exec_writable();
        let _ = std::fs::remove_dir_all(&config.worktree);
        assert!(
            outcome.is_err(),
            "exec must not modify an unowned hardlink through a file grant: {outcome:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_alias_outside_the_worktree_refuses_the_exec_grant() {
        let mut config = an_exec_attempt("outside");
        let outside = config.worktree.with_extension("outside.rs");
        std::fs::write(&outside, "outside").expect("outside file");
        std::os::unix::fs::symlink(&outside, config.worktree.join("alias")).expect("alias");
        config.write_set = WriteSet::new(vec![RepoPath::try_new("alias").expect("alias path")]);
        let outcome = config.exec_writable();
        let _ = std::fs::remove_dir_all(&config.worktree);
        let _ = std::fs::remove_file(outside);
        assert!(
            outcome.is_err(),
            "a canonical target outside the worktree is not an exec write grant: {outcome:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_root_alias_grant_is_carved_by_seatbelt_or_refused_by_additive_backends() {
        let mut config = an_exec_attempt("root-alias");
        std::os::unix::fs::symlink(".", config.worktree.join("root-alias")).expect("root alias");
        config.write_set =
            WriteSet::new(vec![RepoPath::try_new("root-alias").expect("alias path")]);
        let outcome = config.exec_writable();
        let additive = config.exec_writable_with_root_deny(false);
        let _ = std::fs::remove_dir_all(&config.worktree);
        assert!(
            additive.is_err(),
            "additive backends must refuse a root grant: {additive:?}"
        );
        assert_eq!(
            outcome.is_ok(),
            cfg!(target_os = "macos"),
            "only Seatbelt can subtract root control paths from a root grant: {outcome:?}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_source_directory_can_run_exec_and_write_an_owned_file() {
        use crate::{FileBroker, ToolHandler, ToolOutcome};
        let mut config = an_exec_attempt("exec-directory");
        config.write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("source directory")]);
        let mut broker = FileBroker::new(&config.worktree, config.write_set.clone())
            .expect("broker")
            .with_exec(config.exec_sandbox().expect("sandbox"));
        let outcome = broker.call(
            "exec",
            &json!({"argv": ["/bin/sh", "-c", "printf changed > src/new.rs"]}),
        );
        let contents = std::fs::read_to_string(config.worktree.join("src/new.rs"));
        let _ = std::fs::remove_dir_all(&config.worktree);
        assert!(
            matches!(outcome, ToolOutcome::Done(_)),
            "directory write sets must keep exec available: {outcome:?}"
        );
        assert_eq!(
            contents.expect("source created"),
            "changed",
            "directory grants allow normal new files"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_root_write_grant_cannot_change_git_controls_under_seatbelt() {
        use crate::{FileBroker, ToolHandler};
        let mut config = an_exec_attempt("exec-root[case]\"quote\\slash");
        std::os::unix::fs::symlink(".", config.worktree.join("root-alias")).expect("root alias");
        config.write_set =
            WriteSet::new(vec![RepoPath::try_new("root-alias").expect("alias path")]);
        let mut broker = FileBroker::new(&config.worktree, config.write_set.clone())
            .expect("broker")
            .with_exec(config.exec_sandbox().expect("sandbox"));
        let normal = broker.call(
            "exec",
            &json!({"argv": ["/bin/sh", "-c", "printf changed > src/new.rs"]}),
        );
        assert!(
            matches!(normal, crate::ToolOutcome::Done(_))
                && config.worktree.join("src/new.rs").exists(),
            "root grants must still execute and allow ordinary source writes: {normal:?}"
        );
        for control in [".git", ".gitattributes", ".gitmodules"] {
            let command = format!("printf changed > {control}");
            let _ = broker.call("exec", &json!({"argv": ["/bin/sh", "-c", command]}));
            assert!(
                !config.worktree.join(control).exists(),
                "root grant cannot create missing Git control {control}"
            );
        }
        for controls in [
            vec![".git", ".gitattributes", ".gitmodules"],
            vec![".git/config", ".gitattributes", ".gitmodules"],
        ] {
            let _ = std::fs::remove_file(config.worktree.join(".git"));
            if controls.contains(&".git/config") {
                std::fs::create_dir_all(config.worktree.join(".git")).expect("Git directory");
            }
            for control in &controls {
                std::fs::write(config.worktree.join(control), "original").expect("Git control");
            }
            let mut broker = FileBroker::new(&config.worktree, config.write_set.clone())
                .expect("broker")
                .with_exec(config.exec_sandbox().expect("sandbox"));
            for control in &controls {
                let command = format!("printf changed > {}", control.to_ascii_uppercase());
                let _ = broker.call("exec", &json!({"argv": ["/bin/sh", "-c", command]}));
                let alternate = config.worktree.join(control.to_ascii_uppercase());
                if alternate.exists() {
                    assert_eq!(
                        std::fs::read_to_string(&alternate).expect("case variant"),
                        "original",
                        "case variant cannot create or replace Git controls"
                    );
                }
                assert_eq!(
                    std::fs::read_to_string(config.worktree.join(control))
                        .expect("control survives"),
                    "original",
                    "Seatbelt deny overrides the root grant for {control}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&config.worktree);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_safe_source_file_can_be_changed_by_the_os_confined_exec() {
        use crate::{FileBroker, ToolHandler, ToolOutcome};
        let config = an_exec_attempt("exec-source");
        let mut broker = FileBroker::new(&config.worktree, config.write_set.clone())
            .expect("broker")
            .with_exec(config.exec_sandbox().expect("sandbox"));
        let outcome = broker.call(
            "exec",
            &json!({"argv": ["/bin/sh", "-c", "printf changed > src/code.rs"]}),
        );
        let contents =
            std::fs::read_to_string(config.worktree.join("src/code.rs")).expect("source survives");
        let _ = std::fs::remove_dir_all(&config.worktree);
        assert!(
            matches!(outcome, ToolOutcome::Done(_)),
            "safe exec still starts: {outcome:?}"
        );
        assert_eq!(
            contents, "changed",
            "the OS grant admits changes to the owned regular file"
        );
    }

    #[test]
    fn an_existing_source_file_keeps_its_exec_write_grant() {
        let config = an_exec_attempt("source");
        let expected = config
            .worktree
            .join("src/code.rs")
            .canonicalize()
            .expect("source exists");
        let outcome = config.exec_writable().expect("safe source grant");
        let _ = std::fs::remove_dir_all(&config.worktree);
        assert_eq!(
            outcome,
            vec![expected],
            "safe regular source files remain writable"
        );
    }

    #[test]
    fn scratch_that_contains_the_worktree_refuses_exec() {
        let mut config = an_exec_attempt("scratch");
        config.scratch = Some(config.worktree.clone());
        let outcome = config.exec_writable();
        let _ = std::fs::remove_dir_all(&config.worktree);
        assert!(
            outcome.is_err(),
            "scratch cannot reintroduce a root directory grant: {outcome:?}"
        );
    }

    /// Different valid seven-RID package identities have the same syntax.
    fn different_attempt_sids() -> (&'static str, &'static str) {
        ("S-1-15-2-1-2-3-4-5-6-7", "S-1-15-2-7-6-5-4-3-2-1")
    }

    #[test]
    fn another_attempts_valid_sid_is_refused_and_the_recorded_identity_is_accepted() {
        let (recorded, other) = different_attempt_sids();
        assert!(
            super::require_attempt_sid(recorded, other).is_err(),
            "a valid SID from a different attempt is not authority"
        );
        assert!(
            super::require_attempt_sid(recorded, recorded).is_ok(),
            "the matching attempt identity is accepted"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_recorded_sid_from_another_app_is_rejected_before_serving() {
        let config = an_exec_attempt("sid");
        let directory = config.worktree.join("att_identity");
        std::fs::create_dir_all(&directory).expect("attempt directory");
        let expected = kaisa_sys::AppContainerProfile::derive_identity("kaisa.att_identity")
            .expect("attempt identity");
        let other = kaisa_sys::AppContainerProfile::derive_identity("kaisa.att_other")
            .expect("other identity");
        std::fs::write(
            directory.join(crate::CONTAINER_SID_FILE),
            other.sid_string().expect("other SID"),
        )
        .expect("recorded");
        assert!(
            super::verified_container_sid(&directory).is_err(),
            "a different valid AppContainer identity must be refused"
        );
        let expected = expected.sid_string().expect("attempt SID");
        std::fs::write(directory.join(crate::CONTAINER_SID_FILE), &expected).expect("recorded");
        assert_eq!(
            super::verified_container_sid(&directory).expect("matching identity"),
            Some(expected),
            "the matching recorded identity is retained"
        );
        let _ = std::fs::remove_dir_all(&config.worktree);
    }

    #[test]
    fn the_bridge_command_serves_the_attempts_tools() {
        let base: PathBuf =
            std::env::temp_dir().join(format!("kaisa-bridge-{}", std::process::id()));
        let worktree: PathBuf = base.join("worktree");
        let attempt: PathBuf = base.join("attempt");
        for directory in [worktree.join("src/auth"), attempt.clone()] {
            std::fs::create_dir_all(directory).expect("writable temp dir");
        }
        BridgeConfig {
            worktree: worktree.clone(),
            write_set: WriteSet::new(vec![RepoPath::try_new("src/auth").expect("valid")]),
            role: Role::Developer,
            scratch: None,
            broker_port: None,
        }
        .write_to(&attempt)
        .expect("the config is written");
        let requests: String = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "fs_write", "arguments": {"path": "src/auth/session.rs", "content": "pub fn refresh() {}\n"}}}),
        ]
        .iter()
        .map(|request| format!("{request}\n"))
        .collect();
        let mut responses: Vec<u8> = Vec::new();

        serve_attempt(&attempt, requests.as_bytes(), &mut responses).expect("the bridge serves");
        let written: bool = worktree.join("src/auth/session.rs").exists();
        let _ = std::fs::remove_dir_all(&base);

        assert!(
            written,
            "the agent's MCP config starts this bridge, and its tools act on the attempt's worktree"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_bridge_whose_broker_is_gone_fails_instead_of_acting_itself() {
        let base: PathBuf =
            std::env::temp_dir().join(format!("kaisa-bridge-gone-{}", std::process::id()));
        let worktree: PathBuf = base.join("worktree");
        let attempt: PathBuf = base.join("attempt");
        for directory in [worktree.join("src/auth"), attempt.clone()] {
            std::fs::create_dir_all(directory).expect("writable temp dir");
        }
        BridgeConfig {
            worktree: worktree.clone(),
            write_set: WriteSet::new(vec![RepoPath::try_new("src/auth").expect("valid")]),
            role: Role::Developer,
            scratch: None,
            broker_port: None,
        }
        .write_to(&attempt)
        .expect("the config is written");
        crate::token::AttemptToken::issue(&attempt).expect("issued");
        std::fs::write(
            attempt.join(crate::endpoint::BROKER_ENDPOINT_FILE),
            base.join("no-broker-here.sock")
                .as_os_str()
                .as_encoded_bytes(),
        )
        .expect("recorded");
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "fs_write", "arguments": {"path": "src/auth/notes.md", "content": "x"}}});

        let outcome = bridge_from(
            &attempt,
            std::io::Cursor::new(format!("{request}\n")),
            &mut Vec::new(),
        );
        let written: bool = worktree.join("src/auth/notes.md").exists();
        let _ = std::fs::remove_dir_all(&base);

        assert!(
            outcome.is_err() && !written,
            "an attempt with a broker never falls back to serving its tools in the agent's process"
        );
    }
}
