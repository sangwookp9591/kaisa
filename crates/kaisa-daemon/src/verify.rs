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
/// Maximum UTF-8 bytes of scrubbed captured output given to a reviewer per check.
pub(crate) const CHECK_OUTPUT_TAIL_BYTES: usize = 4 * 1024;
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
    /// Recognised test harness completion; None means exit-code only.
    pub test_evidence: Option<bool>,
    /// The digest of its output, so the evidence can be matched to a stored transcript.
    pub output_digest: String,
    /// Bounded captured output tail, scrubbed before it can enter a recorded pack.
    pub output_tail: String,
    /// False if the check ran without OS confinement (Windows until checks get a container).
    pub confined: bool,
    /// Check-definition files the candidate changed since its base; when any are named, the
    /// check may be testing rules the agent wrote, so a person must accept the result.
    pub definitions_changed: Vec<String>,
    /// File-specific test weakening signals. Any of them stops delivery for a person.
    pub weakened_tests: Vec<String>,
    /// Reviewer-visible process termination advice; never a plain-goal person gate.
    pub advisories: Vec<String>,
}

impl Evidence {
    /// True if the check exited successfully with the required harness evidence.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failure_reason.is_none()
            && self.test_evidence != Some(false)
            && self.exit == Some(ProcessExit::Code(0))
    }

    /// Human-readable validation boundary for the receipt and revision feedback.
    #[must_use]
    pub fn validation_label(&self) -> &'static str {
        match self.test_evidence {
            None => "exit-code only",
            Some(true) => "positive test evidence",
            Some(false) if self.exit == Some(ProcessExit::Code(0)) => "no positive test evidence",
            Some(false) => "test command failed",
        }
    }

    /// Actionable explanation when exit zero supplied no positive completion evidence.
    #[must_use]
    pub fn missing_test_evidence_reason(&self) -> Option<String> {
        if self.test_evidence != Some(false) || self.exit != Some(ProcessExit::Code(0)) {
            return None;
        }
        let missing =
            if self.output_tail.contains("0 passed;") || self.output_tail.contains("# tests 0") {
                "the command exited 0 but reported no executed tests"
            } else {
                "the command exited 0 but reported no complete passing test summary"
            };
        Some(format!(
            "no positive test evidence from `{}`: {missing}; run at least one test with a supported text reporter and rerun kaisa",
            self.check.join(" ")
        ))
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
    let mut definitions_changed: Vec<String> =
        changed_check_definitions(repository, base, candidate)
            .map_err(at("comparing check definitions"))?;
    let referenced = referenced_check_paths(repository, base, checks)?;
    for path in kaisa_workspace::changed_paths(repository, base, candidate)
        .map_err(at("comparing referenced check scripts"))?
    {
        if referenced.contains(&path) && !definitions_changed.contains(&path) {
            definitions_changed.push(path);
        }
    }
    definitions_changed.sort();

    let mut weakened_tests = kaisa_workspace::weakened_tests(repository, base, candidate)
        .map_err(at("comparing candidate tests"))?;
    let (configuration, advisories) = crate::qa::plain_qa_findings(repository, base, candidate)?;
    weakened_tests.extend(configuration);
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
        let harness = trusted_test_harness(repository, base, check)?;
        let test_evidence = harness.map(|harness| {
            let complete = crate::qa::harness_test_evidence(&run, None, harness);
            if harness != crate::qa::TestHarness::Tap {
                return complete;
            }
            let output = String::from_utf8_lossy(&run.output);
            complete
                && !output.lines().any(|line| {
                    let line = line.trim_start();
                    let name = line
                        .strip_prefix("ok ")
                        .and_then(|s| s.split_once(" - ").map(|(_, name)| name))
                        .or_else(|| {
                            line.strip_prefix("✔ ")
                                .map(|s| s.rsplit_once(" (").map_or(s, |(name, _)| name))
                        });
                    let Some(name) = name else {
                        return false;
                    };
                    let name = name.split(" #").next().unwrap_or(name);
                    // Node's exit-zero wrapper identifies a file that actually exists, unlike
                    // a descriptive test name merely ending in .js or .ts.
                    let path = Path::new(name);
                    spec.working_directory.join(path).is_file()
                })
        });
        evidence.push(Evidence {
            candidate_tree: candidate.tree.clone(),
            check: check.argv.clone(),
            exit: run.exit,
            failure_reason: source_changed.then(|| CHANGED_CHECK_SOURCES.to_owned()),
            test_evidence,
            output_digest: blake3::hash(&run.output).to_hex().to_string(),
            output_tail: review_output_tail(&run),
            confined: sandbox.is_confined(),
            definitions_changed: definitions_changed.clone(),
            weakened_tests: weakened_tests.clone(),
            advisories: advisories.clone(),
        });
        if source_changed {
            // Keep the failed evidence, but never let later checks run on changed sources.
            break;
        }
    }
    Ok(evidence)
}

