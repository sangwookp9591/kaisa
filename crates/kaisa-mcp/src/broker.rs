//! The execution broker's file tools (architecture §5.6 "Broker tool contracts", §5.2
//! write-set ownership).
//!
//! Every path is repository-relative and checked twice: as text (no absolute paths, no `..`)
//! and on disk (after resolving symbolic links it must still be inside the worktree). Writes
//! must also be inside the attempt's write set; a refused write comes back to the agent as a
//! question for its coordinator, not as a silent failure.

use std::path::{Path, PathBuf};

use kaisa_domain::{RepoPath, WriteSet};
use serde_json::Value;

use crate::server::{ToolHandler, ToolOutcome};

/// The largest file `fs_read` returns, in bytes.
const READ_LIMIT_BYTES: u64 = 1024 * 1024;

/// The file tools of one attempt.
#[derive(Debug)]
pub struct FileBroker {
    /// The worktree, canonicalised.
    root: PathBuf,
    write_set: WriteSet,
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
        })
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
}
