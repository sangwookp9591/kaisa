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
    /// The attempt's AppContainer (Windows).
    #[cfg(windows)]
    AppContainer(kaisa_sys::AppContainerProfile),
    /// Isolation could not be applied; exec must fail closed.
    Unavailable(String),
    /// No OS sandbox; broker exec always refuses this configuration.
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
            env: Vec::new(),
        };
        let confinement: Confinement<'_> = match sandbox {
            #[cfg(target_os = "macos")]
            ExecSandbox::Seatbelt(policy) => Confinement::Seatbelt(policy),
            #[cfg(target_os = "linux")]
            ExecSandbox::Landlock(ruleset) => Confinement::Landlock(ruleset),
            #[cfg(windows)]
            ExecSandbox::AppContainer(profile) => Confinement::AppContainer(profile),
            ExecSandbox::Unavailable(reason) => return ToolOutcome::Refused(reason.clone()),
            #[cfg(windows)]
            ExecSandbox::Unconfined => {
                return ToolOutcome::Refused("exec requires the attempt's AppContainer".to_owned());
            }
            #[cfg(not(windows))]
            ExecSandbox::Unconfined => {
                return ToolOutcome::Refused("exec requires OS sandbox isolation".to_owned());
            }
        };
        match run_captured(&spec, &confinement, &[], EXEC_DEADLINE, EXEC_OUTPUT_CAP) {
            Ok(run) => ToolOutcome::Done(describe(&run)),
            Err(error) => ToolOutcome::Refused(format!("{program} could not be run: {error}")),
        }
    }

    /// Resolves each existing component, including directory aliases. Missing suffixes are
    /// appended to the canonical ancestor; dangling links are refused rather than created.
    pub(crate) fn resolve(&self, relative: &RepoPath) -> Result<PathBuf, String> {
        let mut resolved = self.root.clone();
        for component in Path::new(relative.as_str()).components() {
            resolved.push(component);
            match std::fs::symlink_metadata(&resolved) {
                Ok(_) => {
                    resolved = resolved.canonicalize().map_err(|error| {
                        format!("{relative} cannot be resolved through a symbolic link: {error}")
                    })?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("{relative} cannot be resolved: {error}")),
            }
            if !resolved.starts_with(&self.root) {
                return Err(format!(
                    "{relative} leads outside the repository through a symbolic link"
                ));
            }
        }
        Ok(resolved)
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
        let Some(content) = arguments.get("content").and_then(Value::as_str) else {
            return ToolOutcome::Refused("fs_write needs a string `content`".to_owned());
        };
        let file: PathBuf = match self.resolve(&path) {
            Ok(file) => file,
            Err(reason) => return ToolOutcome::Refused(reason),
        };
        let actual = match file.strip_prefix(&self.root).ok().and_then(|relative| {
            relative
                .to_str()
                .and_then(|text| RepoPath::try_new(text).ok())
        }) {
            Some(actual) => actual,
            None => return ToolOutcome::Refused(format!("{path} is not a repository path")),
        };
        if protected_git_path(&path) || protected_git_path(&actual) {
            return ToolOutcome::Refused(format!("{path} is a protected Git control path"));
        }
        if !self.write_set.permits(&actual) {
            return ToolOutcome::Refused(format!(
                "{path} is outside your write set. If the change really needs it, ask your \
                 coordinator to widen the write set; do not work around this."
            ));
        }
        if let Some(parent) = file.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            return ToolOutcome::Refused(format!("{path} cannot be written: {error}"));
        }
        // Recheck after creating parents, immediately before opening the actual target.
        // This reduces the race window; directory-handle-relative I/O would be needed to
        // eliminate concurrent ancestor replacement entirely.
        if self.resolve(&path).as_ref() != Ok(&file) {
            return ToolOutcome::Refused(format!("{path} changed while it was being resolved"));
        }
        if let Ok(metadata) = std::fs::symlink_metadata(&file)
            && metadata.file_type().is_symlink()
        {
            return ToolOutcome::Refused(format!("{path} became a symbolic link"));
        }
        // The link-count check also races with concurrent replacement/link creation.
        // Removing this TOCTOU requires handle-relative I/O and an atomic replacement policy.
        match existing_link_count(&file) {
            Ok(Some(links)) if links > 1 => {
                return ToolOutcome::Refused(format!("{path} is a hardlinked file"));
            }
            Err(error) => {
                return ToolOutcome::Refused(format!("{path} cannot be checked: {error}"));
            }
            _ => {}
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

/// Deny at every depth, including nested repositories and case-insensitive filesystems.
fn protected_git_path(path: &RepoPath) -> bool {
    path.as_str().split('/').any(|component| {
        [".git", ".gitattributes", ".gitmodules"]
            .iter()
            .any(|name| component.eq_ignore_ascii_case(name))
    })
}

pub(crate) fn existing_link_count(path: &Path) -> std::io::Result<Option<u64>> {
    #[cfg(any(unix, windows))]
    {
        match kaisa_sys::hard_link_count(path) {
            Ok(links) => Ok(Some(links)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "cannot inspect hardlinks",
        ))
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

    fn a_broker_owning_paths(worktree: &ScratchWorktree, paths: &[&str]) -> FileBroker {
        FileBroker::new(
            &worktree.root,
            WriteSet::new(
                paths
                    .iter()
                    .map(|path| RepoPath::try_new(path).expect("valid owned path"))
                    .collect(),
            ),
        )
        .expect("the worktree resolves")
    }

    #[test]
    fn git_control_paths_are_refused_even_when_explicitly_owned() {
        let worktree = ScratchWorktree::new("git-controls");
        let controls = [
            ".git",
            ".git/config",
            ".gitattributes",
            ".gitmodules",
            "src/auth/.git/hooks/pre-commit",
            "src/auth/.gitattributes",
            "src/auth/.gitmodules",
            ".GiT/config",
        ];
        let mut broker = a_broker_owning_paths(&worktree, &controls);
        for path in controls {
            let outcome = broker.call("fs_write", &json!({"path": path, "content": "changed"}));
            assert!(
                matches!(&outcome, ToolOutcome::Refused(reason) if reason.contains("protected Git")),
                "Git controls stay protected regardless of write ownership: {path}: {outcome:?}"
            );
            assert!(
                !worktree.root.join(path).exists(),
                "refusal must not create {path}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_owned_alias_to_git_controls_is_refused() {
        let worktree = ScratchWorktree::new("git-alias");
        std::fs::create_dir(worktree.root.join(".git")).expect("Git directory");
        auth_alias(&worktree, "../../.git");
        let mut broker = a_broker_owning_paths(&worktree, &["src/auth", ".git"]);
        let outcome = write_through_auth_alias(&mut broker, "/config");
        assert!(
            matches!(&outcome, ToolOutcome::Refused(reason) if reason.contains("protected Git")),
            "canonical Git targets stay protected through aliases: {outcome:?}"
        );
        assert!(
            !worktree.root.join(".git/config").exists(),
            "Git config must stay absent"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn an_owned_hardlink_to_an_unowned_file_refuses_the_write() {
        let worktree = ScratchWorktree::new("hardlink");
        let original = worktree.root.join("src/billing/invoice.rs");
        let alias = worktree.root.join("src/auth/invoice.rs");
        let original_content = "billing data";
        std::fs::write(&original, original_content).expect("billing file");
        std::fs::hard_link(&original, &alias).expect("a hardlink on the same filesystem");
        let mut broker = a_broker_owning_auth(&worktree);
        let outcome = broker.call(
            "fs_write",
            &json!({"path": "src/auth/invoice.rs", "content": "changed"}),
        );
        assert!(
            matches!(&outcome, ToolOutcome::Refused(reason) if reason.contains("hardlinked")),
            "owned hardlinks must not truncate an unowned inode: {outcome:?}"
        );
        for file in [original, alias] {
            assert_eq!(
                std::fs::read_to_string(file).expect("unchanged file"),
                original_content,
                "both names of the inode must retain their contents"
            );
        }
    }

    #[test]
    fn unavailable_isolation_refuses_exec_with_its_reason() {
        let worktree = ScratchWorktree::new("exec-unavailable");
        let reason = "test kernel has no isolation";
        let mut broker =
            a_broker_owning_auth(&worktree).with_exec(crate::attempt::unavailable_exec(reason));
        let outcome = broker.call("exec", &json!({"argv": ["missing-command"]}));
        assert!(
            matches!(&outcome, ToolOutcome::Refused(message) if message.contains(reason)),
            "sandbox failure must return its reason before spawning: {outcome:?}"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn an_unconfined_exec_is_refused_before_spawning() {
        let worktree = ScratchWorktree::new("exec-no-isolation");
        let mut broker = a_broker_owning_auth(&worktree).with_exec(super::ExecSandbox::Unconfined);
        let outcome = broker.call(
            "exec",
            &json!({"argv": ["/bin/sh", "-c", "echo unconfined"]}),
        );
        assert!(
            matches!(&outcome, ToolOutcome::Refused(reason) if reason.contains("isolation")),
            "exec must never fall back to an unconfined process: {outcome:?}"
        );
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

    #[cfg(unix)]
    fn auth_alias(worktree: &ScratchWorktree, target: &str) {
        std::os::unix::fs::symlink(target, worktree.root.join("src/auth/link"))
            .expect("the attempt can plant an alias");
    }

    #[cfg(unix)]
    fn write_through_auth_alias(broker: &mut FileBroker, suffix: &str) -> ToolOutcome {
        broker.call(
            "fs_write",
            &json!({
                "path": format!("src/auth/link{suffix}"), "content": "changed"
            }),
        )
    }

    #[cfg(unix)]
    #[test]
    fn a_link_inside_the_write_set_to_an_unowned_directory_refuses_the_write() {
        let worktree = ScratchWorktree::new("alias-unowned");
        auth_alias(&worktree, "../billing");
        let mut broker = a_broker_owning_auth(&worktree);

        let outcome = write_through_auth_alias(&mut broker, "/new/invoice.rs");

        assert!(
            matches!(&outcome, ToolOutcome::Refused(reason) if reason.contains("outside your write set")),
            "the canonical billing target is unowned even though the alias is owned: {outcome:?}"
        );
        assert!(
            !worktree.root.join("src/billing/new").exists(),
            "refusing an alias must happen before creating unowned parent directories"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_link_inside_the_write_set_refuses_the_write() {
        let worktree = ScratchWorktree::new("alias-dangling");
        auth_alias(&worktree, "missing.rs");
        let mut broker = a_broker_owning_auth(&worktree);

        let outcome = write_through_auth_alias(&mut broker, "");

        assert!(
            matches!(&outcome, ToolOutcome::Refused(reason) if reason.contains("symbolic link")),
            "a dangling link must be refused even when its missing target would be owned: {outcome:?}"
        );
        assert!(
            !worktree.root.join("src/auth/missing.rs").exists(),
            "a dangling link must never create its missing target"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_an_owned_directory_writes_and_reads_the_actual_target() {
        let worktree = ScratchWorktree::new("alias-owned");
        std::fs::create_dir(worktree.root.join("src/auth/owned")).expect("owned directory");
        auth_alias(&worktree, "owned");
        let mut broker = a_broker_owning_auth(&worktree);

        let written = write_through_auth_alias(&mut broker, "/new/session.rs");
        let read = broker.call("fs_read", &json!({"path": "src/auth/link/new/session.rs"}));

        assert!(
            matches!(written, ToolOutcome::Done(_)),
            "owned aliases remain writable: {written:?}"
        );
        assert!(
            matches!(&read, ToolOutcome::Done(content) if content == "changed"),
            "reads follow the same canonical target: {read:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_exec_runs_inside_the_sandbox() {
        let worktree = ScratchWorktree::new("exec");
        let profile = kaisa_sys::AppContainerProfile::ensure(&format!(
            "kaisa.broker.exec.{}",
            std::process::id()
        ))
        .expect("an attempt profile can be created");
        profile
            .grant(&worktree.root, kaisa_sys::ContainerAccess::ReadExecute)
            .expect("worktree is readable");
        let identity =
            kaisa_sys::AppContainerProfile::from_sid_string(&profile.sid_string().expect("SID"))
                .expect("broker opens the attempt identity");
        let mut broker =
            a_broker_owning_auth(&worktree).with_exec(super::ExecSandbox::AppContainer(identity));
        let command = json!({"argv": [r"C:\Windows\System32\cmd.exe", "/d", "/c",
            format!("echo broker-contained & echo changed > \"{}\"", worktree.root.join("src/billing/invoice.rs").display())]});

        let outcome = broker.call("exec", &command);
        let _ = profile.revoke(&worktree.root);
        let _ = profile.delete();

        assert!(
            matches!(&outcome, ToolOutcome::Done(output) if output.contains("broker-contained")),
            "the contained command must actually run: {outcome:?}"
        );
        assert!(
            !worktree.root.join("src/billing/invoice.rs").exists(),
            "the broker command cannot write outside the attempt's grants"
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_unconfined_windows_exec_is_refused() {
        let worktree = ScratchWorktree::new("exec-unconfined");
        let mut broker = a_broker_owning_auth(&worktree).with_exec(super::ExecSandbox::Unconfined);
        let outcome = broker.call(
            "exec",
            &json!({"argv": ["cmd.exe", "/c", "echo unconfined"]}),
        );
        assert!(
            matches!(&outcome, ToolOutcome::Refused(reason) if reason.contains("AppContainer")),
            "Windows exec must fail closed without isolation: {outcome:?}"
        );
    }

    /// An `exec` that tries to write into `src/billing`, which the attempt does not own.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
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
