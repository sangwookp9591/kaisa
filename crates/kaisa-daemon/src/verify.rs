//! Trusted checks: the project's own test commands, run by Kaisa on a clean checkout of the
//! frozen candidate, in the OS sandbox, with no agent involved (T21b, ADR-0015). What they
//! report is evidence bound to the candidate's tree.

use std::path::{Path, PathBuf};
use std::time::Duration;

use kaisa_context::{DetectedCommand, Purpose};
use kaisa_sys::{CapturedRun, Confinement, ProcessExit, ProcessSpec, run_captured};
use kaisa_workspace::{Candidate, CandidateCheckout};

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
}

impl Evidence {
    /// True if the check exited successfully.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.exit == Some(ProcessExit::Code(0))
    }
}

/// Runs every test command in `checks` on a clean checkout of `candidate`.
///
/// # Errors
///
/// Returns [`GoalError`] if the checkout cannot be made or a check cannot be started. A check
/// that runs and fails is evidence, not an error.
pub fn run_trusted_checks(
    repository: &Path,
    state_directory: &Path,
    candidate: &Candidate,
    checks: &[DetectedCommand],
) -> Result<Vec<Evidence>, GoalError> {
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
        let spec = ProcessSpec {
            program: PathBuf::from(program),
            args: args.to_vec(),
            working_directory: directory,
            env: Vec::new(),
        };
        let sandbox = check_sandbox(checkout.path()).map_err(at("building the check sandbox"))?;
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
        });
    }
    Ok(evidence)
}

/// The sandbox a check runs in: it may change only its own checkout and has no network.
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

fn check_sandbox(checkout: &Path) -> std::io::Result<CheckSandbox> {
    let writable: Vec<PathBuf> = vec![checkout.canonicalize()?];
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
