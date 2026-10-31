//! Trusted checks: the project's own test commands, run by Kaisa on a clean checkout of the
//! frozen candidate, in the OS sandbox, with no agent involved (T21b, ADR-0015). What they
//! report is evidence bound to the candidate's tree.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use kaisa_context::{DetectedCommand, Purpose};
use kaisa_sys::{CapturedRun, Confinement, ProcessExit, ProcessSpec, run_captured};
use kaisa_workspace::{Candidate, CandidateCheckout, changed_check_definitions};

use crate::goal::{GoalError, at};
use crate::ids::fresh_suffix;

/// How long one check may run.
const CHECK_DEADLINE: Duration = Duration::from_secs(30 * 60);
/// How much of a check's output is kept for its digest, in bytes.
const CHECK_OUTPUT_CAP: usize = 16 * 1024 * 1024;
/// Maximum length of a Kaisa-authored check failure reason shown at the gate.
const MAX_CHECK_FAILURE_REASON_CHARS: usize = 160;
/// Fixed text only: never include check output, which may contain credentials.
const CHANGED_CHECK_SOURCES: &str =
    "체크가 후보 소스를 바꿨다. 체크 정의를 확인하라(예: 포매터를 --check 모드로 실행).";

/// What one trusted check showed about one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// The candidate tree the check ran on.
    pub candidate_tree: String,
    /// The command, as run.
    pub check: Vec<String>,
    /// How it ended; `None` if it ran past its deadline.
    pub exit: Option<ProcessExit>,
    /// A bounded, Kaisa-authored reason the check's evidence is invalid, regardless of exit.
    pub failure_reason: Option<String>,
    /// The digest of its output, so the evidence can be matched to a stored transcript.
    pub output_digest: String,
    /// False if the check ran without OS confinement (Windows until checks get a container).
    pub confined: bool,
    /// Check-definition files the candidate changed since its base; when any are named, the
    /// check may be testing rules the agent wrote, so a person must accept the result.
    pub definitions_changed: Vec<String>,
    /// File-specific test weakening signals. Any of them stops delivery for a person.
    pub weakened_tests: Vec<String>,
}

impl Evidence {
    /// True if the check exited successfully.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failure_reason.is_none() && self.exit == Some(ProcessExit::Code(0))
    }

    /// The reason at the delivery boundary, capped even for programmatically built evidence.
    pub(crate) fn bounded_failure_reason(&self) -> Option<String> {
        self.failure_reason.as_ref().map(|reason| {
            reason
                .chars()
                .take(MAX_CHECK_FAILURE_REASON_CHARS)
                .collect()
        })
    }

    /// True if this evidence was produced on exactly `candidate`'s content. Any change to the
    /// candidate after the check, even one line, makes it stale (T21d).
    #[must_use]
    pub fn applies_to(&self, candidate: &Candidate) -> bool {
        self.failure_reason.is_none() && self.candidate_tree == candidate.tree
    }
}

/// Runs every test command in `checks` on a clean checkout of `candidate`, noting which check
/// definitions it changed since `base`. Each check gets a private temporary directory beneath
/// `attempt_directory`; only it and the disposable checkout are writable.
///
/// # Errors
///
/// Returns [`GoalError`] if the checkout cannot be made or a check cannot be started. A check
/// that runs and fails is evidence, not an error. A check that changes tracked candidate
/// sources produces invalid evidence with a fixed failure reason; no later check is run.
pub fn run_trusted_checks(
    repository: &Path,
    state_directory: &Path,
    attempt_directory: &Path,
    base: &str,
    candidate: &Candidate,
    checks: &[DetectedCommand],
) -> Result<Vec<Evidence>, GoalError> {
    let definitions_changed: Vec<String> = changed_check_definitions(repository, base, candidate)
        .map_err(at("comparing check definitions"))?;
    let weakened_tests = kaisa_workspace::weakened_tests(repository, base, candidate)
        .map_err(at("comparing candidate tests"))?;
    let place: PathBuf = state_directory
        .join("checks")
        .join(format!("check_{}", fresh_suffix()));
    std::fs::create_dir_all(state_directory.join("checks")).map_err(at("preparing checks"))?;
    let checkout = CandidateCheckout::create(repository, candidate, &place)
        .map_err(at("checking out the candidate"))?;
    // Record the gitdir before any check runs: never rediscover it through a pointer a
    // check could have replaced, including on platforms where checks run reduced.
    let sources = CheckedSources::record(checkout.path())
        .map_err(at("recording the check checkout's Git directory"))?;
    let mut evidence: Vec<Evidence> = Vec::new();
    for check in checks.iter().filter(|check| check.purpose == Purpose::Test) {
        let Some((program, args)) = check.argv.split_first() else {
            continue;
        };
        let directory: PathBuf = check.directory.as_ref().map_or_else(
            || checkout.path().to_owned(),
            |relative| checkout.path().join(relative.as_str()),
        );
        let temporary = CheckTemporary::create(attempt_directory)
            .map_err(at("preparing the check temporary directory"))?;
        let spec = ProcessSpec {
            program: PathBuf::from(program),
            args: args.to_vec(),
            working_directory: directory,
            env: ["TMPDIR", "TEMP", "TMP"]
                .into_iter()
                .map(|key| (key.to_owned(), temporary.0.to_string_lossy().into_owned()))
                .collect(),
        };
        let sandbox = check_sandbox(state_directory, checkout.path(), &temporary.0)
            .map_err(at("building the check sandbox"))?;
        let run: CapturedRun = run_captured(
            &spec,
            &sandbox.confinement(),
            &[],
            CHECK_DEADLINE,
            CHECK_OUTPUT_CAP,
        )
        .map_err(at("running a trusted check"))?;
        let source_changed = !sources
            .matches(checkout.path(), &candidate.tree)
            .map_err(at("validating checked candidate sources"))?;
        evidence.push(Evidence {
            candidate_tree: candidate.tree.clone(),
            check: check.argv.clone(),
            exit: run.exit,
            failure_reason: source_changed.then(|| CHANGED_CHECK_SOURCES.to_owned()),
            output_digest: blake3::hash(&run.output).to_hex().to_string(),
            confined: sandbox.is_confined(),
            definitions_changed: definitions_changed.clone(),
            weakened_tests: weakened_tests.clone(),
        });
        if source_changed {
            // Keep the failed evidence, but never let later checks run on changed sources.
            break;
        }
    }
    Ok(evidence)
}

