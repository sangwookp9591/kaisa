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

    /// The exec sandbox this attempt gets on this OS.
    fn exec_sandbox(&self) -> io::Result<ExecSandbox> {
        let worktree: PathBuf = self.worktree.canonicalize()?;
        let mut writable: Vec<PathBuf> = sandbox_profile(self.role, &self.write_set)
            .writable
            .iter()
            .map(|path| worktree.join(path.as_str()))
            .collect();
        writable.extend(self.scratch.iter().cloned());
        #[cfg(target_os = "macos")]
        {
            Ok(ExecSandbox::Seatbelt(kaisa_sys::SeatbeltPolicy {
                writable,
                broker_port: self.broker_port,
                ..kaisa_sys::SeatbeltPolicy::default()
            }))
        }
        #[cfg(target_os = "linux")]
        {
            Ok(
                match kaisa_sys::write_ruleset(&writable, self.broker_port)? {
                    Some(ruleset) => ExecSandbox::Landlock(ruleset),
                    None => ExecSandbox::Unconfined,
                },
            )
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = writable;
            Ok(ExecSandbox::Unconfined)
        }
    }
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
    attempt_server(&BridgeConfig::load(attempt_directory)?)?
        .serve(input, output)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}")))
}

/// What `kaisa mcp --attempt` does: relay to the attempt's broker when one is listening outside
/// the sandbox, otherwise serve the attempt's tools in this process (Windows until T15.5s).
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
    #[cfg(unix)]
    if attempt_directory
        .join(crate::endpoint::BROKER_ENDPOINT_FILE)
        .exists()
    {
        return crate::endpoint::relay(attempt_directory, input, output);
    }
    serve_attempt(attempt_directory, io::BufReader::new(input), output)
}

/// The MCP server holding the attempt's tools and its broker.
pub(crate) fn attempt_server(config: &BridgeConfig) -> io::Result<McpServer<FileBroker>> {
    let mut broker: FileBroker = FileBroker::new(&config.worktree, config.write_set.clone())?;
    if tools_for(config.role)
        .iter()
        .any(|tool| tool.name == "exec")
    {
        broker = broker.with_exec(config.exec_sandbox()?);
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
