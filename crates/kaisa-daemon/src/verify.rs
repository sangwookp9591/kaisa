//! Trusted checks: the project's own test commands, run by Kaisa on a clean checkout of the
//! frozen candidate, in the OS sandbox, with no agent involved (T21b, ADR-0015). What they
//! report is evidence bound to the candidate's tree.

use std::path::{Path, PathBuf};
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

/// What one trusted check showed about one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// The candidate tree the check ran on.
    pub candidate_tree: String,
    /// The command, as run.
    pub check: Vec<String>,
    /// How it ended; `None` if it ran past its deadline.
    pub exit: Option<ProcessExit>,
    /// The digest of its output, so the evidence can be matched to a stored transcript.
    pub output_digest: String,
    /// False if the check ran without OS confinement (Windows until checks get a container).
    pub confined: bool,
    /// Check-definition files the candidate changed since its base; when any are named, the
    /// check may be testing rules the agent wrote, so a person must accept the result.
    pub definitions_changed: Vec<String>,
}

impl Evidence {
    /// True if the check exited successfully.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.exit == Some(ProcessExit::Code(0))
    }

    /// True if this evidence was produced on exactly `candidate`'s content. Any change to the
    /// candidate after the check, even one line, makes it stale (T21d).
    #[must_use]
    pub fn applies_to(&self, candidate: &Candidate) -> bool {
        self.candidate_tree == candidate.tree
    }
}

/// Runs every test command in `checks` on a clean checkout of `candidate`, noting which check
/// definitions it changed since `base`. Each check gets a private temporary directory beneath
/// `attempt_directory`; only it and the disposable checkout are writable.
///
/// # Errors
///
/// Returns [`GoalError`] if the checkout cannot be made or a check cannot be started. A check
/// that runs and fails is evidence, not an error.
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
    let place: PathBuf = state_directory
        .join("checks")
        .join(format!("check_{}", fresh_suffix()));
    std::fs::create_dir_all(state_directory.join("checks")).map_err(at("preparing checks"))?;
    let checkout = CandidateCheckout::create(repository, candidate, &place)
        .map_err(at("checking out the candidate"))?;
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
        let sandbox = check_sandbox(checkout.path(), &temporary.0)
            .map_err(at("building the check sandbox"))?;
        let run: CapturedRun = run_captured(
            &spec,
            &sandbox.confinement(),
            &[],
            CHECK_DEADLINE,
            CHECK_OUTPUT_CAP,
        )
        .map_err(at("running a trusted check"))?;
        evidence.push(Evidence {
            candidate_tree: candidate.tree.clone(),
            check: check.argv.clone(),
            exit: run.exit,
            output_digest: blake3::hash(&run.output).to_hex().to_string(),
            confined: sandbox.is_confined(),
            definitions_changed: definitions_changed.clone(),
        });
    }
    Ok(evidence)
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

fn check_sandbox(checkout: &Path, temporary: &Path) -> std::io::Result<CheckSandbox> {
    let writable: Vec<PathBuf> = vec![checkout.canonicalize()?, temporary.canonicalize()?];
    #[cfg(target_os = "macos")]
    {
        Ok(CheckSandbox::Seatbelt(kaisa_sys::SeatbeltPolicy {
            writable,
            ..kaisa_sys::SeatbeltPolicy::default()
        }))
    }
    #[cfg(target_os = "linux")]
    {
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
