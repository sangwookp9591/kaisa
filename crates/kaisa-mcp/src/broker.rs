//! The execution broker's file tools (architecture §5.6 "Broker tool contracts", §5.2
//! write-set ownership).
//!
//! Every path is repository-relative and checked twice: as text (no absolute paths, no `..`)
//! and on disk (after resolving symbolic links it must still be inside the worktree). Writes
//! must also be inside the attempt's write set; a refused write comes back to the agent as a
//! question for its coordinator, not as a silent failure.

use std::path::{Path, PathBuf};
use std::time::Duration;

use kaisa_domain::{RepoPath, WriteSet};
use kaisa_sys::{CapturedRun, Confinement, ProcessSpec, run_captured};
use serde_json::Value;

use crate::server::{ToolHandler, ToolOutcome};

/// The largest file `fs_read` returns, in bytes.
const READ_LIMIT_BYTES: u64 = 1024 * 1024;
/// How long one `exec` may run.
const EXEC_DEADLINE: Duration = Duration::from_secs(10 * 60);
/// How much of an `exec`'s output goes back to the model, in bytes.
const EXEC_OUTPUT_CAP: usize = 64 * 1024;

/// The OS sandbox `exec` runs under.
#[derive(Debug)]
#[non_exhaustive]
pub enum ExecSandbox {
    /// A Seatbelt policy (macOS).
    #[cfg(target_os = "macos")]
    Seatbelt(kaisa_sys::SeatbeltPolicy),
    /// A Landlock ruleset with its UDP filter (Linux).
    #[cfg(target_os = "linux")]
    Landlock(kaisa_sys::LandlockRuleset),
    /// No OS sandbox; the run must be labelled reduced (Windows until T15.5).
    Unconfined,
}

/// The file tools of one attempt.
#[derive(Debug)]
pub struct FileBroker {
    /// The worktree, canonicalised.
    root: PathBuf,
    write_set: WriteSet,
    /// How `exec` is confined; `None` when the role has no `exec`.
    exec: Option<ExecSandbox>,
}

impl FileBroker {
    /// A broker for the worktree at `root` with `write_set`.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the worktree cannot be resolved.
    pub fn new(root: &Path, write_set: WriteSet) -> std::io::Result<Self> {
        Ok(Self {
            root: root.canonicalize()?,
            write_set,
            exec: None,
        })
    }

    /// Enables `exec`, confined by `sandbox`.
    #[must_use]
    pub fn with_exec(mut self, sandbox: ExecSandbox) -> Self {
        self.exec = Some(sandbox);
        self
    }