/// Classify from trusted argv and the base package script, never candidate package metadata.
fn trusted_test_harness(
    repository: &Path,
    base: &str,
    check: &DetectedCommand,
) -> Result<Option<crate::qa::TestHarness>, GoalError> {
    use crate::qa::TestHarness;
    fn direct(argv: &[String]) -> Option<TestHarness> {
        let program = Path::new(argv.first()?).file_stem()?.to_str()?;
        let args = &argv[1..];
        if program == "cargo"
            && args
                .iter()
                .take_while(|arg| arg.as_str() != "--")
                .any(|arg| arg == "--no-run")
        {
            return None;
        }
        if program == "node"
            && args
                .iter()
                .any(|arg| arg == "--test" || arg.starts_with("--test="))
        {
            return Some(TestHarness::Tap);
        }
        if program == "cargo" {
            let mut args = args.iter();
            while let Some(arg) = args.next() {
                if [
                    "--color",
                    "--config",
                    "--manifest-path",
                    "--target-dir",
                    "-C",
                    "-Z",
                ]
                .contains(&arg.as_str())
                {
                    args.next();
                } else if arg.starts_with('-') || arg.starts_with('+') {
                    continue;
                } else {
                    return match arg.as_str() {
                        "test" | "t" => Some(TestHarness::Libtest),
                        "nextest" if args.next().is_some_and(|arg| arg == "run") => {
                            Some(TestHarness::Nextest)
                        }
                        _ => None,
                    };
                }
            }
        }
        None
    }
    if let Some(harness) = direct(&check.argv) {
        return Ok(Some(harness));
    }
    let Some(program) = check
        .argv
        .first()
        .and_then(|p| Path::new(p).file_stem())
        .and_then(|p| p.to_str())
    else {
        return Ok(None);
    };
    if !matches!(program, "npm" | "pnpm" | "yarn" | "bun") {
        return Ok(None);
    }
    let script = match check.argv.get(1).map(String::as_str) {
        Some("run" | "run-script") => check.argv.get(2),
        Some(_) => check.argv.get(1),
        None => None,
    };
    let Some(script) = script else {
        return Ok(None);
    };
    let manifest = check.directory.as_ref().map_or_else(
        || "package.json".to_owned(),
        |dir| format!("{}/package.json", dir.as_str()),
    );
    let output = Command::new("git")
        .args(["show", &format!("{base}:{manifest}")])
        .current_dir(repository)
        .env_clear()
        .envs(kaisa_sys::base_environment())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .output()
        .map_err(at("reading trusted test runner"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let package: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
    Ok(package
        .get("scripts")
        .and_then(|scripts| scripts.get(script))
        .and_then(serde_json::Value::as_str)
        .and_then(|body| shell_words::split(body).ok())
        .and_then(|argv| direct(&argv)))
}

/// Resolve only trusted argv and base-revision package scripts. Candidate text never decides
/// which files define a check. Any file operand is conservative: data/config operands can
/// influence a check just as an interpreter script or a make -f file can.
fn referenced_check_paths(
    repository: &Path,
    base: &str,
    checks: &[DetectedCommand],
) -> Result<std::collections::BTreeSet<String>, GoalError> {
    let mut paths = std::collections::BTreeSet::new();
    for check in checks.iter().filter(|check| check.purpose == Purpose::Test) {
        let directory = check.directory.as_ref().map_or("", |path| path.as_str());
        add_argv_paths(&mut paths, directory, &check.argv);
        let Some(program) = check.argv.first() else {
            continue;
        };
        if !matches!(program.as_str(), "npm" | "pnpm" | "yarn" | "bun") {
            continue;
        }
        let script = match check.argv.get(1).map(String::as_str) {
            Some("run" | "run-script") => check.argv.get(2),
            Some(_) => check.argv.get(1),
            None => None,
        };
        let Some(script) = script else { continue };
        let manifest = if directory.is_empty() {
            "package.json".to_owned()
        } else {
            format!("{directory}/package.json")
        };
        let output = Command::new("git")
            .args(["show", &format!("{base}:{manifest}")])
            .current_dir(repository)
            .output()
            .map_err(at("reading trusted package scripts"))?;
        if !output.status.success() {
            continue;
        }
        let Ok(package) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
            continue;
        };
        // npm also executes the selected script's lifecycle hooks.
        for name in [
            format!("pre{script}"),
            script.clone(),
            format!("post{script}"),
        ] {
            if let Some(body) = package
                .get("scripts")
                .and_then(|scripts| scripts.get(&name))
                .and_then(serde_json::Value::as_str)
            {
                add_shell_paths(&mut paths, directory, body);
            }
        }
    }
    // Only existing base files are definitions. A test such as `test -f src/new.md`
    // names an expected output, so creating it must remain ordinary candidate work.
    let tracked = Command::new("git")
        .args(["ls-tree", "-r", "--name-only", "-z", base, "--"])
        .current_dir(repository)
        .env_clear()
        .envs(kaisa_sys::base_environment())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .output()
        .map_err(at("listing trusted check files"))?;
    if !tracked.status.success() {
        return Err(GoalError {
            step: "listing trusted check files",
            detail: String::from_utf8_lossy(&tracked.stderr).into_owned(),
        });
    }
    let existing = String::from_utf8_lossy(&tracked.stdout)
        .split('\0')
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    paths.retain(|path| existing.contains(path));
    Ok(paths)
}

fn add_shell_paths(paths: &mut std::collections::BTreeSet<String>, directory: &str, body: &str) {
    if let Ok(argv) = shell_words::split(body) {
        add_argv_paths(paths, directory, &argv);
    }
}

fn add_argv_paths(
    paths: &mut std::collections::BTreeSet<String>,
    directory: &str,
    argv: &[String],
) {
    for argument in argv {
        let path = Path::new(argument);
        if argument.is_empty() || path.is_absolute() {
            continue;
        }
        let mut relative = PathBuf::from(directory);
        let mut safe = true;
        for component in path.components() {
            match component {
                std::path::Component::Normal(name) => relative.push(name),
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir if relative.pop() => {}
                _ => {
                    safe = false;
                    break;
                }
            }
        }
        if safe {
            paths.insert(
                relative
                    .iter()
                    .map(|part| part.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }
    // Shell command strings are quoted argv, not paths themselves.
    for pair in argv.windows(2) {
        if pair[0] == "-c" {
            add_shell_paths(paths, directory, &pair[1]);
        }
    }
}

/// Scan the complete capture before taking a tail: truncation must not hide a secret.
pub(crate) fn review_output_tail(run: &CapturedRun) -> String {
    if !kaisa_workspace::scan_bytes(&run.output).is_empty() {
        return "[omitted: secret detected]".to_owned();
    }
    // A truncated capture contains a prefix, not the process's final output. Do not imply
    // that its last bytes are the final lines, or that uncaptured bytes were scanned.
    if run.truncated {
        return "[omitted: check output exceeded capture limit]".to_owned();
    }
    let output = String::from_utf8_lossy(&run.output);
    let mut start = output.len().saturating_sub(CHECK_OUTPUT_TAIL_BYTES);
    while !output.is_char_boundary(start) {
        start += 1;
    }
    output[start..].to_owned()
}

/// The clean checkout's original Git metadata, pinned before executing untrusted checks.
pub(crate) struct CheckedSources {
    git_directory: PathBuf,
}

impl CheckedSources {
    pub(crate) fn record(checkout: &Path) -> std::io::Result<Self> {
        let pointer = std::fs::read_to_string(checkout.join(".git"))?;
        let git_directory = pointer
            .trim_end()
            .strip_prefix("gitdir: ")
            .ok_or_else(|| std::io::Error::other("invalid check checkout Git pointer"))?;
        Ok(Self {
            git_directory: checkout.join(git_directory).canonicalize()?,
        })
    }

    pub(crate) fn matches(&self, checkout: &Path, tree: &str) -> std::io::Result<bool> {
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
pub(crate) enum CheckSandbox {
    // macOS always confines checks; Linux falls back here on kernels without Landlock.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Unconfined,
    #[cfg(target_os = "macos")]
    Seatbelt(kaisa_sys::SeatbeltPolicy),
    #[cfg(target_os = "linux")]
    Landlock(kaisa_sys::LandlockRuleset),
}

impl CheckSandbox {
    pub(crate) fn confinement(&self) -> Confinement<'_> {
        match self {
            Self::Unconfined => Confinement::Unconfined,
            #[cfg(target_os = "macos")]
            Self::Seatbelt(policy) => Confinement::Seatbelt(policy),
            #[cfg(target_os = "linux")]
            Self::Landlock(ruleset) => Confinement::Landlock(ruleset),
        }
    }

    pub(crate) fn is_confined(&self) -> bool {
        !matches!(self, Self::Unconfined)
    }
}

pub(crate) fn check_sandbox(
    state: &Path,
    checkout: &Path,
    temporary: &Path,
) -> std::io::Result<CheckSandbox> {
    check_sandbox_readonly(state, checkout, temporary, &[])
}

/// Checks with immutable harness inputs excluded from every writable grant.
pub(crate) fn check_sandbox_readonly(
    state: &Path,
    checkout: &Path,
    temporary: &Path,
    readonly: &[PathBuf],
) -> std::io::Result<CheckSandbox> {
    #[cfg(not(target_os = "macos"))]
    let _ = state;
    #[cfg(not(target_os = "linux"))]
    let writable: Vec<PathBuf> = vec![checkout.canonicalize()?, temporary.canonicalize()?];
    #[cfg(target_os = "macos")]
    {
        let mut policy = kaisa_sys::SeatbeltPolicy {
            readable: writable.clone(),
            writable,
            unwritable: readonly.to_vec(),
            git_control_root: Some(checkout.canonicalize()?),
            ..kaisa_sys::SeatbeltPolicy::default()
        };
        policy.protect_state(state)?;
        Ok(CheckSandbox::Seatbelt(policy))
    }
    #[cfg(target_os = "linux")]
    {
        let writable = linux_check_writable(checkout, temporary)?
            .into_iter()
            .filter(|path| {
                !readonly
                    .iter()
                    .any(|protected| path == protected || protected.starts_with(path))
            })
            .collect::<Vec<_>>();
        Ok(match kaisa_sys::write_ruleset(&writable, None)? {
            Some(ruleset) => CheckSandbox::Landlock(ruleset),
            None => CheckSandbox::Unconfined,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (writable, readonly);
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

    /// Interpreter, explicit executable and makefile operands, including quoted shell paths.
    fn script_invocations() -> Vec<(Vec<String>, &'static str, &'static str)> {
        [
            (vec!["sh", "x.sh"], "", "x.sh"),
            (vec!["bash", "scripts/test.sh"], "", "scripts/test.sh"),
            (
                vec!["python", "tools/run_tests.py"],
                "",
                "tools/run_tests.py",
            ),
            (vec!["make", "-f", "checks/Test.mk"], "", "checks/Test.mk"),
            (vec!["./tools/test"], "", "tools/test"),
            (vec!["bash", "./test.sh"], "web", "web/test.sh"),
            (vec!["bash", "../tools/test.sh"], "web", "tools/test.sh"),
            (
                vec!["sh", "-c", "bash 'scripts/test suite.sh'"],
                "",
                "scripts/test suite.sh",
            ),
        ]
        .into_iter()
        .map(|(argv, directory, expected)| {
            (
                argv.into_iter().map(str::to_owned).collect(),
                directory,
                expected,
            )
        })
        .collect()
    }

    #[test]
    fn trusted_interpreter_and_make_operands_resolve_to_repository_relative_check_paths() {
        for (argv, directory, expected) in script_invocations() {
            let mut paths = std::collections::BTreeSet::new();
            super::add_argv_paths(&mut paths, directory, &argv);
            assert!(
                paths.contains(expected),
                "trusted argv {argv:?} in {directory} must protect {expected}: {paths:?}"
            );
        }
    }

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
        for command in commands_walking_state_ancestors(&checkout) {
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

#[cfg(test)]
mod output_tests {
    use kaisa_sys::{CapturedRun, ProcessExit};

    use super::{CHECK_OUTPUT_TAIL_BYTES, review_output_tail};

    fn captured_output(text: &str) -> CapturedRun {
        CapturedRun {
            output: text.as_bytes().to_vec(),
            exit: Some(ProcessExit::Code(0)),
            truncated: false,
        }
    }

    #[test]
    fn long_check_output_keeps_only_a_utf8_tail_within_the_named_limit() {
        let output = format!("{}final result", "검사".repeat(CHECK_OUTPUT_TAIL_BYTES));
        let tail = review_output_tail(&captured_output(&output));
        assert!(
            tail.len() <= CHECK_OUTPUT_TAIL_BYTES && tail.ends_with("final result"),
            "the final result survives without exceeding the byte cap or splitting UTF-8"
        );
    }

    #[test]
    fn a_secret_before_the_visible_tail_omits_the_entire_output() {
        let output = format!(
            "AKIAABCDEFGHIJKLMNOP\n{}",
            "passed\n".repeat(CHECK_OUTPUT_TAIL_BYTES)
        );
        let tail = review_output_tail(&captured_output(&output));
        assert_eq!(
            tail, "[omitted: secret detected]",
            "scanning only the bounded tail would conceal an earlier credential"
        );
    }

    #[test]
    fn a_utf16_secret_in_check_output_omits_the_entire_output() {
        const AWS_KEY_LINE: &str = "credential: AKIAABCDEFGHIJKLMNOP\n";
        for little_endian in [true, false] {
            let mut run = captured_output("");
            run.output = AWS_KEY_LINE
                .encode_utf16()
                .flat_map(|unit| {
                    if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect();
            assert_eq!(
                review_output_tail(&run),
                "[omitted: secret detected]",
                "raw UTF-16 credentials must be caught before lossy decoding (LE={little_endian})"
            );
        }
    }

    #[test]
    fn a_truncated_capture_is_omitted_instead_of_claiming_to_be_the_final_tail() {
        let mut run = captured_output("initial output");
        run.truncated = true;
        assert_eq!(
            review_output_tail(&run),
            "[omitted: check output exceeded capture limit]",
            "a prefix capture cannot prove the final output or the absence of later secrets"
        );
    }
}
