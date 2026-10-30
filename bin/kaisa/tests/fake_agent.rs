//! A scripted agent acting through the real `kaisa mcp --attempt` bridge, as separate processes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use kaisa_domain::{RepoPath, Role, WriteSet};
use kaisa_mcp::BridgeConfig;
use serde_json::json;

/// The context pack the agent receives as its first message.
const PACK_TEXT: &str = "## Task\nMake the login test wait for the token refresh.\n";

/// The fake agent is built next to `kaisa` by `cargo test --workspace`.
fn the_fake_agent() -> PathBuf {
    let kaisa: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"));
    let name: String = format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX);
    let agent: PathBuf = kaisa.with_file_name(name);
    assert!(
        agent.exists(),
        "run the workspace tests (`cargo test --workspace`) so the fake agent is built: {}",
        agent.display()
    );
    agent
}

/// An attempt that owns `src/auth`, with its bridge config and MCP config written.
struct Attempt {
    base: PathBuf,
}

impl Attempt {
    fn new(name: &str) -> Self {
        let base: PathBuf =
            std::env::temp_dir().join(format!("kaisa-e2e-{name}-{}", std::process::id()));
        for directory in ["worktree/src/auth", "worktree/src/billing", "attempt"] {
            std::fs::create_dir_all(base.join(directory)).expect("writable temp dir");
        }
        BridgeConfig {
            worktree: base.join("worktree"),
            write_set: WriteSet::new(vec![RepoPath::try_new("src/auth").expect("valid")]),
            role: Role::Developer,
            scratch: None,
            broker_port: None,
        }
        .write_to(&base.join("attempt"))
        .expect("the bridge config is written");
        let mcp = json!({"mcpServers": {"kaisa": {
            "command": env!("CARGO_BIN_EXE_kaisa"),
            "args": ["mcp", "--attempt", base.join("attempt").to_string_lossy()],
        }}});
        std::fs::write(base.join("attempt/mcp.json"), mcp.to_string()).expect("written");
        Self { base }
    }

    fn file(&self, relative: &str) -> PathBuf {
        self.base.join("worktree").join(relative)
    }

    fn mcp_config(&self) -> PathBuf {
        self.base.join("attempt/mcp.json")
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// Runs the fake agent asking it to write `target`; true if it exited successfully.
fn the_agent_writes(attempt: &Attempt, target: &str) -> bool {
    let mut agent = Command::new(the_fake_agent())
        .args([
            "--mcp-config",
            &attempt.mcp_config().to_string_lossy(),
            "--fake-write",
            target,
        ])
        .stdin(Stdio::piped())
        .spawn()
        .expect("the fake agent starts");
    let first_message = json!({"type": "user", "message": {"role": "user", "content": PACK_TEXT}});
    writeln!(agent.stdin.take().expect("piped"), "{first_message}").expect("sent");
    agent.wait().expect("the agent exits").success()
}

fn contents(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn the_fake_agent_changes_a_file_only_through_the_bridge() {
    let attempt = Attempt::new("bridge");

    let wrote_inside: bool = the_agent_writes(&attempt, "src/auth/notes.md");
    let wrote_outside: bool = the_agent_writes(&attempt, "src/billing/notes.md");

    assert!(
        wrote_inside && contents(&attempt.file("src/auth/notes.md")).starts_with(PACK_TEXT),
        "the agent received its pack and changed its own file through the bridge"
    );
    assert!(
        !wrote_outside && !attempt.file("src/billing/notes.md").exists(),
        "the bridge refused the out-of-set write and the agent learned it failed"
    );
}