    fn exec(&self, arguments: &Value) -> ToolOutcome {
        let Some(sandbox) = &self.exec else {
            return ToolOutcome::Refused("exec is not available to this attempt".to_owned());
        };
        let argv: Vec<String> = arguments
            .get("argv")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let Some((program, args)) = argv.split_first() else {
            return ToolOutcome::Refused("exec needs a non-empty `argv`".to_owned());
        };
        let spec = ProcessSpec {
            program: PathBuf::from(program),
            args: args.to_vec(),
            working_directory: self.root.clone(),
        };
        let confinement: Confinement<'_> = match sandbox {
            #[cfg(target_os = "macos")]
            ExecSandbox::Seatbelt(policy) => Confinement::Seatbelt(policy),
            #[cfg(target_os = "linux")]
            ExecSandbox::Landlock(ruleset) => Confinement::Landlock(ruleset),
            ExecSandbox::Unconfined => Confinement::Unconfined,
        };
        match run_captured(&spec, &confinement, EXEC_DEADLINE, EXEC_OUTPUT_CAP) {
            Ok(run) => ToolOutcome::Done(describe(&run)),
            Err(error) => ToolOutcome::Refused(format!("{program} could not be run: {error}")),
        }
    }

    /// The on-disk path for `relative`, refused if it leaves the worktree through a symbolic
    /// link. `existing_ancestor_only` checks the nearest existing ancestor (for new files).
    fn resolve(&self, relative: &RepoPath) -> Result<PathBuf, String> {
        let target: PathBuf = self.root.join(relative.as_str());
        let mut probe: &Path = &target;
        while !probe.exists() {
            probe = probe
                .parent()
                .ok_or_else(|| format!("{relative} has no existing parent"))?;
        }
        let resolved: PathBuf = probe
            .canonicalize()
            .map_err(|error| format!("{relative} cannot be resolved: {error}"))?;
        if resolved.starts_with(&self.root) {
            Ok(target)
        } else {
            Err(format!(
                "{relative} leads outside the repository through a symbolic link"
            ))
        }
    }

    fn read(&self, arguments: &Value) -> ToolOutcome {
        let path = match path_argument(arguments) {
            Ok(path) => path,
            Err(reason) => return ToolOutcome::Refused(reason),
        };
        let file: PathBuf = match self.resolve(&path) {
            Ok(file) => file,
            Err(reason) => return ToolOutcome::Refused(reason),
        };
        match std::fs::metadata(&file) {
            Ok(metadata) if metadata.len() > READ_LIMIT_BYTES => {
                return ToolOutcome::Refused(format!(
                    "{path} is larger than {READ_LIMIT_BYTES} bytes; use search instead"
                ));
            }
            Ok(_) => {}
            Err(error) => return ToolOutcome::Refused(format!("{path} cannot be read: {error}")),
        }
        match std::fs::read_to_string(&file) {
            Ok(content) => ToolOutcome::Done(content),
            Err(error) => ToolOutcome::Refused(format!("{path} cannot be read: {error}")),
        }
    }

    fn write(&self, arguments: &Value) -> ToolOutcome {
        let path = match path_argument(arguments) {
            Ok(path) => path,
            Err(reason) => return ToolOutcome::Refused(reason),
        };
        if !self.write_set.permits(&path) {
            return ToolOutcome::Refused(format!(
                "{path} is outside your write set. If the change really needs it, ask your \
                 coordinator to widen the write set; do not work around this."
            ));
        }
        let Some(content) = arguments.get("content").and_then(Value::as_str) else {
            return ToolOutcome::Refused("fs_write needs a string `content`".to_owned());
        };
        let file: PathBuf = match self.resolve(&path) {
            Ok(file) => file,
            Err(reason) => return ToolOutcome::Refused(reason),
        };
        if let Some(parent) = file.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            return ToolOutcome::Refused(format!("{path} cannot be written: {error}"));
        }
        match std::fs::write(&file, content) {
            Ok(()) => ToolOutcome::Done(format!(
                "wrote {} bytes to {path} (blake3 {})",
                content.len(),
                blake3::hash(content.as_bytes()).to_hex()
            )),
            Err(error) => ToolOutcome::Refused(format!("{path} cannot be written: {error}")),
        }
    }
}

fn describe(run: &CapturedRun) -> String {
    let ending: String = match run.exit {
        Some(kaisa_sys::ProcessExit::Code(code)) => format!("exit code {code}"),
        Some(kaisa_sys::ProcessExit::Terminated) => "terminated".to_owned(),
        None => "ended at its deadline".to_owned(),
    };
    let cut: &str = if run.truncated { " (output cut)" } else { "" };
    format!("{ending}{cut}\n{}", String::from_utf8_lossy(&run.output))
}

fn path_argument(arguments: &Value) -> Result<RepoPath, String> {
    let text: &str = arguments
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "the tool needs a string `path`".to_owned())?;
    RepoPath::try_new(text).map_err(|error| format!("{text:?} is not a repository path: {error}"))
}