/// The clean checkout's original Git metadata, pinned before executing untrusted checks.
struct CheckedSources {
    git_directory: PathBuf,
}

impl CheckedSources {
    fn record(checkout: &Path) -> std::io::Result<Self> {
        let pointer = std::fs::read_to_string(checkout.join(".git"))?;
        let git_directory = pointer
            .trim_end()
            .strip_prefix("gitdir: ")
            .ok_or_else(|| std::io::Error::other("invalid check checkout Git pointer"))?;
        Ok(Self {
            git_directory: checkout.join(git_directory).canonicalize()?,
        })
    }

    fn matches(&self, checkout: &Path, tree: &str) -> std::io::Result<bool> {
        // The recorded directory is canonical, which on Windows means a `\\?\` path that Git
        // cannot follow to the worktree's `commondir`; Git gets the ordinary spelling.
        let git_directory = format!(
            "--git-dir={}",
            kaisa_workspace::git_cli_path(&self.git_directory)
        );
        let work_tree = format!("--work-tree={}", kaisa_workspace::git_cli_path(checkout));
        let mut command = Command::new("git");
        command
            .current_dir(checkout)
            .arg(git_directory)
            .arg(work_tree)
            .args([
                "-c",
                "core.fsmonitor=false",
                "-c",
                "diff.ignoreSubmodules=all",
                "-c",
                "submodule.recurse=false",
                "diff",
                "--quiet",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=all",
                tree,
                "--",
            ])
            .env("GIT_TERMINAL_PROMPT", "0");
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_CONFIG",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
        ] {
            command.env_remove(key);
        }
        let output = command.output()?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(std::io::Error::other(format!(
                "checking candidate sources failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
        }
    }
}

/// A private directory per check, reclaimed even when starting the check fails.
struct CheckTemporary(PathBuf);

impl CheckTemporary {
    fn create(attempt_directory: &Path) -> std::io::Result<Self> {
        let path = attempt_directory.join(format!("check-tmp_{}", fresh_suffix()));
        kaisa_sys::ensure_private_directory(&path)?;
        Ok(Self(path.canonicalize()?))
    }
}

impl Drop for CheckTemporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The sandbox a check runs in: writes only its disposable checkout and private temp, no network.
enum CheckSandbox {
    // macOS always confines checks; Linux falls back here on kernels without Landlock.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Unconfined,
    #[cfg(target_os = "macos")]
    Seatbelt(kaisa_sys::SeatbeltPolicy),
    #[cfg(target_os = "linux")]
    Landlock(kaisa_sys::LandlockRuleset),
}

impl CheckSandbox {
    fn confinement(&self) -> Confinement<'_> {
        match self {
            Self::Unconfined => Confinement::Unconfined,
            #[cfg(target_os = "macos")]
            Self::Seatbelt(policy) => Confinement::Seatbelt(policy),
            #[cfg(target_os = "linux")]
            Self::Landlock(ruleset) => Confinement::Landlock(ruleset),
        }
    }

    fn is_confined(&self) -> bool {
        !matches!(self, Self::Unconfined)
    }
}

fn check_sandbox(state: &Path, checkout: &Path, temporary: &Path) -> std::io::Result<CheckSandbox> {
    #[cfg(not(target_os = "macos"))]
    let _ = state;
    #[cfg(not(target_os = "linux"))]
    let writable: Vec<PathBuf> = vec![checkout.canonicalize()?, temporary.canonicalize()?];
    #[cfg(target_os = "macos")]
    {
        let mut policy = kaisa_sys::SeatbeltPolicy {
            readable: writable.clone(),
            writable,
            git_control_root: Some(checkout.canonicalize()?),
            ..kaisa_sys::SeatbeltPolicy::default()
        };
        policy.protect_state(state)?;
        Ok(CheckSandbox::Seatbelt(policy))
    }
    #[cfg(target_os = "linux")]
    {
        let writable = linux_check_writable(checkout, temporary)?;
        Ok(match kaisa_sys::write_ruleset(&writable, None)? {
            Some(ruleset) => CheckSandbox::Landlock(ruleset),
            None => CheckSandbox::Unconfined,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = writable;
        Ok(CheckSandbox::Unconfined)
    }
}

/// Landlock grants cannot subtract root Git controls from a checkout-wide grant. Grant
/// existing children instead, and prepare common build directories before confinement.
#[cfg(any(target_os = "linux", all(test, unix)))]
fn linux_check_writable(checkout: &Path, temporary: &Path) -> std::io::Result<Vec<PathBuf>> {
    for output in crate::goal::BUILD_OUTPUT {
        let directory = checkout.join(output);
        if !directory.try_exists()? {
            std::fs::create_dir(&directory)?;
        }
    }
    let mut writable = vec![temporary.canonicalize()?];
    for entry in std::fs::read_dir(checkout)? {
        let entry = entry?;
        if [".git", ".gitattributes", ".gitmodules"]
            .iter()
            .any(|control| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(control)
            })
            || entry.file_type()?.is_symlink()
        {
            continue;
        }
        writable.push(entry.path().canonicalize()?);
    }
    Ok(writable)
}

#[cfg(all(test, unix))]
mod tests {
    use super::linux_check_writable;

    #[cfg(target_os = "macos")]
    #[test]
    fn a_trusted_check_cannot_read_state_secrets_but_can_read_its_checkout() {
        let state = std::env::temp_dir().join(format!("kaisa-check-state-{}", std::process::id()));
        let checkout = state.join("checks/own");
        let temporary = state.join("attempts/own/check-temp");
        std::fs::create_dir_all(&checkout).expect("check checkout");
        std::fs::create_dir_all(&temporary).expect("check temporary directory");
        let secret = state.join("kaisa.db");
        let source = checkout.join("source.rs");
        for file in [&secret, &source] {
            std::fs::write(file, "fixture").expect("planted fixture");
        }
        let sandbox = super::check_sandbox(&state, &checkout, &temporary).expect("check sandbox");
        let read = |path: &std::path::Path| {
            let spec = kaisa_sys::ProcessSpec {
                program: "/bin/cat".into(),
                args: vec![path.to_string_lossy().into_owned()],
                working_directory: checkout.clone(),
                env: Vec::new(),
            };
            kaisa_sys::run_captured(
                &spec,
                &sandbox.confinement(),
                b"",
                super::CHECK_DEADLINE,
                super::CHECK_OUTPUT_CAP,
            )
            .expect("check process exits")
            .exit
            .expect("cat exits before deadline")
        };
        const SUCCESS: kaisa_sys::ProcessExit = kaisa_sys::ProcessExit::Code(0);
        assert_ne!(
            read(&secret),
            SUCCESS,
            "checks must not expose daemon storage"
        );
        assert_eq!(
            read(&source),
            SUCCESS,
            "checks still need their candidate source"
        );
        std::fs::remove_dir_all(state).expect("remove isolated check state");
    }

    #[test]
    fn linux_check_grants_exclude_root_git_controls_and_symlinks_but_allow_build_directories() {
        let base = std::env::temp_dir().join(format!("kaisa-check-grants-{}", std::process::id()));
        let checkout = base.join("checkout");
        let temporary = base.join("private-temp");
        let outside = base.join("other-attempt");
        let _ = std::fs::remove_dir_all(&base);
        for directory in [&checkout, &temporary, &outside] {
            std::fs::create_dir_all(directory).expect("the check fixture is writable");
        }
        for control in [".git", ".gitattributes", ".gitmodules"] {
            std::fs::write(checkout.join(control), "protected").expect("a root Git control");
        }
        std::os::unix::fs::symlink(&outside, checkout.join("outside-link"))
            .expect("a candidate symlink to private state");
        let writable = linux_check_writable(&checkout, &temporary).expect("check grants");
        assert!(
            !writable.contains(&checkout.canonicalize().expect("checkout exists"))
                && !writable.contains(&outside.canonicalize().expect("outside exists"))
                && [".git", ".gitattributes", ".gitmodules"]
                    .iter()
                    .all(|control| !writable.contains(
                        &checkout
                            .join(control)
                            .canonicalize()
                            .expect("control exists")
                    )),
            "Landlock must never receive the root, a Git control, or an outside symlink grant: {writable:?}"
        );
        assert!(
            crate::goal::BUILD_OUTPUT
                .iter()
                .all(|output| writable.contains(
                    &checkout
                        .join(output)
                        .canonicalize()
                        .expect("build directory exists")
                )),
            "build outputs must remain writable without a checkout-wide Landlock grant"
        );
        std::fs::remove_dir_all(base).expect("remove the check fixture");
    }
}