impl ToolHandler for FileBroker {
    fn call(&mut self, tool: &str, arguments: &Value) -> ToolOutcome {
        match tool {
            "fs_read" => self.read(arguments),
            "fs_write" => self.write(arguments),
            "exec" => self.exec(arguments),
            other => ToolOutcome::Refused(format!("{other} is not available yet")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kaisa_domain::{RepoPath, WriteSet};
    use serde_json::json;

    use super::FileBroker;
    use crate::server::{ToolHandler, ToolOutcome};

    /// A worktree with two areas, removed when dropped.
    struct ScratchWorktree {
        root: PathBuf,
    }

    impl ScratchWorktree {
        fn new(name: &str) -> Self {
            let root: PathBuf =
                std::env::temp_dir().join(format!("kaisa-broker-{name}-{}", std::process::id()));
            for directory in ["src/auth", "src/billing"] {
                std::fs::create_dir_all(root.join(directory)).expect("writable temp dir");
            }
            Self { root }
        }
    }

    impl Drop for ScratchWorktree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// The attempt owns `src/auth` only.
    fn a_broker_owning_auth(worktree: &ScratchWorktree) -> FileBroker {
        FileBroker::new(
            &worktree.root,
            WriteSet::new(vec![RepoPath::try_new("src/auth").expect("valid")]),
        )
        .expect("the worktree resolves")
    }

    #[test]
    fn a_write_inside_the_write_set_is_applied() {
        let worktree = ScratchWorktree::new("inside");
        let mut broker: FileBroker = a_broker_owning_auth(&worktree);

        let outcome: ToolOutcome = broker.call(
            "fs_write",
            &json!({"path": "src/auth/session.rs", "content": "pub fn refresh() {}\n"}),
        );

        assert!(
            matches!(outcome, ToolOutcome::Done(_))
                && worktree.root.join("src/auth/session.rs").exists(),
            "the agent's own paths are writable, got {outcome:?}"
        );
    }

    #[test]
    fn a_write_outside_the_write_set_is_refused_as_a_question() {
        let worktree = ScratchWorktree::new("outside");
        let mut broker: FileBroker = a_broker_owning_auth(&worktree);

        let outcome: ToolOutcome = broker.call(
            "fs_write",
            &json!({"path": "src/billing/invoice.rs", "content": "changed\n"}),
        );

        assert!(
            matches!(&outcome, ToolOutcome::Refused(reason) if reason.contains("ask your coordinator"))
                && !worktree.root.join("src/billing/invoice.rs").exists(),
            "an out-of-set change becomes a question, never a silent edit, got {outcome:?}"
        );
    }

    #[test]
    fn a_read_outside_the_repository_is_refused() {
        let worktree = ScratchWorktree::new("escape");
        let mut broker: FileBroker = a_broker_owning_auth(&worktree);

        let outcome: ToolOutcome = broker.call("fs_read", &json!({"path": "../../etc/passwd"}));

        assert!(
            matches!(outcome, ToolOutcome::Refused(_)),
            "the broker never reads outside the repository, got {outcome:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_out_of_the_repository_is_not_followed() {
        let worktree = ScratchWorktree::new("symlink");
        std::os::unix::fs::symlink("/etc", worktree.root.join("src/auth/escape"))
            .expect("a symlink can be created");
        let mut broker: FileBroker = a_broker_owning_auth(&worktree);

        let outcome: ToolOutcome =
            broker.call("fs_read", &json!({"path": "src/auth/escape/passwd"}));

        assert!(
            matches!(&outcome, ToolOutcome::Refused(reason) if reason.contains("symbolic link")),
            "a link planted in the worktree must not open the rest of the disk, got {outcome:?}"
        );
    }

    /// An `exec` that tries to write into `src/billing`, which the attempt does not own.
    fn an_exec_writing_billing(worktree: &ScratchWorktree) -> serde_json::Value {
        json!({"argv": ["/bin/sh", "-c", format!("echo changed > '{}'", worktree.root.join("src/billing/invoice.rs").display())]})
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_exec_runs_inside_the_sandbox() {
        let worktree = ScratchWorktree::new("exec");
        let auth: PathBuf = worktree
            .root
            .canonicalize()
            .expect("exists")
            .join("src/auth");
        let mut broker: FileBroker = a_broker_owning_auth(&worktree).with_exec(
            super::ExecSandbox::Seatbelt(kaisa_sys::SeatbeltPolicy {
                writable: vec![auth],
                ..kaisa_sys::SeatbeltPolicy::default()
            }),
        );

        broker.call("exec", &an_exec_writing_billing(&worktree));

        assert!(
            !worktree.root.join("src/billing/invoice.rs").exists(),
            "a command the agent runs is held to the same write set as its file tools"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_exec_runs_inside_the_sandbox() {
        let worktree = ScratchWorktree::new("exec");
        let ruleset = kaisa_sys::write_ruleset(&[worktree.root.join("src/auth")], None)
            .expect("the ruleset can be built")
            .expect("the CI kernel supports Landlock");
        let mut broker: FileBroker =
            a_broker_owning_auth(&worktree).with_exec(super::ExecSandbox::Landlock(ruleset));

        broker.call("exec", &an_exec_writing_billing(&worktree));

        assert!(
            !worktree.root.join("src/billing/invoice.rs").exists(),
            "a command the agent runs is held to the same write set as its file tools"
        );
    }
}
