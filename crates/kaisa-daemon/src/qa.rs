//! Independent private QA after a developer has exited. QA sees only a frozen spec and an
//! allow-listed public interface, proposes files through one strict fence, and never writes
//! the candidate. A suite builds once in a disposable checkout; each scenario checks immutable
//! external private inputs, artifact digests and positive named-test harness evidence.
//! Runtime private-QA write denial and author isolation were exercised only on macOS;
//! native Linux/Windows guarantees remain unverified (NFR-04/NFR-08). The Linux/Windows
//! cross-clippy checks in scripts/check.sh are compile-only; this macOS host's Windows
//! cross-check excludes the SQLite-dependent daemon.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kaisa_domain::{
    AttemptId, ContainmentCapability, ContentDigest, EventKind, Execution, ExitStatus, Sequence,
};
use kaisa_ledger::EventStore;
use kaisa_sys::{CapturedRun, ProcessExit, ProcessSpec, run_captured};
use kaisa_workspace::{Candidate, CandidateCheckout, QaKind, QaSuite, QaTestFile, record_qa_suite};
use serde_json::{Value, json};

use crate::goal::{GoalError, GoalOutcome, at, record, record_execution};
use crate::ids::fresh_suffix;
use crate::qa_interface::{PublicInterface, extract_public_interface, node_bins, rust_bins};
use crate::sandbox::AgentSandbox;
use crate::verify::{CheckedSources, check_sandbox_readonly, review_output_tail};

const QA_DEADLINE: Duration = Duration::from_secs(30 * 60);
const QA_FEEDBACK_FIELD_CAP: usize = 256;
const QA_FEEDBACK_TOTAL_CAP: usize = 4096;
const QA_EVIDENCE: &str = "candidate-influenceable-in-process";
const QA_OUTPUT_CAP: usize = 4 * 1024 * 1024;

/// Toolchain selected by the orchestrator from trusted project configuration, never by QA.
#[derive(Debug, Clone)]
pub enum QaToolchain {
    /// Cargo integration tests in a Rust package rooted at the checkout.
    Rust {
        /// Trusted cargo executable.
        cargo: PathBuf,
    },
    /// Built-in Node test runner; no package scripts or candidate-selected test commands.
    Node {
        /// Trusted node executable.
        node: PathBuf,
    },
}

/// Whether revision developers can read the already-authored private tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QaIsolation {
    /// Confined developer with enforced state-read isolation.
    Full,
    /// A named containment gap; the acceptance gate must explicitly account for it.
    Reduced(String),
}
impl QaIsolation {
    /// Stable gate/report label.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Full => "full".into(),
            Self::Reduced(reason) => format!("reduced({reason})"),
        }
    }
}

/// Computes QA isolation from actual developer containment, independent of unrelated gaps
/// such as Unix descendant reparenting. The state-read boundary is verified on macOS only.
#[must_use]
pub fn qa_isolation(developer: &GoalOutcome) -> QaIsolation {
    if developer.observed {
        return QaIsolation::Reduced("observed developer".into());
    }
    if developer
        .missing
        .contains(&ContainmentCapability::WriteConfinement)
    {
        return QaIsolation::Reduced("unconfined developer".into());
    }
    if developer
        .missing
        .contains(&ContainmentCapability::StateReadIsolation)
    {
        return QaIsolation::Reduced("developer state-read isolation unavailable".into());
    }
    if !cfg!(target_os = "macos") {
        return QaIsolation::Reduced("platform state-read QA boundary not verified".into());
    }
    QaIsolation::Full
}

/// Inputs to two independent, read-only QA attempts. Agent processes read plain stdin and
/// return raw or provider-stream final answers. The caller supplies trusted launcher argv.
pub struct QaRequest<'a> {
    /// Repository containing the frozen commit.
    pub repository: &'a Path,
    /// Trusted pre-development revision used for the QA-influencing diff scan.
    pub base: &'a str,
    /// State outside every worktree.
    pub state_directory: &'a Path,
    /// Run identifier for suite paths and events. Author once; rerun these suites for revisions.
    pub run_id: &'a str,
    /// Frozen candidate, never the developer's mutable working tree.
    pub candidate: &'a Candidate,
    /// Frozen specification JSON, including criteria with stable ids.
    pub frozen_spec: &'a Value,
    /// Completed developer receipt; its exit is checked against the ledger before QA starts.
    pub developer: &'a GoalOutcome,
    /// Acceptance-test author, consuming the isolated pack as plain stdin.
    pub author: &'a ProcessSpec,
    /// Independent adversarial-test author, consuming the isolated pack as plain stdin.
    pub blind: &'a ProcessSpec,
    /// Trusted cargo or node program.
    pub toolchain: &'a QaToolchain,
}

/// One QA attempt's recorded context receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QaAttempt {
    /// Ledger attempt id.
    pub attempt: AttemptId,
    /// Digest of exactly the isolated bytes delivered on stdin.
    pub context_pack: ContentDigest,
    /// State-read confinement may be weaker on some platforms; never silently claimed.
    pub missing: Vec<ContainmentCapability>,
}

/// Evidence for one private test file (one named scenario), including its criterion mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QaResult {
    /// Acceptance or adversarial.
    pub kind: QaKind,
    /// Scenario identifier supplied by QA.
    pub test_id: String,
    /// Frozen acceptance criterion ids.
    pub criterion_ids: Vec<String>,
    /// True only with positive named-test evidence and unchanged sources/inputs.
    pub passed: bool,
    /// BLAKE3 digest of the captured output.
    pub output_digest: String,
    /// Frozen tree tested.
    pub candidate_tree: String,
    /// Whether the check actually ran with OS confinement.
    pub confined: bool,
    /// Source changes invalidate evidence even if the test returned zero.
    pub source_unchanged: bool,
    /// OS write denial applied and installed bytes match before and after execution.
    pub private_test_unchanged: bool,
    /// Expected libtest or TAP names, pinned from the private source before execution.
    pub expected_tests: Vec<String>,
    /// Every expected name and a matching zero-failure summary were observed.
    pub positive_evidence: bool,
    /// Same-process candidate code can forge harness output (NFR-04).
    pub qa_evidence: &'static str,
    /// OS write denial, or an explicit reduced label; a digest alone is not integrity proof.
    /// `write-denied` reports mechanism availability, not native platform validation.
    pub private_test_integrity: &'static str,
    /// Blocking findings require reviewer justification with evidence before acceptance.
    pub blocking_findings: Vec<String>,
    /// Isolation of the explicit developer/revision attempt being checked.
    pub qa_isolation: QaIsolation,
    /// Observable diagnostics, deliberately excluding raw output and test source.
    pub failures: Vec<ObservableFailure>,
}

/// Feedback WP4 may safely give a revision developer. There is no test-source field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservableFailure {
    /// Criterion exercised.
    pub criterion_id: String,
    /// Scenario name.
    pub scenario: String,
    /// Kaisa-computed scenario outcome.
    pub passed: bool,
    /// Bounded, source-redacted assertion excerpt or Kaisa-computed failure reason.
    pub message: String,
}

/// All evidence the specification workflow needs from private QA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QaOutcome {
    /// Private immutable suite receipts.
    pub suites: Vec<QaSuite>,
    /// Independent author context receipts.
    pub attempts: Vec<QaAttempt>,
    /// Per-scenario results, including failures.
    pub results: Vec<QaResult>,
    /// Revision isolation gate input.
    pub qa_isolation: QaIsolation,
    /// Missing acceptance mappings block the gate even if every recorded test passes.
    pub uncovered_criteria: Vec<String>,
}

/// Authors and runs both suites, only after the recorded developer process has ended.
///
/// # Errors
/// Refuses temporal overlap, invalid QA output, unknown criterion mappings, tampered tests,
/// missing read-only confinement, or a sandbox/checkout/storage failure.
pub fn run_private_qa(request: &QaRequest<'_>) -> Result<QaOutcome, GoalError> {
    let mut ledger = EventStore::open(&request.state_directory.join("kaisa.db"))
        .map_err(at("opening private QA ledger"))?;
    require_developer_ended(&ledger, request)?;
    let criteria = criterion_ids(request.frozen_spec)?;
    let temporary = QaTemporary::create(request.state_directory)?;
    let checkout = CandidateCheckout::create(
        request.repository,
        request.candidate,
        &temporary.0.join("interface"),
    )
    .map_err(at("checking out QA public interface"))?;
    let sources =
        CheckedSources::record(checkout.path()).map_err(at("pinning QA interface tree"))?;
    let mut interface =
        extract_public_interface(checkout.path()).map_err(at("extracting QA public interface"))?;
    if qa_influencing_findings(request.repository, request.base, request.candidate)?.is_empty() {
        add_cli_help(
            request,
            checkout.path(),
            &sources,
            &temporary.0,
            &mut interface,
        )?;
    }
    let mut suites = Vec::new();
    let mut attempts = Vec::new();
    let mut uncovered_criteria = Vec::new();
    // Both authoring sessions finish before any test executes. Neither author gets the other
    // suite, developer diagnostics, project profile, lessons, diff, or revision history.
    for (kind, process) in [
        (QaKind::Acceptance, request.author),
        (QaKind::Adversarial, request.blind),
    ] {
        let pack = qa_context(request.frozen_spec, &interface, kind, request.toolchain);
        let (attempt, answer) = author_suite(
            request.state_directory,
            request.repository,
            &temporary.0,
            process,
            &pack,
            &mut ledger,
        )?;
        let tests = parse_qa_answer(&answer, &criteria)?;
        let uncovered = if kind == QaKind::Acceptance {
            let mapped: BTreeSet<_> = tests
                .iter()
                .flat_map(|test| test.criterion_ids.iter().cloned())
                .collect();
            criteria.difference(&mapped).cloned().collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        uncovered_criteria.extend(uncovered.iter().cloned());
        let suite = record_qa_suite(request.state_directory, request.run_id, kind, &tests)
            .map_err(at("sealing private QA suite"))?;
        record(
            &mut ledger,
            EventKind::QaSuiteRecorded,
            request.run_id,
            json!({"run_id":request.run_id,"kind":kind.code(),"candidate_tree":request.candidate.tree,
                "manifest_digest":suite.manifest_digest(),"attempt":attempt.attempt.as_str(),
                "uncovered_criteria":uncovered,"coverage_blocked":!uncovered.is_empty(),
                "manifest":tests.iter().map(|t| json!({"test_id":t.test_id,"criterion_ids":t.criterion_ids,
                    "file":t.file,"digest":blake3::hash(t.contents.as_bytes()).to_hex().to_string()})).collect::<Vec<_>>()}),
        )?;
        suites.push(suite);
        attempts.push(attempt);
    }
    let mut results = Vec::new();
    for suite in &suites {
        results.extend(run_qa_suite(&QaSuiteRequest {
            repository: request.repository,
            state: request.state_directory,
            run_id: request.run_id,
            base: request.base,
            candidate: request.candidate,
            suite,
            toolchain: request.toolchain,
            attempt: request.developer,
        })?);
    }
    Ok(QaOutcome {
        suites,
        attempts,
        results,
        qa_isolation: qa_isolation(request.developer),
        uncovered_criteria,
    })
}

fn require_developer_ended(ledger: &EventStore, request: &QaRequest<'_>) -> Result<(), GoalError> {
    let ended = ledger
        .attempts_from_log()
        .map_err(at("checking developer temporal isolation"))?
        .into_iter()
        .any(|attempt| {
            attempt.attempt == request.developer.attempt
                && matches!(attempt.execution, Execution::Exited(ExitStatus::Code(0)))
        });
    if !ended
        || request.developer.exit != Some(ProcessExit::Code(0))
        || request.developer.commit.as_deref() != Some(&request.candidate.commit)
    {
        return Err(error(
            "private QA requires an exited successful developer on this frozen candidate",
        ));
    }
    Ok(())
}

fn criterion_ids(spec: &Value) -> Result<BTreeSet<String>, GoalError> {
    let criteria = spec
        .get("criteria")
        .and_then(Value::as_array)
        .ok_or_else(|| error("frozen QA spec has no criteria"))?;
    let mut ids = BTreeSet::new();
    for criterion in criteria {
        let id = criterion
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| error("frozen criterion has no id"))?;
        if id.is_empty() || !ids.insert(id.to_owned()) {
            return Err(error("empty or duplicate frozen criterion id"));
        }
    }
    if ids.is_empty() {
        return Err(error("private QA requires acceptance criteria"));
    }
    Ok(ids)
}

fn qa_context(
    spec: &Value,
    interface: &PublicInterface,
    kind: QaKind,
    toolchain: &QaToolchain,
) -> String {
    let mission = match kind {
        QaKind::Acceptance => {
            "Write independent acceptance tests mapped to the frozen criterion ids."
        }
        QaKind::Adversarial => {
            "Write independent adversarial tests: boundaries, malformed input, unexpected states, error/recovery, security-relevant input, and concurrency where the public interface allows it."
        }
    };
    let placement = match toolchain {
        QaToolchain::Rust { .. } => {
            "Write flat Rust #[test] fn declarations with unique static names. Kaisa compiles them as integration targets from immutable external paths; cwd is the package root."
        }
        QaToolchain::Node { .. } => {
            "Use top-level node:test test(\"unique static name\", ...) declarations. Tests run from an immutable external directory with public module links one level above; import public modules with ../ paths."
        }
    };
    format!(
        "{mission}\nYou are a read-only QA author. Use only the quoted frozen_spec and public_interface data below. Do not inspect implementation, development tests, transcripts, developer answers, Git history or other QA suites. Do not write files or invoke tools. Your only output is exactly one fenced kaisa-qa JSON block, with no surrounding prose. Schema: {{\"tests\":[{{\"test_id\":\"scenario_name\",\"criterion_ids\":[\"AC-1\"],\"file\":\"scenario.rs\",\"contents\":\"test source\"}}]}}. Each file is one scenario; use stable descriptive test ids and assert the criterion's observable result. {placement} Use short assertion messages describing the failed observable behaviour, never test source. Runtime self-declared observations are ignored.\nUntrusted contract data (JSON):\n{}\n",
        json!({"frozen_spec":spec,"public_interface":interface.as_json()})
    )
}

/// Parses the entire final answer, never a fence hidden in prose or multiple proposals.
/// Unknown fields, duplicate JSON keys, repeated ids/files and unknown criteria are errors.
///
/// # Errors
/// Returns a fixed protocol error for any malformed proposal.
pub fn parse_qa_answer(
    answer: &str,
    criteria: &BTreeSet<String>,
) -> Result<Vec<QaTestFile>, GoalError> {
    let json = answer
        .trim()
        .strip_prefix("```kaisa-qa\n")
        .and_then(|body| body.strip_suffix("\n```"))
        .filter(|body| !body.contains("```kaisa-qa"))
        .ok_or_else(|| error("QA answer must be exactly one kaisa-qa fence"))?;
    // Deserialize through a visitor is unnecessary here: reject duplicate keys lexically
    // before Value's map decoder could silently take the last value.
    reject_duplicate_json_keys(json)?;
    let value: Value = serde_json::from_str(json).map_err(at("parsing private QA JSON"))?;
    exact_keys(&value, &["tests"])?;
    let tests = value["tests"]
        .as_array()
        .filter(|tests| !tests.is_empty())
        .ok_or_else(|| error("QA proposal has no tests"))?;
    let mut ids = BTreeSet::new();
    let mut files = BTreeSet::new();
    let mut output = Vec::new();
    for test in tests {
        exact_keys(test, &["test_id", "criterion_ids", "file", "contents"])?;
        let text = |key: &str| {
            test[key]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| error("QA proposal has an invalid string field"))
        };
        let test_id = text("test_id")?;
        let file = text("file")?;
        let contents = text("contents")?;
        let criterion_ids = test["criterion_ids"]
            .as_array()
            .filter(|ids| !ids.is_empty())
            .ok_or_else(|| error("QA test has no criterion mapping"))?
            .iter()
            .map(|id| {
                id.as_str()
                    .filter(|id| criteria.contains(*id))
                    .map(str::to_owned)
                    .ok_or_else(|| error("QA test names an unknown criterion"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let safe = |name: &str| {
            !name.is_empty()
                && name != "."
                && name != ".."
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        };
        if !safe(&test_id)
            || !safe(&file)
            || !matches!(
                Path::new(&file).extension().and_then(|s| s.to_str()),
                Some("rs" | "js" | "mjs" | "cjs")
            )
            || !ids.insert(test_id.clone())
            || !files.insert(file.clone())
            || criterion_ids.iter().collect::<BTreeSet<_>>().len() != criterion_ids.len()
        {
            return Err(error("QA proposal contains unsafe or duplicate ids/files"));
        }
        output.push(QaTestFile {
            test_id,
            criterion_ids,
            file,
            contents,
        });
    }
    Ok(output)
}

fn exact_keys(value: &Value, expected: &[&str]) -> Result<(), GoalError> {
    if value.as_object().is_none_or(|object| {
        object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key))
    }) {
        return Err(error("private QA JSON has missing or unknown fields"));
    }
    Ok(())
}

/// Minimal lexical duplicate-key check: nested object scopes each maintain their own key set.
/// JSON syntax/type validation still belongs to serde_json immediately afterwards.
fn reject_duplicate_json_keys(json: &str) -> Result<(), GoalError> {
    let bytes = json.as_bytes();
    let mut at = 0;
    let mut scopes: Vec<BTreeSet<String>> = Vec::new();
    while at < bytes.len() {
        match bytes[at] {
            b'{' => {
                scopes.push(BTreeSet::new());
                at += 1;
            }
            b'}' => {
                scopes.pop();
                at += 1;
            }
            b'"' => {
                let start = at;
                at += 1;
                while at < bytes.len() {
                    if bytes[at] == b'\\' {
                        at = (at + 2).min(bytes.len());
                    } else if bytes[at] == b'"' {
                        at += 1;
                        break;
                    } else {
                        at += 1;
                    }
                }
                let mut next = at;
                while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                    next += 1;
                }
                if bytes.get(next) == Some(&b':') {
                    let key: String = serde_json::from_str(&json[start..at]).map_err(at_error)?;
                    if scopes.last_mut().is_some_and(|keys| !keys.insert(key)) {
                        return Err(error("duplicate private QA JSON key"));
                    }
                }
            }
            _ => at += 1,
        }
    }
    Ok(())
}
fn at_error(error: serde_json::Error) -> GoalError {
    crate::goal::at("parsing QA JSON key")(error)
}
fn error(detail: &str) -> GoalError {
    GoalError {
        step: "private QA",
        detail: detail.to_owned(),
    }
}

pub(crate) fn author_suite(
    state: &Path,
    repository: &Path,
    temporary: &Path,
    process: &ProcessSpec,
    pack: &str,
    ledger: &mut EventStore,
) -> Result<(QaAttempt, String), GoalError> {
    let suffix = fresh_suffix();
    let attempt = AttemptId::try_new(&format!("att_{suffix}")).map_err(at("naming QA attempt"))?;
    let directory = temporary.join(format!("author_{suffix}"));
    let scratch = directory.join("scratch");
    let public = directory.join("public");
    std::fs::create_dir_all(&scratch).map_err(at("preparing QA scratch"))?;
    std::fs::create_dir_all(&public).map_err(at("preparing read-only QA context"))?;
    prepare_author_repository(&public, pack)?;
    let digest = ContentDigest::try_new(blake3::hash(pack.as_bytes()).to_hex().as_str())
        .map_err(at("hashing QA context"))?;
    // Characterization supplies a structured base-only pack; ordinary QA keeps its prose pack.
    let role = if serde_json::from_str::<Value>(pack)
        .ok()
        .is_some_and(|value| value["role"] == "characterization")
    {
        "characterization"
    } else {
        "qa"
    };
    record(
        ledger,
        EventKind::AttemptAdmitted,
        attempt.as_str(),
        json!({"task":format!("task_{suffix}"),"role":role,"read_only":true}),
    )?;
    ledger
        .record_context_pack(&attempt, &digest, pack.as_bytes())
        .map_err(at("recording isolated QA context"))?;
    let sandbox = author_sandbox(state, repository, &public, &scratch, process)
        .map_err(at("confining read-only QA author"))?;
    let missing = sandbox.missing();
    if missing.contains(&ContainmentCapability::WriteConfinement) {
        return Err(error("read-only QA author confinement unavailable"));
    }
    let mut process = process.clone();
    process.working_directory = public;
    process.env.extend([
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        (
            "GIT_CONFIG_GLOBAL".into(),
            if cfg!(windows) { "NUL" } else { "/dev/null" }.into(),
        ),
    ]);
    for key in ["TMPDIR", "TMP", "TEMP"] {
        process
            .env
            .push((key.to_owned(), scratch.to_string_lossy().into_owned()));
    }
    record_execution(ledger, &attempt, Execution::Starting)?;
    record_execution(ledger, &attempt, Execution::Running)?;
    let captured = run_captured(
        &process,
        &sandbox.confinement(),
        pack.as_bytes(),
        QA_DEADLINE,
        QA_OUTPUT_CAP,
    );
    sandbox.release();
    let run = match captured {
        Ok(run) => run,
        Err(err) => {
            record_execution(ledger, &attempt, Execution::Unknown)?;
            return Err(at("running QA author")(err));
        }
    };
    let exit = match run.exit {
        Some(ProcessExit::Code(code)) => ExitStatus::Code(code),
        _ => ExitStatus::Terminated,
    };
    record_execution(ledger, &attempt, Execution::Exited(exit))?;
    if run.truncated || run.exit != Some(ProcessExit::Code(0)) {
        return Err(error("QA author failed or returned truncated output"));
    }
    let answer = kaisa_agents::final_answer(&String::from_utf8_lossy(&run.output))
        .ok_or_else(|| error("QA author has no successful final answer"))?;
    Ok((
        QaAttempt {
            attempt,
            context_pack: digest,
            missing,
        },
        answer,
    ))
}

fn prepare_author_repository(public: &Path, pack: &str) -> Result<(), GoalError> {
    std::fs::write(public.join("context.json"), pack)
        .map_err(at("writing QA context repository"))?;
    for args in [
        vec!["init", "-q"],
        vec!["add", "context.json"],
        vec![
            "-c",
            "user.name=Kaisa",
            "-c",
            "user.email=kaisa@example.invalid",
            "commit",
            "-qm",
            "QA context",
        ],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(public)
            .env_clear()
            .envs(kaisa_sys::base_environment())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .output()
            .map_err(at("creating QA context repository"))?;
        if !output.status.success() {
            return Err(error("cannot create isolated QA context repository"));
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn developer_private_roots(repository: &Path) -> io::Result<Vec<PathBuf>> {
    let common = git_output(repository, &["rev-parse", "--git-common-dir"])?;
    let mut roots = vec![
        repository.canonicalize()?,
        repository.join(common.trim()).canonicalize()?,
    ];
    let worktrees = git_output(repository, &["worktree", "list", "--porcelain", "-z"])?;
    for path in worktrees
        .split('\0')
        .filter_map(|field| field.strip_prefix("worktree "))
    {
        roots.push(Path::new(path).canonicalize()?);
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
}

#[cfg(target_os = "macos")]
fn author_sandbox(
    state: &Path,
    repository: &Path,
    public: &Path,
    scratch: &Path,
    _: &ProcessSpec,
) -> io::Result<AgentSandbox> {
    let mut policy = kaisa_sys::SeatbeltPolicy {
        readable: vec![public.canonicalize()?, scratch.canonicalize()?],
        writable: vec![scratch.canonicalize()?],
        ..kaisa_sys::SeatbeltPolicy::default()
    };
    policy.protect_state(state)?;
    policy
        .unreadable
        .extend(developer_private_roots(repository)?);
    Ok(AgentSandbox::Seatbelt(policy))
}
#[cfg(target_os = "linux")]
fn author_sandbox(
    state: &Path,
    repository: &Path,
    public: &Path,
    scratch: &Path,
    process: &ProcessSpec,
) -> io::Result<AgentSandbox> {
    let mut readable = vec![
        public.canonicalize()?,
        scratch.canonicalize()?,
        process.program.canonicalize()?,
    ];
    for path in ["/usr", "/bin", "/lib", "/lib64", "/etc", "/dev"] {
        if Path::new(path).exists() {
            readable.push(Path::new(path).canonicalize()?);
        }
    }
    let roots = developer_private_roots(repository)?;
    let state = state.canonicalize()?;
    if readable.iter().any(|grant| {
        state.starts_with(grant)
            || roots
                .iter()
                .any(|root| root.starts_with(grant) || grant.starts_with(root))
    }) {
        return Err(io::Error::other(
            "QA author runtime read grant overlaps developer repository",
        ));
    }
    Ok(
        match kaisa_sys::read_write_ruleset(&[scratch.canonicalize()?], &readable)? {
            Some(ruleset) => AgentSandbox::Landlock(ruleset),
            None => AgentSandbox::Unconfined,
        },
    )
}
#[cfg(windows)]
fn author_sandbox(
    state: &Path,
    repository: &Path,
    public: &Path,
    scratch: &Path,
    process: &ProcessSpec,
) -> io::Result<AgentSandbox> {
    let program = process.program.canonicalize()?;
    let parent = program
        .parent()
        .ok_or_else(|| io::Error::other("QA author executable has no parent"))?;
    let state = state.canonicalize()?;
    if state.starts_with(parent)
        || parent.starts_with(&state)
        || developer_private_roots(repository)?
            .iter()
            .any(|root| root.starts_with(parent) || parent.starts_with(root))
    {
        return Err(io::Error::other(
            "QA author executable grant overlaps developer repository",
        ));
    }
    let mut container =
        crate::sandbox::ContainedAttempt::create(scratch, &format!("kaisaQa{}", fresh_suffix()))?;
    container.grant_for_agent(public, public, scratch, &[&process.program])?;
    Ok(AgentSandbox::AppContainer(container))
}
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn author_sandbox(
    _: &Path,
    _: &Path,
    _: &Path,
    _: &Path,
    _: &ProcessSpec,
) -> io::Result<AgentSandbox> {
    Ok(AgentSandbox::Unconfined)
}

/// Explicit inputs for checking a suite against one developer or revision attempt.
pub struct QaSuiteRequest<'a> {
    /// Repository containing the frozen commit.
    pub repository: &'a Path,
    /// Trusted pre-development revision for configuration and termination scanning.
    pub base: &'a str,
    /// Private state outside every worktree.
    pub state: &'a Path,
    /// Run receipt identifier.
    pub run_id: &'a str,
    /// Frozen candidate to execute.
    pub candidate: &'a Candidate,
    /// Previously sealed private suite.
    pub suite: &'a QaSuite,
    /// Trusted compiler or runner.
    pub toolchain: &'a QaToolchain,
    /// Actual developer/revision attempt; never reuse the first developer's isolation label.
    pub attempt: &'a GoalOutcome,
}

/// Runs a suite with one checkout/build and immutable private inputs outside its write grants.
/// Every scenario rechecks all input digests; only positive named-test evidence can pass.
/// # Errors
/// Returns an error before execution on storage tampering or infrastructure failure.
pub fn run_qa_suite(request: &QaSuiteRequest<'_>) -> Result<Vec<QaResult>, GoalError> {
    execute_suite(&SuiteExecution {
        repository: request.repository,
        base: request.base,
        state: request.state,
        run_id: request.run_id,
        candidate: request.candidate,
        suite: request.suite,
        toolchain: request.toolchain,
        isolation: qa_isolation(request.attempt),
        attempt_id: request.attempt.attempt.as_str(),
    })
}

/// Shared trusted executor; characterization has no developer receipt before development.
pub(crate) struct SuiteExecution<'a> {
    pub repository: &'a Path,
    pub base: &'a str,
    pub state: &'a Path,
    pub run_id: &'a str,
    pub candidate: &'a Candidate,
    pub suite: &'a QaSuite,
    pub toolchain: &'a QaToolchain,
    pub isolation: QaIsolation,
    pub attempt_id: &'a str,
}

pub(crate) fn execute_suite(request: &SuiteExecution<'_>) -> Result<Vec<QaResult>, GoalError> {
    let SuiteExecution {
        repository,
        state,
        run_id,
        candidate,
        suite,
        toolchain,
        ..
    } = request;
    let tests = suite
        .verified_tests()
        .map_err(at("verifying private QA digest"))?;
    let mut ledger =
        EventStore::open(&state.join("kaisa.db")).map_err(at("opening QA result ledger"))?;
    let mut feedback_sources = tests.clone();
    for event in ledger
        .read_from(Sequence::FIRST)
        .map_err(at("reading private QA suite receipts"))?
    {
        if event.recorded.event.kind != EventKind::QaSuiteRecorded {
            continue;
        }
        let payload: Value = serde_json::from_str(&event.recorded.event.payload)
            .map_err(at("reading QA suite receipt"))?;
        if payload["run_id"].as_str() != Some(run_id) {
            continue;
        }
        let kind = match payload["kind"].as_str() {
            Some("acceptance") => QaKind::Acceptance,
            Some("adversarial") => QaKind::Adversarial,
            _ => return Err(error("invalid private suite kind receipt")),
        };
        let digest = payload["manifest_digest"]
            .as_str()
            .ok_or_else(|| error("private suite receipt lacks digest"))?;
        let recorded = QaSuite::open(
            &state.join("qa").join(run_id).join(kind.code()),
            kind,
            digest,
        )
        .map_err(at("verifying feedback redaction suite"))?;
        feedback_sources.extend(
            recorded
                .verified_tests()
                .map_err(at("reading feedback redaction suite"))?,
        );
    }
    let findings = qa_influencing_findings(repository, request.base, candidate)?;
    let temporary = QaTemporary::create(state)?;
    let runtime = temporary.0.join("runtime");
    let inputs = temporary.0.join("inputs");
    std::fs::create_dir_all(&runtime).map_err(at("creating QA runtime"))?;
    std::fs::create_dir_all(&inputs).map_err(at("creating immutable QA inputs"))?;
    let checkout = CandidateCheckout::create(repository, candidate, &temporary.0.join("candidate"))
        .map_err(at("checking out private QA candidate"))?;
    let sources =
        CheckedSources::record(checkout.path()).map_err(at("pinning private QA source tree"))?;
    let manifest = checkout.path().join("Cargo.toml");
    if matches!(toolchain, QaToolchain::Rust { .. }) {
        crate::qa_interface::read_public(checkout.path(), Path::new("Cargo.toml"))
            .map_err(at("validating regular QA package manifest"))?;
    }
    let original_manifest = if matches!(toolchain, QaToolchain::Rust { .. }) {
        Some(std::fs::read(&manifest).map_err(at("reading original QA manifest"))?)
    } else {
        None
    };
    let mut installed = Vec::new();
    let mut expected = Vec::new();
    let mut names = Vec::new();
    for test in &tests {
        expected.push(expected_test_names(test, toolchain)?);
        let name = format!("kaisa_qa_{}", fresh_suffix());
        let path = match toolchain {
            QaToolchain::Rust { .. } => {
                if !test.file.ends_with(".rs") {
                    return Err(error("Rust private QA requires .rs files"));
                }
                inputs.join(format!("{name}.rs"))
            }
            QaToolchain::Node { .. } => {
                let extension = Path::new(&test.file)
                    .extension()
                    .and_then(|s| s.to_str())
                    .filter(|s| ["js", "mjs", "cjs"].contains(s))
                    .ok_or_else(|| error("Node private QA requires JS files"))?;
                let parent = inputs.join("node");
                let directory = parent.join("tests");
                if !parent.exists() {
                    prepare_node_inputs(checkout.path(), &parent)?;
                }
                std::fs::create_dir_all(&directory).map_err(at("creating immutable Node tests"))?;
                directory.join(format!("{name}.{extension}"))
            }
        };
        std::fs::write(&path, &test.contents).map_err(at("installing immutable QA source"))?;
        let mut permissions = std::fs::metadata(&path)
            .map_err(at("reading QA input permissions"))?
            .permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).map_err(at("sealing QA input"))?;
        if matches!(toolchain, QaToolchain::Rust { .. }) {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&manifest)
                .map_err(at("opening QA manifest"))?;
            writeln!(
                file,
                "\n[[test]]\nname = {}\npath = {}",
                json!(name),
                json!(path.to_string_lossy())
            )
            .map_err(at("adding immutable QA target"))?;
        }
        installed.push(path);
        names.push(name);
    }
    let generated_manifest = if original_manifest.is_some() {
        std::fs::read(&manifest).ok()
    } else {
        None
    };
    if !inputs_match(&installed, &tests) {
        return Err(error(
            "installed private QA inputs changed before compilation",
        ));
    }
    let readonly = vec![inputs.clone(), manifest.clone()];
    let mut build = None;
    if findings.is_empty()
        && let QaToolchain::Rust { cargo } = toolchain
    {
        let mut args = vec![
            "test".into(),
            "--offline".into(),
            "--no-run".into(),
            "--message-format=json".into(),
            "--manifest-path".into(),
            manifest.to_string_lossy().into_owned(),
        ];
        args.extend(cargo_configuration_overrides(cargo, &runtime)?);
        for name in &names {
            args.extend(["--test".into(), name.clone()]);
        }
        let process = ProcessSpec {
            program: cargo.clone(),
            args,
            working_directory: runtime.clone(),
            env: Vec::new(),
        };
        build = Some(qa_command(
            state,
            checkout.path(),
            &runtime,
            &readonly,
            &process,
        )?);
    }
    let artifacts: Vec<(PathBuf, blake3::Hash)> = build
        .as_ref()
        .into_iter()
        .flat_map(|(run, _)| {
            String::from_utf8_lossy(&run.output)
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|v| v["reason"] == "compiler-artifact")
                .filter_map(|v| v["executable"].as_str().map(PathBuf::from))
                .collect::<Vec<_>>()
        })
        .map(|path| {
            let path = path
                .canonicalize()
                .map_err(at("resolving compiled QA artifact"))?;
            if !path.starts_with(&runtime) {
                return Err(error(
                    "compiled QA artifact is outside the Kaisa build directory",
                ));
            }
            let bytes = std::fs::read(&path).map_err(at("pinning compiled QA artifact"))?;
            Ok((path, blake3::hash(&bytes)))
        })
        .collect::<Result<Vec<_>, GoalError>>()?;
    let artifacts_match = || {
        artifacts.iter().all(|(path, digest)| {
            std::fs::read(path).is_ok_and(|bytes| blake3::hash(&bytes) == *digest)
        })
    };
    let execution_scratch = temporary.0.join("execution");
    std::fs::create_dir(&execution_scratch).map_err(at("creating QA execution scratch"))?;
    let mut execution_readonly = readonly.clone();
    execution_readonly.push(runtime.clone());
    let mut results = Vec::new();
    let mut tainted = false;
    for (index, test) in tests.iter().enumerate() {
        suite
            .verified_tests()
            .map_err(at("rechecking private QA digest before execution"))?;
        let before = !tainted && inputs_match(&installed, &tests) && artifacts_match();
        let mut command = None;
        if findings.is_empty() && before {
            match toolchain {
                QaToolchain::Rust { .. } => {
                    if let Some((built, _)) = &build
                        && !built.truncated
                        && built.exit == Some(ProcessExit::Code(0))
                    {
                        let executable = String::from_utf8_lossy(&built.output)
                            .lines()
                            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                            .find(|v| {
                                v["reason"] == "compiler-artifact"
                                    && v["target"]["name"] == names[index]
                                    && v["executable"].is_string()
                            })
                            .and_then(|v| v["executable"].as_str().map(PathBuf::from));
                        if let Some(program) = executable {
                            command = Some(ProcessSpec {
                                program,
                                args: vec![
                                    "--nocapture".into(),
                                    "--format=pretty".into(),
                                    "--test-threads=1".into(),
                                ],
                                working_directory: checkout.path().to_owned(),
                                env: Vec::new(),
                            });
                        }
                    }
                }
                QaToolchain::Node { node } => {
                    command = Some(ProcessSpec {
                        program: node.clone(),
                        args: vec![
                            "--test".into(),
                            "--test-reporter=tap".into(),
                            installed[index].to_string_lossy().into_owned(),
                        ],
                        working_directory: checkout.path().to_owned(),
                        env: Vec::new(),
                    })
                }
            }
        }
        let executed = command.is_some();
        let (run, confined) = if let Some(process) = command {
            qa_command(
                state,
                checkout.path(),
                &execution_scratch,
                &execution_readonly,
                &process,
            )?
        } else if let Some(build) = &build {
            build.clone()
        } else {
            (
                CapturedRun {
                    exit: None,
                    output: Vec::new(),
                    truncated: false,
                },
                false,
            )
        };
        let input_bytes_unchanged = before && inputs_match(&installed, &tests) && artifacts_match();
        // The manifest is harness-owned. Verify its generated bytes, restore the frozen bytes
        // only for Git's tracked-source comparison, then put the identical harness bytes back.
        let manifest_unchanged =
            original_manifest.is_none() || std::fs::read(&manifest).ok() == generated_manifest;
        if let Some(bytes) = &original_manifest {
            std::fs::write(&manifest, bytes)
                .map_err(at("restoring frozen manifest for comparison"))?;
        }
        let unchanged = manifest_unchanged
            && sources
                .matches(checkout.path(), &candidate.tree)
                .map_err(at("checking private QA candidate integrity"))?;
        if let Some(bytes) = &generated_manifest {
            std::fs::write(&manifest, bytes).map_err(at("restoring QA manifest"))?;
        }
        let write_denied = confined && qa_write_deny_available();
        let private_test_unchanged = write_denied && input_bytes_unchanged;
        let positive_evidence =
            executed && positive_test_evidence(&run, &expected[index], toolchain);
        let passed = findings.is_empty()
            && !run.truncated
            && unchanged
            && input_bytes_unchanged
            && positive_evidence
            && run.exit == Some(ProcessExit::Code(0));
        let failures = if passed {
            Vec::new()
        } else {
            observable_diagnostics(
                test,
                &feedback_sources,
                &run,
                unchanged && input_bytes_unchanged,
                positive_evidence,
                &findings,
            )
        };
        let result = QaResult {
            kind: suite.kind(),
            test_id: test.test_id.clone(),
            criterion_ids: test.criterion_ids.clone(),
            passed,
            output_digest: blake3::hash(&run.output).to_hex().to_string(),
            candidate_tree: candidate.tree.clone(),
            confined,
            source_unchanged: unchanged,
            private_test_unchanged,
            expected_tests: expected[index].clone(),
            positive_evidence,
            qa_evidence: QA_EVIDENCE,
            private_test_integrity: if write_denied {
                "write-denied"
            } else {
                "reduced(write-deny unavailable; digests only)"
            },
            blocking_findings: findings.clone(),
            qa_isolation: request.isolation.clone(),
            failures,
        };
        record(
            &mut ledger,
            EventKind::QaResult,
            &test.test_id,
            json!({"run_id":run_id,"test_id":result.test_id,"kind":result.kind.code(),"criterion_ids":result.criterion_ids,
                "passed":result.passed,"output_digest":result.output_digest,"candidate_tree":result.candidate_tree,"confined":result.confined,
                "source_unchanged":result.source_unchanged,"private_test_unchanged":result.private_test_unchanged,
                "private_test_integrity":result.private_test_integrity,"qa_evidence":result.qa_evidence,"expected_tests":result.expected_tests,
                "positive_evidence":result.positive_evidence,"blocking_findings":result.blocking_findings,
                "attempt":request.attempt_id,"qa_isolation":result.qa_isolation.label()}),
        )?;
        results.push(result);
        // A mutated shared build invalidates the remainder; never reuse tainted artifacts.
        if !unchanged || !input_bytes_unchanged {
            build = None;
            tainted = true;
        }
    }
    Ok(results)
}

fn cargo_configuration_overrides(cargo: &Path, directory: &Path) -> Result<Vec<String>, GoalError> {
    let output = std::process::Command::new(cargo)
        .arg("-vV")
        .current_dir(directory)
        .env_clear()
        .envs(kaisa_sys::base_environment())
        .output()
        .map_err(at("identifying trusted Cargo host"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let host = text
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|host| {
            !host.is_empty()
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
        .ok_or_else(|| error("trusted Cargo did not identify its host target"))?;
    // Cargo has no null runner value. This explicit fail-closed sentinel disables both
    // cfg and exact-host runners: --no-run is mandatory and Kaisa executes binaries itself.
    let runner = format!(
        "[{}, \"--kaisa-no-runner\"]",
        json!(cargo.to_string_lossy())
    );
    Ok(vec![
        "--config".into(),
        format!("target.'cfg(all())'.runner={runner}"),
        "--config".into(),
        format!("target.{host}.runner={runner}"),
        "--config".into(),
        "build.rustc-wrapper=\"\"".into(),
        "--config".into(),
        "build.rustc-workspace-wrapper=\"\"".into(),
    ])
}

fn qa_write_deny_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        return kaisa_sys::landlock_abi().is_some_and(|abi| abi >= 3);
    }
    #[cfg(not(target_os = "linux"))]
    {
        cfg!(target_os = "macos")
    }
}

fn inputs_match(paths: &[PathBuf], tests: &[QaTestFile]) -> bool {
    paths.iter().zip(tests).all(|(path, test)| {
        std::fs::read(path)
            .is_ok_and(|bytes| blake3::hash(&bytes) == blake3::hash(test.contents.as_bytes()))
    })
}

fn prepare_node_inputs(checkout: &Path, parent: &Path) -> Result<(), GoalError> {
    std::fs::create_dir(parent).map_err(at("creating Node public module links"))?;
    for entry in std::fs::read_dir(checkout).map_err(at("listing Node public modules"))? {
        let entry = entry.map_err(at("reading Node public entry"))?;
        if entry.file_name() == ".git" || entry.file_name() == "tests" {
            continue;
        }
        let destination = parent.join(entry.file_name());
        #[cfg(unix)]
        std::os::unix::fs::symlink(entry.path(), destination)
            .map_err(at("linking public Node module"))?;
        #[cfg(windows)]
        if entry
            .file_type()
            .map_err(at("Node entry metadata"))?
            .is_dir()
        {
            std::os::windows::fs::symlink_dir(entry.path(), destination)
                .map_err(at("linking public Node directory"))?;
        } else {
            std::os::windows::fs::symlink_file(entry.path(), destination)
                .map_err(at("linking public Node module"))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn readonly_cargo_cache(cargo_home: &Path) -> Result<Vec<PathBuf>, GoalError> {
    let mut paths = Vec::new();
    // Reuse only trusted offline dependency sources, never user/candidate Cargo config,
    // credentials or executables. Symlink targets stay outside all writable grants.
    {
        let cache = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
        if let Some(cache) = cache {
            for directory in ["registry", "git"] {
                let source = cache.join(directory);
                if source.is_dir() {
                    let source = source
                        .canonicalize()
                        .map_err(at("resolving trusted Cargo cache"))?;
                    let destination = cargo_home.join(directory);
                    if !destination.exists() {
                        std::os::unix::fs::symlink(&source, &destination)
                            .map_err(at("linking read-only Cargo dependency cache"))?;
                    }
                    paths.push(source);
                }
            }
        }
    }
    Ok(paths)
}
#[cfg(not(unix))]
fn readonly_cargo_cache(_: &Path) -> Result<Vec<PathBuf>, GoalError> {
    Ok(Vec::new())
}

fn qa_command(
    state: &Path,
    checkout: &Path,
    runtime: &Path,
    readonly: &[PathBuf],
    process: &ProcessSpec,
) -> Result<(CapturedRun, bool), GoalError> {
    let cargo_home = runtime.join("cargo-home");
    std::fs::create_dir_all(&cargo_home).map_err(at("preparing Kaisa Cargo home"))?;
    let mut protected = readonly.to_vec();
    protected.extend(readonly_cargo_cache(&cargo_home)?);
    let mut sandbox = check_sandbox_readonly(state, checkout, runtime, &protected)
        .map_err(at("confining immutable QA inputs"))?;
    #[cfg(target_os = "macos")]
    if let crate::verify::CheckSandbox::Seatbelt(policy) = &mut sandbox {
        policy.readable.extend(protected.iter().cloned());
        policy
            .protect_state(state)
            .map_err(at("granting read-only QA inputs"))?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = &mut sandbox;
    let mut process = process.clone();
    for (key, value) in [
        ("CARGO_HOME", cargo_home.to_string_lossy().into_owned()),
        (
            "CARGO_TARGET_DIR",
            runtime.join("target").to_string_lossy().into_owned(),
        ),
        ("RUSTC_WRAPPER", String::new()),
        ("RUSTC_WORKSPACE_WRAPPER", String::new()),
        ("CARGO_BUILD_RUSTC_WRAPPER", String::new()),
        ("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", String::new()),
        ("CARGO_BUILD_RUSTFLAGS", String::new()),
        ("CARGO_ENCODED_RUSTFLAGS", String::new()),
        ("NODE_OPTIONS", String::new()),
        (
            "KAISA_STATE_DIR",
            runtime.join("test-state").to_string_lossy().into_owned(),
        ),
        ("TMPDIR", runtime.to_string_lossy().into_owned()),
        ("TMP", runtime.to_string_lossy().into_owned()),
        ("TEMP", runtime.to_string_lossy().into_owned()),
    ] {
        process.env.push((key.to_owned(), value));
    }
    let run = run_captured(
        &process,
        &sandbox.confinement(),
        &[],
        QA_DEADLINE,
        QA_OUTPUT_CAP,
    )
    .map_err(at("running immutable private QA"))?;
    Ok((run, sandbox.is_confined()))
}

fn candidate_command(
    state: &Path,
    checkout: &Path,
    temporary: &Path,
    sources: &CheckedSources,
    tree: &str,
    process: &ProcessSpec,
) -> Result<(CapturedRun, bool, bool), GoalError> {
    let (run, confined) = qa_command(state, checkout, temporary, &[], process)?;
    let unchanged = sources
        .matches(checkout, tree)
        .map_err(at("checking private QA candidate integrity"))?;
    Ok((run, confined, unchanged))
}

fn add_cli_help(
    request: &QaRequest<'_>,
    checkout: &Path,
    sources: &CheckedSources,
    temporary: &Path,
    interface: &mut PublicInterface,
) -> Result<(), GoalError> {
    let commands: Vec<(String, ProcessSpec)> = match request.toolchain {
        QaToolchain::Node { node } => node_bins(checkout)
            .map_err(at("extracting Node CLI bins"))?
            .into_iter()
            .map(|bin| {
                let spec = ProcessSpec {
                    program: node.clone(),
                    args: vec![bin.clone(), "--help".into()],
                    working_directory: checkout.to_owned(),
                    env: Vec::new(),
                };
                (bin, spec)
            })
            .collect(),
        QaToolchain::Rust { cargo } => rust_bins(checkout)
            .map_err(at("extracting Rust CLI bins"))?
            .into_iter()
            .map(|(name, _)| {
                let command = format!("{name} --help");
                (
                    command,
                    ProcessSpec {
                        program: cargo.clone(),
                        args: vec![
                            "build".into(),
                            "--offline".into(),
                            "--message-format=json".into(),
                            "--manifest-path".into(),
                            checkout.join("Cargo.toml").to_string_lossy().into_owned(),
                            "--bin".into(),
                            name,
                        ],
                        working_directory: checkout.to_owned(),
                        env: Vec::new(),
                    },
                )
            })
            .collect(),
    };
    for (name, mut spec) in commands {
        if let QaToolchain::Rust { cargo } = request.toolchain {
            spec.working_directory = temporary.to_owned();
            spec.args
                .extend(cargo_configuration_overrides(cargo, temporary)?);
        }
        let (mut run, confined, mut unchanged) = candidate_command(
            request.state_directory,
            checkout,
            temporary,
            sources,
            &request.candidate.tree,
            &spec,
        )?;
        if matches!(request.toolchain, QaToolchain::Rust { .. })
            && run.exit == Some(ProcessExit::Code(0))
            && !run.truncated
        {
            let executable = String::from_utf8_lossy(&run.output)
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find(|v| v["reason"] == "compiler-artifact" && v["executable"].is_string())
                .and_then(|v| v["executable"].as_str().map(PathBuf::from));
            if let Some(program) = executable {
                let checked = candidate_command(
                    request.state_directory,
                    checkout,
                    temporary,
                    sources,
                    &request.candidate.tree,
                    &ProcessSpec {
                        program,
                        args: vec!["--help".into()],
                        working_directory: checkout.to_owned(),
                        env: Vec::new(),
                    },
                )?;
                run = checked.0;
                unchanged &= checked.2;
            }
        }
        if !unchanged {
            return Err(error("CLI --help modified the frozen candidate sources"));
        }
        // A broken CLI remains observable API evidence; never imply it produced valid help.
        interface.help.push((
            name,
            format!(
                "confined={confined}; exit={:?}\n{}",
                run.exit,
                review_output_tail(&run)
            ),
        ));
    }
    Ok(())
}

fn git_output(repository: &Path, args: &[&str]) -> io::Result<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repository)
        .env_clear()
        .envs(kaisa_sys::base_environment())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("QA Git query failed"));
    }
    String::from_utf8(output.stdout).map_err(io::Error::other)
}

fn qa_influencing_findings(
    repository: &Path,
    base: &str,
    candidate: &Candidate,
) -> Result<Vec<String>, GoalError> {
    let paths = kaisa_workspace::changed_paths(repository, base, candidate)
        .map_err(at("scanning QA-influencing paths"))?;
    let mut findings = BTreeSet::new();
    for path in paths {
        let file = path.rsplit('/').next().unwrap_or(&path);
        let configuration = (path.split('/').any(|part| part == ".cargo")
            && file.starts_with("config"))
            || file == "build.rs"
            || file.starts_with("rust-toolchain")
            || file == ".npmrc"
            || [
                "jest.config",
                "vitest.config",
                "mocha",
                ".mocharc",
                "node.options",
            ]
            .iter()
            .any(|prefix| file.starts_with(prefix));
        let package_configuration = if file == "package.json" {
            let config = |revision: &str| -> Option<Value> {
                git_output(repository, &["show", &format!("{revision}:{path}")])
                    .ok()
                    .and_then(|text| serde_json::from_str(&text).ok())
            };
            let before = config(base);
            let after = config(&candidate.commit);
            [
                "scripts",
                "jest",
                "vitest",
                "mocha",
                "ava",
                "nyc",
                "nodeOptions",
                "node-options",
            ]
            .iter()
            .any(|key| {
                before.as_ref().and_then(|v| v.get(key)) != after.as_ref().and_then(|v| v.get(key))
            })
        } else {
            false
        };
        if configuration || package_configuration {
            findings.insert(format!("QA-influencing configuration: {path}; reviewer justification with evidence required"));
        }
        if !matches!(
            Path::new(&path).extension().and_then(|s| s.to_str()),
            Some("rs" | "js" | "mjs" | "cjs" | "ts")
        ) || path.split('/').any(|part| part == "tests")
            || file.contains(".test.")
            || file.contains(".spec.")
        {
            continue;
        }
        let source = |revision: &str| {
            git_output(repository, &["show", &format!("{revision}:{path}")]).unwrap_or_default()
        };
        let before = source(base);
        let after = source(&candidate.commit);
        let javascript = !path.ends_with(".rs");
        let before_code = crate::qa_interface::code_tokens(&before, javascript).join("");
        let after_code = crate::qa_interface::code_tokens(&after, javascript).join("");
        if [
            "std::process::exit(",
            "process::abort(",
            "process.exit(",
            "process.kill(process.pid",
        ]
        .iter()
        .any(|call| after_code.matches(call).count() > before_code.matches(call).count())
        {
            findings.insert(format!("Added process-termination call in {path}; reviewer justification with evidence required"));
        }
        let added = after;
        if added.contains("NODE_OPTIONS") {
            findings.insert(format!("QA-influencing configuration: {path} changes node options; reviewer justification with evidence required"));
        }
    }
    Ok(findings.into_iter().collect())
}

/// Only flat Rust #[test] functions and Node test('static name', ...) are supported. This
/// deliberately refuses dynamic/ignored/nested declarations rather than guessing test counts.
fn expected_test_names(
    test: &QaTestFile,
    toolchain: &QaToolchain,
) -> Result<Vec<String>, GoalError> {
    let names = match toolchain {
        QaToolchain::Rust { .. } => {
            let tokens = crate::qa_interface::code_tokens(&test.contents, false);
            let mut names = Vec::new();
            for window in tokens.windows(7) {
                if window[..5] == ["#", "[", "test", "]", "fn"] && window[6] == "(" {
                    names.push(window[5].clone());
                }
            }
            names
        }
        QaToolchain::Node { .. } => node_test_names(&test.contents)?,
    };
    if names.is_empty() || names.iter().collect::<BTreeSet<_>>().len() != names.len() {
        return Err(error(
            "private QA requires unique, statically named expected tests",
        ));
    }
    Ok(names)
}

fn node_test_names(source: &str) -> Result<Vec<String>, GoalError> {
    // A tiny JS lexical scan preserves only static string operands, skipping comments and
    // consuming templates whole. Other API extraction uses the opaque-literal lexer.
    let bytes = source.as_bytes();
    let mut at = 0;
    let mut tokens = Vec::new();
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if bytes[at..].starts_with(b"//") {
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        if bytes[at..].starts_with(b"/*") {
            at += 2;
            while at + 1 < bytes.len() && !bytes[at..].starts_with(b"*/") {
                at += 1;
            }
            at = (at + 2).min(bytes.len());
            continue;
        }
        if matches!(bytes[at], b'\'' | b'"' | b'`') {
            let quote = bytes[at];
            at += 1;
            let start = at;
            let mut escaped = false;
            while at < bytes.len() && bytes[at] != quote {
                if bytes[at] == b'\\' {
                    escaped = true;
                    at += 1;
                }
                at += 1;
            }
            let value = source.get(start..at).unwrap_or_default();
            tokens.push(if escaped || quote == b'`' {
                "<dynamic>".into()
            } else {
                format!("string:{value}")
            });
            at = (at + 1).min(bytes.len());
            continue;
        }
        let start = at;
        if bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' || bytes[at] == b'$' {
            while at < bytes.len()
                && (bytes[at].is_ascii_alphanumeric() || b"_$".contains(&bytes[at]))
            {
                at += 1;
            }
        } else {
            at += 1;
        }
        tokens.push(String::from_utf8_lossy(&bytes[start..at]).into_owned());
    }
    let mut names = Vec::new();
    for window in tokens.windows(4) {
        if window[0] == "test" && window[1] == "(" && window[3] == "," {
            let name = window[2]
                .strip_prefix("string:")
                .filter(|name| !name.is_empty())
                .ok_or_else(|| error("Node QA requires static test names"))?;
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

fn positive_test_evidence(run: &CapturedRun, expected: &[String], toolchain: &QaToolchain) -> bool {
    if expected.is_empty() || run.truncated {
        return false;
    }
    let output = String::from_utf8_lossy(&run.output);
    let mut observed = Vec::new();
    match toolchain {
        QaToolchain::Rust { .. } => {
            for line in output.lines() {
                if let Some(name) = line
                    .strip_prefix("test ")
                    .and_then(|line| line.strip_suffix(" ... ok"))
                {
                    observed.push(name.to_owned());
                }
            }
            let summary = format!(
                "test result: ok. {} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;",
                expected.len()
            );
            if output
                .lines()
                .filter(|line| line.starts_with("test result: "))
                .count()
                != 1
                || output
                    .lines()
                    .filter(|line| line.starts_with(&summary))
                    .count()
                    != 1
            {
                return false;
            }
        }
        QaToolchain::Node { .. } => {
            for line in output.lines() {
                if let Some(rest) = line
                    .strip_prefix("ok ")
                    .and_then(|line| line.split_once(" - ").map(|(_, name)| name))
                {
                    observed.push(rest.to_owned());
                }
            }
            for required in [
                format!("1..{}", expected.len()),
                format!("# tests {}", expected.len()),
                format!("# pass {}", expected.len()),
                "# fail 0".into(),
                "# cancelled 0".into(),
                "# skipped 0".into(),
                "# todo 0".into(),
            ] {
                if output.lines().filter(|line| *line == required).count() != 1 {
                    return false;
                }
            }
        }
    }
    observed.sort();
    let mut expected = expected.to_vec();
    expected.sort();
    observed == expected
}

fn normalized(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}
fn redact_private(text: &str, suite: &[QaTestFile]) -> String {
    const MIN_SUBSTRING: usize = 16;
    const SHINGLE_WORDS: usize = 8;
    let text: String = text.chars().take(QA_FEEDBACK_FIELD_CAP).collect();
    let compact = normalized(&text);
    let tokens = words(&text);
    let overlaps = suite.iter().any(|test| {
        let source = normalized(&test.contents);
        (!compact.is_empty() && source.contains(&compact))
            || compact
                .chars()
                .collect::<Vec<_>>()
                .windows(MIN_SUBSTRING)
                .any(|window| source.contains(&window.iter().collect::<String>()))
            || test
                .contents
                .lines()
                .map(normalized)
                .any(|line| line.len() >= MIN_SUBSTRING && compact.contains(&line))
            || words(&test.contents)
                .iter()
                .any(|word| word.len() >= MIN_SUBSTRING && compact.contains(word))
            || tokens.windows(SHINGLE_WORDS).any(|shingle| {
                words(&test.contents)
                    .windows(SHINGLE_WORDS)
                    .any(|private| private == shingle)
            })
    });
    if overlaps {
        "[private source redacted]".into()
    } else {
        text
    }
}

fn observable_diagnostics(
    test: &QaTestFile,
    suite: &[QaTestFile],
    run: &CapturedRun,
    unchanged: bool,
    positive: bool,
    findings: &[String],
) -> Vec<ObservableFailure> {
    // No self-declared observation data is consumed. Both assertion messages and ids are
    // scanned before forwarding, because the candidate shares the harness output channel.
    let output = review_output_tail(run);
    let mut excerpt = None;
    let mut panic_next = false;
    for line in output.lines() {
        if line.contains("kaisa-observation:") {
            continue;
        }
        if panic_next {
            excerpt = Some(line.trim().to_owned());
            break;
        }
        if line.starts_with("thread '") && line.contains("panicked at") {
            panic_next = true;
        }
        if let Some(message) = line.trim().strip_prefix("error: ") {
            excerpt = Some(message.to_owned());
            break;
        }
    }
    let message = if !findings.is_empty() {
        "blocked: QA-influencing configuration or termination call requires reviewer evidence"
            .into()
    } else if !unchanged {
        "candidate or private test inputs changed".into()
    } else if !positive && run.exit == Some(ProcessExit::Code(0)) {
        "no positive evidence for every expected test".into()
    } else {
        excerpt.unwrap_or_else(|| {
            format!(
                "test process {:?}; truncated={}; no positive evidence",
                run.exit, run.truncated
            )
        })
    };
    test.criterion_ids
        .iter()
        .map(|id| ObservableFailure {
            criterion_id: redact_private(id, suite),
            scenario: redact_private(&test.test_id, suite),
            passed: false,
            message: redact_private(&message, suite),
        })
        .collect()
}

/// Bounded, already source-redacted Kaisa feedback; raw output is never forwarded.
#[must_use]
pub fn revision_feedback(results: &[QaResult]) -> Vec<ObservableFailure> {
    let mut remaining = QA_FEEDBACK_TOTAL_CAP.saturating_sub(2);
    results.iter().filter(|result| !result.passed).flat_map(|result| result.failures.iter()).take_while(|failure| {
        let size = json!({"criterion_id":failure.criterion_id,"scenario":failure.scenario,"passed":failure.passed,"message":failure.message}).to_string().len() + 1;
        if size > remaining { false } else { remaining -= size; true }
    }).cloned().collect()
}

struct QaTemporary(PathBuf);
impl QaTemporary {
    fn create(state: &Path) -> Result<Self, GoalError> {
        let path = state.join("checks").join(format!("qa_{}", fresh_suffix()));
        kaisa_sys::ensure_private_directory(&path).map_err(at("preparing private QA scratch"))?;
        Ok(Self(
            path.canonicalize()
                .map_err(at("resolving private QA scratch"))?,
        ))
    }
}
impl Drop for QaTemporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use kaisa_domain::{
        AttemptId, ContainmentCapability, ContentDigest, EventKind, Execution, ExitStatus,
        ReducedModeBasis, Sequence,
    };
    use kaisa_ledger::EventStore;
    use kaisa_sys::{ProcessExit, ProcessSpec};
    use kaisa_workspace::{Candidate, QaKind, QaTestFile, freeze_candidate, record_qa_suite};
    use serde_json::{Value, json};

    use super::{
        QaIsolation, QaRequest, QaToolchain, parse_qa_answer, qa_isolation, revision_feedback,
        run_private_qa, run_qa_suite,
    };
    use crate::goal::{GoalOutcome, record, record_execution};

    #[test]
    fn a_staged_review_bundle_in_private_state_never_enters_blind_qa_context() {
        let root =
            std::env::temp_dir().join(format!("kaisa-review-qa-boundary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repository = root.join("candidate");
        let state = root.join("state");
        std::fs::create_dir_all(repository.join("src")).expect("candidate fixture");
        std::fs::create_dir_all(&state).expect("separate private state");
        std::fs::write(repository.join("src/lib.rs"), "pub fn rotate() {}\n")
            .expect("public signature");
        let input = crate::staged_review::StagedReviewInput {
            developer_final_answer: DEVELOPER_MARKER.into(),
            revision_history: DEVELOPER_MARKER.into(),
            human_interventions: DEVELOPER_MARKER.into(),
            ..Default::default()
        };
        let bundle = crate::staged_review::context_bundle(&input).to_string();
        std::fs::write(state.join("review-only.json"), &bundle)
            .expect("review-only persistence outside candidate");
        let interface = crate::qa_interface::extract_public_interface(&repository)
            .expect("candidate public interface");
        let pack = super::qa_context(
            &json!({"criteria":[{"id":CRITERION}]}),
            &interface,
            QaKind::Adversarial,
            &QaToolchain::Rust {
                cargo: PathBuf::from("cargo"),
            },
        );
        assert!(
            bundle.contains(DEVELOPER_MARKER),
            "the review fixture really contains developer reasoning"
        );
        assert!(
            !pack.contains(DEVELOPER_MARKER) && !pack.contains("stage3_decisions_and_history"),
            "Blind QA cannot discover review bundles outside its public-interface checkout"
        );
        std::fs::remove_dir_all(root).expect("remove boundary fixture");
    }

    const CRITERION: &str = "AC-1";
    const PRIVATE_MARKER: &str = "PRIVATE_TEST_SOURCE_SHOULD_NOT_REACH_DEVELOPER";
    const DEVELOPER_MARKER: &str = "DEVELOPER_REASONING_AND_TEST_RESULTS_MUST_NOT_REACH_QA";
    const PASSING_TEST: &str = "#[test] fn a_nonempty_input_is_accepted() { assert!(qa_fixture::accept(\"hello\"), \"nonempty public input must be accepted\"); }";
    const FAILING_TEST: &str = r##"#[test] fn an_empty_input_is_rejected() {
        let actual = qa_fixture::accept("");
        if actual { eprintln!("kaisa-observation:{}", r#"{"scenario":"empty input is rejected","input":"empty string","expected":"rejection","actual":"accepted"}"#); }
        assert!(!actual, "PRIVATE_TEST_SOURCE_SHOULD_NOT_REACH_DEVELOPER");
    }"##;

    struct Fixture {
        root: PathBuf,
        repository: PathBuf,
        state: PathBuf,
        candidate: Candidate,
        base: String,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "kaisa-independent-qa-{}",
                crate::ids::fresh_suffix()
            ));
            let repository = root.join("developer");
            let state = root.join("state");
            std::fs::create_dir_all(repository.join("src")).expect("candidate source fixture");
            std::fs::create_dir_all(repository.join("tests")).expect("developer tests fixture");
            std::fs::create_dir_all(&state).expect("isolated KAISA_STATE_DIR fixture");
            std::fs::write(
                repository.join("Cargo.toml"),
                "[package]\nname = \"qa_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            )
            .expect("fixture manifest");
            std::fs::write(repository.join("src/lib.rs"), format!("pub fn accept(input: &str) -> bool {{ let reasoning = \"{DEVELOPER_MARKER}\"; let _ = (input, reasoning); true }}\n#[cfg(test)] mod tests {{ const PRIVATE: &str = \"{DEVELOPER_MARKER}\"; }}")).expect("buggy public API");
            std::fs::write(
                repository.join("tests/developer.rs"),
                format!("// {DEVELOPER_MARKER}\n"),
            )
            .expect("developer test contents");
            std::fs::write(
                repository.join("README.md"),
                "accept(input) accepts nonempty input and rejects empty input.",
            )
            .expect("public readme");
            git(&repository, &["init", "-q"]);
            git(
                &repository,
                &[
                    "add",
                    "Cargo.toml",
                    "src/lib.rs",
                    "README.md",
                    "tests/developer.rs",
                ],
            );
            git(
                &repository,
                &[
                    "-c",
                    "user.name=QA fixture",
                    "-c",
                    "user.email=qa@example.invalid",
                    "commit",
                    "-qm",
                    DEVELOPER_MARKER,
                ],
            );
            let candidate = freeze_candidate(&repository, "HEAD").expect("frozen candidate");
            Self {
                root,
                repository,
                state,
                base: candidate.commit.clone(),
                candidate,
            }
        }
        fn toolchain(&self) -> QaToolchain {
            QaToolchain::Rust {
                cargo: program("cargo"),
            }
        }
        fn spec(&self) -> Value {
            json!({"title":"Reject empty input","criteria":[{"id":CRITERION,"given":"empty input","when":"accept is called","then":"rejection"}]})
        }
        fn developer(&self, ended: bool) -> GoalOutcome {
            let attempt = AttemptId::try_new("att_developer").expect("fixture attempt id");
            let mut ledger =
                EventStore::open(&self.state.join("kaisa.db")).expect("fixture ledger");
            record(
                &mut ledger,
                EventKind::AttemptAdmitted,
                attempt.as_str(),
                json!({"task":"task_developer"}),
            )
            .expect("developer admitted");
            record_execution(&mut ledger, &attempt, Execution::Running).expect("developer running");
            let pack =
                ContentDigest::try_new(blake3::hash(DEVELOPER_MARKER.as_bytes()).to_hex().as_str())
                    .expect("developer pack digest");
            ledger
                .record_context_pack(&attempt, &pack, DEVELOPER_MARKER.as_bytes())
                .expect("developer reasoning pack");
            if ended {
                record_execution(
                    &mut ledger,
                    &attempt,
                    Execution::Exited(ExitStatus::Code(0)),
                )
                .expect("developer ended");
            }
            GoalOutcome {
                attempt,
                exit: Some(ProcessExit::Code(0)),
                truncated: false,
                permission_denials: Vec::new(),
                branch: "developer".into(),
                commit: Some(self.candidate.commit.clone()),
                kept_back: Vec::new(),
                context_pack: pack.as_str().to_owned(),
                confined: false,
                missing: vec![ContainmentCapability::DescendantContainment],
                admitted_on: ReducedModeBasis::None,
                observed: false,
            }
        }
        fn fake(&self, test: QaTestFile) -> ProcessSpec {
            let binary = std::env::current_exe()
                .expect("unit test executable")
                .parent()
                .expect("deps")
                .parent()
                .expect("debug")
                .join(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
            assert!(
                binary.is_file(),
                "build kaisa-fake-agent before running QA agent tests"
            );
            ProcessSpec {
                program: binary,
                args: vec!["--fake-qa".into(), "-".into()],
                working_directory: self.repository.clone(),
                env: vec![
                    (
                        "KAISA_STATE_DIR".into(),
                        self.state.to_string_lossy().into_owned(),
                    ),
                    ("KAISA_FAKE_QA_JSON".into(), proposal(&test).to_string()),
                ],
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn git(root: &Path, args: &[&str]) {
        let result = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("fixture Git");
        assert!(
            result.status.success(),
            "fixture Git {args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fn program(name: &str) -> PathBuf {
        let filename = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        std::env::split_paths(&std::env::var_os("PATH").expect("toolchain PATH"))
            .map(|directory| directory.join(&filename))
            .find(|path| path.is_file())
            .expect("trusted installed toolchain")
    }
    fn private_test(id: &str, source: &str) -> QaTestFile {
        QaTestFile {
            test_id: id.into(),
            criterion_ids: vec![CRITERION.into()],
            file: format!("{id}.rs"),
            contents: source.into(),
        }
    }
    fn make_writable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .expect("simulate a private-file replacement attack");
        }
        #[cfg(not(unix))]
        {
            let mut permissions = std::fs::metadata(path)
                .expect("private artifact")
                .permissions();
            // This branch changes the Windows read-only attribute, not Unix mode bits.
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(path, permissions).expect("simulate private-file replacement");
        }
    }
    fn proposal(test: &QaTestFile) -> Value {
        json!({"tests":[{"test_id":test.test_id,"criterion_ids":test.criterion_ids,"file":test.file,"contents":test.contents}]})
    }
    fn fenced(value: &Value) -> String {
        format!("```kaisa-qa\n{value}\n```")
    }

    #[test]
    fn a_qa_answer_with_extra_fields_fences_unknown_criteria_or_unsafe_paths_is_rejected() {
        let criteria = BTreeSet::from([CRITERION.to_owned()]);
        let test = private_test("empty_input_is_rejected", FAILING_TEST);
        let valid = proposal(&test);
        assert_eq!(
            parse_qa_answer(&fenced(&valid), &criteria).expect("strict valid proposal"),
            vec![test],
            "a valid proposal preserves its mapping and source exactly"
        );
        let mut extra = valid.clone();
        extra["developer_reasoning"] = json!(DEVELOPER_MARKER);
        let mut unknown = valid.clone();
        unknown["tests"][0]["criterion_ids"] = json!(["AC-unknown"]);
        let mut traversal = valid.clone();
        traversal["tests"][0]["file"] = json!("../src/lib.rs");
        let mut repeated = valid.clone();
        repeated["tests"]
            .as_array_mut()
            .expect("tests")
            .push(valid["tests"][0].clone());
        let wrong = [
            fenced(&extra),
            fenced(&unknown),
            fenced(&traversal),
            fenced(&repeated),
            format!("prose\n{}", fenced(&valid)),
            format!("{}\n{}", fenced(&valid), fenced(&valid)),
            "```kaisa-qa\n{\"tests\":[],\"tests\":[]}\n```".into(),
        ];
        for answer in wrong {
            assert!(
                parse_qa_answer(&answer, &criteria).is_err(),
                "invalid proposals must not gain a private artifact: {answer}"
            );
        }
    }

    #[test]
    fn private_qa_cannot_start_while_the_developers_recorded_execution_is_running() {
        let fixture = Fixture::new();
        let developer = fixture.developer(false);
        let spec = fixture.spec();
        let toolchain = fixture.toolchain();
        let author = fixture.fake(private_test("acceptance", PASSING_TEST));
        let request = QaRequest {
            repository: &fixture.repository,
            base: &fixture.base,
            state_directory: &fixture.state,
            run_id: "run_qa",
            candidate: &fixture.candidate,
            frozen_spec: &spec,
            developer: &developer,
            author: &author,
            blind: &author,
            toolchain: &toolchain,
        };
        let error =
            run_private_qa(&request).expect_err("a running developer cannot overlap QA authoring");
        assert!(
            error.detail.contains("exited"),
            "temporal isolation is the reason for refusing QA"
        );
        assert!(
            !fixture.state.join("qa").exists(),
            "no private suite may be authored before the developer exits"
        );
    }

    // A real sandboxed fake-agent pair, followed by Cargo on disposable checkouts. The fake
    // developer receipt has already exited; its private context and tests carry a canary.
    #[test]
    fn independent_qa_records_clean_contexts_and_a_failing_result_for_the_unmet_criterion() {
        let fixture = Fixture::new();
        let developer = fixture.developer(true);
        let spec = fixture.spec();
        let toolchain = fixture.toolchain();
        let author = fixture.fake(private_test("empty_input_is_rejected", FAILING_TEST));
        let blind = fixture.fake(private_test("nonempty_input_is_accepted", PASSING_TEST));
        let outcome = run_private_qa(&QaRequest {
            repository: &fixture.repository,
            base: &fixture.base,
            state_directory: &fixture.state,
            run_id: "run_qa",
            candidate: &fixture.candidate,
            frozen_spec: &spec,
            developer: &developer,
            author: &author,
            blind: &blind,
            toolchain: &toolchain,
        })
        .expect("private QA runs");
        let ledger = EventStore::open_read_only(&fixture.state.join("kaisa.db"))
            .expect("recorded context ledger");
        for attempt in &outcome.attempts {
            let pack = String::from_utf8(
                ledger
                    .context_pack(&attempt.context_pack)
                    .expect("read pack")
                    .expect("recorded pack"),
            )
            .expect("UTF-8 pack");
            assert!(
                pack.contains("frozen_spec")
                    && pack.contains("public_interface")
                    && pack.contains(CRITERION),
                "QA gets the frozen contract and criterion mapping"
            );
            assert!(
                !pack.contains(DEVELOPER_MARKER)
                    && !pack.contains(PRIVATE_MARKER)
                    && !pack.contains(&developer.context_pack),
                "recorded QA context excludes reasoning, test results, private tests and developer context ids: {pack}"
            );
        }
        let failure = outcome
            .results
            .iter()
            .find(|result| !result.passed)
            .expect("bug must produce failing private result");
        assert_eq!(
            failure.criterion_ids,
            vec![CRITERION.to_owned()],
            "the failure maps to the unmet acceptance criterion"
        );
        assert_eq!(
            failure.candidate_tree, fixture.candidate.tree,
            "the failure is bound to the frozen source tree"
        );
        assert!(
            outcome.results.iter().any(|result| result.passed),
            "the independent passing control must execute, not just fail to compile"
        );
        let events = ledger.read_from(Sequence::FIRST).expect("QA events");
        assert!(
            events.iter().any(
                |event| event.recorded.event.kind == EventKind::QaResult && {
                    let payload: Value =
                        serde_json::from_str(&event.recorded.event.payload).expect("event payload");
                    payload["passed"] == false
                        && payload["criterion_ids"] == json!([CRITERION])
                        && payload["candidate_tree"] == fixture.candidate.tree
                }
            ),
            "an unmet private criterion must be a failing qa.result ledger event"
        );
        let feedback = revision_feedback(&outcome.results);
        assert!(
            feedback
                .iter()
                .any(|f| f.criterion_id == CRITERION && !f.passed),
            "revision feedback carries Kaisa-computed criterion and failure status: {feedback:?}"
        );
        assert!(
            !format!("{feedback:?}").contains(PRIVATE_MARKER),
            "compiler and assertion source must never enter developer feedback"
        );
        for suite in &outcome.suites {
            assert!(
                suite.directory().starts_with(
                    fixture
                        .state
                        .canonicalize()
                        .expect("canonical state")
                        .join("qa")
                ) && !suite.directory().starts_with(&fixture.repository),
                "QA sources stay in private state"
            );
        }
        let status = Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=all"])
            .current_dir(&fixture.repository)
            .output()
            .expect("developer status");
        assert!(
            status.stdout.is_empty(),
            "no QA files or changes are installed into the developer checkout: {}",
            String::from_utf8_lossy(&status.stdout)
        );
        assert_eq!(
            freeze_candidate(&fixture.repository, "HEAD").expect("candidate after checks"),
            fixture.candidate,
            "the original frozen candidate is unchanged"
        );
    }

    #[test]
    fn tampering_with_a_private_test_prevents_execution_and_records_no_result() {
        let fixture = Fixture::new();
        let test = private_test("tamper_probe", PASSING_TEST);
        let suite = record_qa_suite(
            &fixture.state,
            "run_tamper",
            QaKind::Acceptance,
            std::slice::from_ref(&test),
        )
        .expect("sealed suite");
        let path = suite.directory().join(&test.file);
        make_writable(&path);
        std::fs::remove_file(&path).expect("attacker replaces sealed file");
        std::fs::write(path, FAILING_TEST).expect("attacker test");
        let result = run_qa_suite(&super::QaSuiteRequest {
            repository: &fixture.repository,
            base: &fixture.base,
            state: &fixture.state,
            run_id: "run_tamper",
            candidate: &fixture.candidate,
            suite: &suite,
            toolchain: &fixture.toolchain(),
            attempt: &fixture.developer(true),
        });
        assert!(
            result
                .expect_err("tampering is refused before the toolchain starts")
                .detail
                .contains("tampered"),
            "digest mismatch must stop execution"
        );
        let ledger =
            EventStore::open_read_only(&fixture.state.join("kaisa.db")).expect("QA ledger");
        assert!(
            !ledger
                .read_from(Sequence::FIRST)
                .expect("events")
                .iter()
                .any(|event| event.recorded.event.kind == EventKind::QaResult),
            "tampered tests cannot produce ordinary pass/fail evidence"
        );
    }

    #[test]
    fn a_private_test_that_changes_candidate_sources_cannot_produce_passing_evidence() {
        let fixture = Fixture::new();
        let source = "#[test] fn a_source_mutation_invalidates_the_evidence() { std::fs::write(\"src/lib.rs\", \"pub fn accept(_: &str) -> bool { false }\").expect(\"source attack\"); }";
        let suite = record_qa_suite(
            &fixture.state,
            "run_mutator",
            QaKind::Adversarial,
            &[private_test("source_mutator", source)],
        )
        .expect("suite");
        let results = run_qa_suite(&super::QaSuiteRequest {
            repository: &fixture.repository,
            base: &fixture.base,
            state: &fixture.state,
            run_id: "run_mutator",
            candidate: &fixture.candidate,
            suite: &suite,
            toolchain: &fixture.toolchain(),
            attempt: &fixture.developer(true),
        })
        .expect("test exits but sources checked");
        assert!(
            results
                .iter()
                .all(|result| !result.passed && !result.source_unchanged),
            "zero exit cannot override the post-check candidate source check: {results:?}"
        );
        assert_eq!(
            freeze_candidate(&fixture.repository, "HEAD").expect("untouched developer tree"),
            fixture.candidate,
            "mutation was confined to the disposable checkout"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_confined_developer_cannot_read_a_planted_private_qa_test() {
        use kaisa_sys::run_captured;
        let fixture = Fixture::new();
        let suite = record_qa_suite(
            &fixture.state,
            "run_private",
            QaKind::Acceptance,
            &[private_test("private_read_probe", PASSING_TEST)],
        )
        .expect("planted private test");
        let attempt = fixture.state.join("attempts/developer");
        let scratch = attempt.join("scratch");
        std::fs::create_dir_all(&scratch).expect("developer scratch");
        let sandbox = crate::sandbox::agent_sandbox(
            &fixture.state,
            &fixture.repository,
            &scratch,
            &[],
            &attempt.join("broker.sock"),
            None,
            false,
        )
        .expect("actual developer Seatbelt policy");
        let process = ProcessSpec {
            program: "/bin/cat".into(),
            args: vec![
                suite
                    .directory()
                    .join("private_read_probe.rs")
                    .to_string_lossy()
                    .into_owned(),
            ],
            working_directory: fixture.repository.clone(),
            env: vec![(
                "KAISA_STATE_DIR".into(),
                fixture.state.to_string_lossy().into_owned(),
            )],
        };
        let read = run_captured(
            &process,
            &sandbox.confinement(),
            &[],
            super::QA_DEADLINE,
            super::QA_OUTPUT_CAP,
        )
        .expect("confined read probe");
        assert_ne!(
            read.exit,
            Some(ProcessExit::Code(0)),
            "Seatbelt must deny a developer reading an existing private test"
        );
        let developer = fixture.developer(true);
        assert_eq!(
            qa_isolation(&developer),
            QaIsolation::Full,
            "macOS state-read isolation remains full despite the unrelated descendant gap"
        );
    }

    #[test]
    fn observed_unconfined_and_state_read_gaps_are_always_labelled_reduced() {
        let fixture = Fixture::new();
        let mut developer = fixture.developer(true);
        if !cfg!(target_os = "macos") {
            assert!(
                matches!(qa_isolation(&developer), QaIsolation::Reduced(_)),
                "unverified platform state-read guarantees must remain reduced"
            );
        }
        for (observed, missing) in [
            (true, vec![]),
            (false, vec![ContainmentCapability::WriteConfinement]),
            (false, vec![ContainmentCapability::StateReadIsolation]),
        ] {
            developer.observed = observed;
            developer.missing = missing;
            assert!(
                matches!(qa_isolation(&developer), QaIsolation::Reduced(_)),
                "an observed/unconfined developer or state-read gap must never be full"
            );
        }
    }
    #[test]
    fn private_node_tests_run_with_the_trusted_runner_and_sandboxed_bin_help_enters_only_qa_context()
     {
        let mut fixture = Fixture::new();
        std::fs::write(fixture.repository.join("package.json"), json!({"type":"module","main":"index.js","bin":{"accept":"cli.mjs"},"description":"Public Node API"}).to_string()).expect("public Node entry points");
        std::fs::write(
            fixture.repository.join("index.js"),
            "/** Reject empty input. */ export function accept(input) { return true; }",
        )
        .expect("buggy Node public API");
        std::fs::write(fixture.repository.join("cli.mjs"), "if (!process.argv.includes('--help')) throw Error('expected help'); console.log('Usage: accept <input>');").expect("Node CLI help");
        git(
            &fixture.repository,
            &["add", "package.json", "index.js", "cli.mjs"],
        );
        git(
            &fixture.repository,
            &[
                "-c",
                "user.name=QA fixture",
                "-c",
                "user.email=qa@example.invalid",
                "commit",
                "-qm",
                "public Node API",
            ],
        );
        fixture.candidate = freeze_candidate(&fixture.repository, "HEAD").expect("Node candidate");
        let failing_source = "import test from 'node:test'; import assert from 'node:assert/strict'; import { accept } from '../index.js'; test('empty input is rejected', () => { console.error('kaisa-observation:' + JSON.stringify({scenario:'empty input is rejected',input:'empty string',expected:'rejection',actual:'accepted'})); assert.equal(accept(''), false, 'PRIVATE_TEST_SOURCE_SHOULD_NOT_REACH_DEVELOPER'); });";
        let passing_source = "import test from 'node:test'; import assert from 'node:assert/strict'; import { accept } from '../index.js'; test('nonempty input is accepted', () => assert.equal(accept('hello'), true));";
        let test = |id: &str, source: &str| QaTestFile {
            test_id: id.into(),
            criterion_ids: vec![CRITERION.into()],
            file: format!("{id}.mjs"),
            contents: source.into(),
        };
        let author = fixture.fake(test("empty_input_is_rejected", failing_source));
        let blind = fixture.fake(test("nonempty_input_is_accepted", passing_source));
        let toolchain = QaToolchain::Node {
            node: program("node"),
        };
        let developer = fixture.developer(true);
        let spec = fixture.spec();
        let outcome = run_private_qa(&QaRequest {
            repository: &fixture.repository,
            base: &fixture.base,
            state_directory: &fixture.state,
            run_id: "run_node",
            candidate: &fixture.candidate,
            frozen_spec: &spec,
            developer: &developer,
            author: &author,
            blind: &blind,
            toolchain: &toolchain,
        })
        .expect("private Node QA");
        assert!(
            outcome.results.iter().any(|r| r.passed) && outcome.results.iter().any(|r| !r.passed),
            "node --test must execute both the passing control and failing criterion: {outcome:?}"
        );
        let ledger =
            EventStore::open_read_only(&fixture.state.join("kaisa.db")).expect("QA ledger");
        for attempt in &outcome.attempts {
            let bytes = ledger
                .context_pack(&attempt.context_pack)
                .expect("recorded context")
                .expect("QA context bytes");
            let pack = String::from_utf8(bytes).expect("UTF-8 context");
            assert!(
                pack.contains("Usage: accept") && pack.contains("Reject empty input"),
                "QA sees actual --help and exported function JSDoc"
            );
            assert!(
                !pack.contains(DEVELOPER_MARKER) && !pack.contains(PRIVATE_MARKER),
                "package scripts and test sources never enter QA context"
            );
        }
        assert!(
            revision_feedback(&outcome.results)
                .iter()
                .any(|failure| failure.criterion_id == CRITERION && !failure.passed),
            "Node TAP diagnostics yield observable feedback without source"
        );
    }

    #[test]
    fn an_explicit_rust_bin_is_queried_for_help_in_the_check_sandbox() {
        let mut fixture = Fixture::new();
        use std::io::Write;
        let mut manifest = std::fs::OpenOptions::new()
            .append(true)
            .open(fixture.repository.join("Cargo.toml"))
            .expect("candidate manifest");
        writeln!(
            manifest,
            "[[bin]]\nname = \"qa-cli\"\npath = \"src/command.rs\""
        )
        .expect("explicit bin");
        std::fs::write(fixture.repository.join("src/command.rs"), "fn main() { assert!(std::env::args().any(|arg| arg == \"--help\")); println!(\"Usage: qa-cli <input>\"); }").expect("custom CLI entry");
        git(
            &fixture.repository,
            &["add", "Cargo.toml", "src/command.rs"],
        );
        git(
            &fixture.repository,
            &[
                "-c",
                "user.name=QA fixture",
                "-c",
                "user.email=qa@example.invalid",
                "commit",
                "-qm",
                "public CLI",
            ],
        );
        fixture.candidate = freeze_candidate(&fixture.repository, "HEAD").expect("CLI candidate");
        let temporary = super::QaTemporary::create(&fixture.state).expect("help scratch");
        let checkout = kaisa_workspace::CandidateCheckout::create(
            &fixture.repository,
            &fixture.candidate,
            &temporary.0.join("help"),
        )
        .expect("fresh help checkout");
        let sources = crate::verify::CheckedSources::record(checkout.path()).expect("pin sources");
        let mut interface = crate::qa_interface::extract_public_interface(checkout.path())
            .expect("public contract");
        let toolchain = fixture.toolchain();
        let spec = fixture.spec();
        let developer = fixture.developer(true);
        let agent = fixture.fake(private_test("acceptance", PASSING_TEST));
        super::add_cli_help(
            &QaRequest {
                repository: &fixture.repository,
                base: &fixture.base,
                state_directory: &fixture.state,
                run_id: "run_help",
                candidate: &fixture.candidate,
                frozen_spec: &spec,
                developer: &developer,
                author: &agent,
                blind: &agent,
                toolchain: &toolchain,
            },
            checkout.path(),
            &sources,
            &temporary.0,
            &mut interface,
        )
        .expect("sandboxed Rust help");
        assert!(
            interface
                .help
                .iter()
                .any(|(command, output)| command == "qa-cli --help"
                    && output.contains("Usage: qa-cli")),
            "custom Rust bin paths still produce actual --help context: {interface:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_qa_author_cannot_modify_the_developer_candidate_even_with_a_direct_write_probe() {
        let fixture = Fixture::new();
        let developer = fixture.developer(true);
        let spec = fixture.spec();
        let toolchain = fixture.toolchain();
        let mut author = fixture.fake(private_test("normal_input", PASSING_TEST));
        author.args.extend([
            "--try-direct-write".into(),
            fixture
                .repository
                .join("src/lib.rs")
                .to_string_lossy()
                .into_owned(),
        ]);
        let outcome = run_private_qa(&QaRequest {
            repository: &fixture.repository,
            base: &fixture.base,
            state_directory: &fixture.state,
            run_id: "run_readonly",
            candidate: &fixture.candidate,
            frozen_spec: &spec,
            developer: &developer,
            author: &author,
            blind: &author,
            toolchain: &toolchain,
        })
        .expect("fake QA's direct write must be denied");
        assert!(
            outcome.results.iter().all(|result| result.passed),
            "read-only authors can still return and run valid tests"
        );
        let status = Command::new("git")
            .args(["diff", "--exit-code"])
            .current_dir(&fixture.repository)
            .status()
            .expect("candidate unchanged");
        assert!(
            status.success(),
            "the QA author must never change developer candidate sources"
        );
    }

    #[test]
    fn a_private_test_that_rewrites_its_installed_copy_cannot_record_a_pass() {
        let fixture = Fixture::new();
        let source = "#[test] fn rewriting_private_source_invalidates_evidence() { std::fs::write(file!(), \"// replaced private test\").expect(\"source replacement\"); }";
        let suite = record_qa_suite(
            &fixture.state,
            "run_private_mutator",
            QaKind::Adversarial,
            &[private_test("private_mutator", source)],
        )
        .expect("private suite");
        let results = run_qa_suite(&super::QaSuiteRequest {
            repository: &fixture.repository,
            base: &fixture.base,
            state: &fixture.state,
            run_id: "run_private_mutator",
            candidate: &fixture.candidate,
            suite: &suite,
            toolchain: &fixture.toolchain(),
            attempt: &fixture.developer(true),
        })
        .expect("record source tamper evidence");
        assert!(
            results.iter().all(|result| !result.passed
                && (result.private_test_unchanged || !super::qa_write_deny_available())),
            "installed private test mutation must fail even when the process exits successfully: {results:?}"
        );
        assert!(
            suite.verified_tests().is_ok(),
            "the private original remains sealed outside the disposable checkout"
        );
    }
    // RWP2 regressions: these assertions failed against the reviewed implementation.
    fn review_recommit(fixture: &mut Fixture, files: &[(&str, &str)]) {
        for (path, text) in files {
            let full = fixture.repository.join(path);
            std::fs::create_dir_all(full.parent().expect("parent")).expect("dir");
            std::fs::write(&full, text).expect("write");
            git(&fixture.repository, &["add", path]);
        }
        git(
            &fixture.repository,
            &[
                "-c",
                "user.name=r",
                "-c",
                "user.email=r@e.invalid",
                "commit",
                "-qm",
                "repro",
            ],
        );
        fixture.candidate = freeze_candidate(&fixture.repository, "HEAD").expect("refreeze");
    }
    fn review_run(fixture: &Fixture, run: &str) -> Vec<super::QaResult> {
        let suite = record_qa_suite(
            &fixture.state,
            run,
            QaKind::Acceptance,
            &[private_test("empty_input_is_rejected", FAILING_TEST)],
        )
        .expect("suite");
        run_qa_suite(&super::QaSuiteRequest {
            repository: &fixture.repository,
            base: &fixture.base,
            state: &fixture.state,
            run_id: run,
            candidate: &fixture.candidate,
            suite: &suite,
            toolchain: &fixture.toolchain(),
            attempt: &fixture.developer(true),
        })
        .expect("ran")
    }
    #[test]
    fn review_repro_a_control_buggy_candidate_fails() {
        let fixture = Fixture::new();
        let results = review_run(&fixture, "run_control");
        assert!(
            results.iter().all(|r| !r.passed),
            "the buggy candidate fails its real private assertion"
        );
    }
    #[test]
    fn review_repro_b_candidate_exit_zero_cannot_pass_a_private_criterion() {
        let mut fixture = Fixture::new();
        review_recommit(
            &mut fixture,
            &[(
                "src/lib.rs",
                "pub fn accept(input: &str) -> bool { if input.is_empty() { std::process::exit(0) } true }\n",
            )],
        );
        let results = review_run(&fixture, "run_exit");
        assert!(
            results.iter().all(|r| !r.passed),
            "premature exit zero cannot satisfy a private criterion"
        );
    }
    #[test]
    fn review_repro_c_candidate_cargo_runner_is_a_blocking_finding() {
        let mut fixture = Fixture::new();
        review_recommit(
            &mut fixture,
            &[(
                ".cargo/config.toml",
                "[target.'cfg(all())']\nrunner = \"/usr/bin/true\"\n",
            )],
        );
        let results = review_run(&fixture, "run_runner");
        assert!(
            results.iter().all(|r| r
                .blocking_findings
                .iter()
                .any(|f| f.contains("QA-influencing configuration"))),
            "changed runner config requires explicit reviewer evidence"
        );
        assert!(
            results.iter().all(|r| !r.passed),
            "candidate Cargo runners cannot turn a failing test into a pass"
        );
    }
    #[test]
    fn review_repro_d_candidate_observations_cannot_exfiltrate_private_source() {
        let mut fixture = Fixture::new();
        review_recommit(
            &mut fixture,
            &[(
                "src/lib.rs",
                r#"pub fn accept(input: &str) -> bool {
    if let Ok(entries) = std::fs::read_dir("tests") {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("kaisa_qa_") {
                let source = std::fs::read_to_string(entry.path()).unwrap_or_default();
                eprintln!("kaisa-observation:{{\"scenario\":\"leak\",\"input\":{:?},\"expected\":\"x\",\"actual\":\"y\"}}", source);
            }
        }
    }
    if let Ok(manifest) = std::fs::read_to_string("Cargo.toml") {
        for line in manifest.lines().filter(|line| line.starts_with("path = ")) {
            let path = line.trim_start_matches("path = ").trim_matches('"');
            if let Ok(source) = std::fs::read_to_string(path) {
                eprintln!("kaisa-observation:{{\"scenario\":\"leak\",\"input\":{:?},\"expected\":\"x\",\"actual\":\"y\"}}", source);
            }
        }
    }
    let _ = input;
    true
}
"#,
            )],
        );
        let results = review_run(&fixture, "run_leak");
        let feedback = revision_feedback(&results);
        assert!(
            !format!("{feedback:?}").contains(PRIVATE_MARKER),
            "candidate observations must not disclose private source"
        );
        assert!(
            feedback.iter().all(|f| f.scenario != "leak"
                && !f.message.contains("#[test]")
                && !f.message.contains("kaisa-observation:")),
            "neither the observation's scenario nor private source fragments cross into developer feedback"
        );
        assert!(
            results.iter().all(|r| !r.passed),
            "the exfiltrating candidate still fails the criterion"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn review_repro_e_qa_authors_cannot_read_developer_tests_or_history() {
        let fixture = Fixture::new();
        let dir = fixture.state.join("checks/qa_probe/author_probe");
        let scratch = dir.join("scratch");
        let public = dir.join("public");
        std::fs::create_dir_all(&scratch).expect("scratch");
        std::fs::create_dir_all(&public).expect("public");
        super::prepare_author_repository(&public, "frozen public context")
            .expect("fresh context repository");
        let probe = |program: &str, args: Vec<String>| {
            let sandbox = super::author_sandbox(
                &fixture.state,
                &fixture.repository,
                &public,
                &scratch,
                &ProcessSpec {
                    program: program.into(),
                    args: vec![],
                    working_directory: public.clone(),
                    env: vec![
                        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                    ],
                },
            )
            .expect("QA author sandbox");
            let run = kaisa_sys::run_captured(
                &ProcessSpec {
                    program: program.into(),
                    args,
                    working_directory: public.clone(),
                    env: vec![
                        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                    ],
                },
                &sandbox.confinement(),
                &[],
                super::QA_DEADLINE,
                super::QA_OUTPUT_CAP,
            )
            .expect("QA author probe runs");
            (run.exit, String::from_utf8_lossy(&run.output).into_owned())
        };
        let context = probe("/bin/cat", vec!["context.json".into()]);
        assert_eq!(
            context.1, "frozen public context",
            "the author can read its context-only repository"
        );
        let own_history = probe(
            "/usr/bin/git",
            vec!["log".into(), "-1".into(), "--format=%s".into()],
        );
        assert_eq!(
            own_history.1.trim(),
            "QA context",
            "the author sees only freshly created context history"
        );
        let cat = probe(
            "/bin/cat",
            vec![
                fixture
                    .repository
                    .join("tests/developer.rs")
                    .to_string_lossy()
                    .into_owned(),
            ],
        );
        assert_ne!(
            cat.0,
            Some(ProcessExit::Code(0)),
            "QA authors cannot read developer tests by absolute path"
        );
        let log = probe(
            "/usr/bin/git",
            vec![
                "-C".into(),
                fixture.repository.to_string_lossy().into_owned(),
                "log".into(),
                "-1".into(),
                "--format=%s".into(),
            ],
        );
        assert_ne!(
            log.0,
            Some(ProcessExit::Code(0)),
            "QA authors cannot inspect developer Git history"
        );
        let state = probe(
            "/bin/cat",
            vec![
                fixture
                    .state
                    .join("kaisa.db")
                    .to_string_lossy()
                    .into_owned(),
            ],
        );
        assert_ne!(
            state.0,
            Some(ProcessExit::Code(0)),
            "QA authors cannot read the state ledger"
        );
    }

    #[test]
    fn review_repro_f_build_script_swaps_and_restores_the_installed_private_test() {
        let mut fixture = Fixture::new();
        review_recommit(
            &mut fixture,
            &[(
                "build.rs",
                r##"fn main() {
    let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    if let Ok(entries) = std::fs::read_dir(root.join("tests")) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("kaisa_qa_") {
                let backup = root.join("target").join("qa_backup.rs");
                std::fs::create_dir_all(root.join("target")).unwrap();
                std::fs::copy(entry.path(), &backup).unwrap();
                std::fs::write(entry.path(), format!("#[test] fn forged() {{ std::fs::copy({:?}, {:?}).unwrap(); }}", backup, entry.path())).unwrap();
            }
        }
    }
}
"##,
            )],
        );
        let results = review_run(&fixture, "run_swap");
        assert!(
            results.iter().all(|r| !r.passed),
            "build scripts must not swap and restore a private test to obtain a pass"
        );
    }

    #[test]
    fn a_preexisting_exit_zero_has_no_positive_harness_evidence() {
        let mut fixture = Fixture::new();
        review_recommit(
            &mut fixture,
            &[(
                "src/lib.rs",
                "pub fn accept(_: &str) -> bool { std::process::exit(0) }",
            )],
        );
        fixture.base = fixture.candidate.commit.clone();
        let results = review_run(&fixture, "run_baseline_exit");
        assert!(
            results
                .iter()
                .all(|r| !r.passed && !r.positive_evidence && r.blocking_findings.is_empty()),
            "positive harness evidence must independently reject early exit, even without a diff finding: {results:?}"
        );
        assert!(
            revision_feedback(&results)
                .iter()
                .all(|f| f.message.contains("no positive evidence")),
            "a clean exit without every expected test is explained explicitly"
        );
    }

    #[test]
    fn a_node_exit_zero_cannot_substitute_the_file_wrapper_for_expected_tests() {
        let mut fixture = Fixture::new();
        review_recommit(
            &mut fixture,
            &[
                ("package.json", r#"{"type":"module","main":"index.js"}"#),
                (
                    "index.js",
                    "export function accept(s) { if (s === '') process.exit(0); return true; }",
                ),
            ],
        );
        fixture.base = fixture.candidate.commit.clone();
        let test = QaTestFile { test_id: "empty_node_input".into(), criterion_ids: vec![CRITERION.into()], file: "empty.mjs".into(),
            contents: "import test from 'node:test'; import assert from 'node:assert/strict'; import {accept} from '../index.js'; test('normal input', () => assert.equal(accept('hello'), true)); test('empty input', () => assert.equal(accept(''), false));".into() };
        let suite = record_qa_suite(&fixture.state, "run_node_exit", QaKind::Acceptance, &[test])
            .expect("private Node suite");
        let results = run_qa_suite(&super::QaSuiteRequest {
            repository: &fixture.repository,
            base: &fixture.base,
            state: &fixture.state,
            run_id: "run_node_exit",
            candidate: &fixture.candidate,
            suite: &suite,
            toolchain: &QaToolchain::Node {
                node: program("node"),
            },
            attempt: &fixture.developer(true),
        })
        .expect("Node test run");
        assert!(
            results
                .iter()
                .all(|r| !r.passed && !r.positive_evidence && r.blocking_findings.is_empty()),
            "Node's success for a prematurely exited file is not evidence for its named tests: {results:?}"
        );
    }

    #[test]
    fn preexisting_cargo_runners_and_wrappers_cannot_replace_the_trusted_build_or_execution() {
        let mut fixture = Fixture::new();
        review_recommit(
            &mut fixture,
            &[(
                ".cargo/config.toml",
                "[target.'cfg(all())']\nrunner = '/usr/bin/true'\n[build]\nrustc-wrapper = '/usr/bin/false'\n",
            )],
        );
        fixture.base = fixture.candidate.commit.clone();
        let tests = [
            private_test("passing_control", PASSING_TEST),
            private_test("unmet_criterion", FAILING_TEST),
        ];
        let suite = record_qa_suite(
            &fixture.state,
            "run_owned_build",
            QaKind::Acceptance,
            &tests,
        )
        .expect("suite");
        let started = std::time::Instant::now();
        let results = run_qa_suite(&super::QaSuiteRequest {
            repository: &fixture.repository,
            base: &fixture.base,
            state: &fixture.state,
            run_id: "run_owned_build",
            candidate: &fixture.candidate,
            suite: &suite,
            toolchain: &fixture.toolchain(),
            attempt: &fixture.developer(true),
        })
        .expect("neutralized config");
        eprintln!(
            "WP2b fixture: {} scenarios, one suite build, {:.3} seconds total",
            tests.len(),
            started.elapsed().as_secs_f64()
        );
        assert!(
            results.iter().any(|r| r.passed && r.positive_evidence)
                && results.iter().any(|r| !r.passed),
            "both the real passing control and real failed criterion execute despite candidate Cargo config: {results:?}"
        );
        assert!(
            results.iter().all(|r| r.blocking_findings.is_empty()),
            "this regression exercises neutralization independently of changed-config blocking"
        );
    }

    #[test]
    fn a_preexisting_build_script_cannot_write_the_external_private_source() {
        let mut fixture = Fixture::new();
        review_recommit(
            &mut fixture,
            &[(
                "build.rs",
                r###"fn main() {
            let manifest = std::fs::read_to_string("Cargo.toml").unwrap();
            for line in manifest.lines().filter(|line| line.starts_with("path = ")) {
                let path = line.trim_start_matches("path = ").trim_matches('"');
                if path.contains("kaisa_qa_") {
                    let backup = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("target/qa_backup.rs");
                    std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
                    std::fs::copy(path, &backup).unwrap();
                    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)); }
                    #[cfg(windows)] { let mut permissions = std::fs::metadata(path).unwrap().permissions(); permissions.set_readonly(false); let _ = std::fs::set_permissions(path, permissions); }
                    std::fs::write(path, format!("#[test] fn an_empty_input_is_rejected() {{ std::fs::copy({:?}, {:?}).unwrap(); }}", backup, path)).expect("attempt to replace external private test");
                }
            }
        }"###,
            )],
        );
        fixture.base = fixture.candidate.commit.clone();
        let results = review_run(&fixture, "run_external_write");
        assert!(
            results
                .iter()
                .all(|r| !r.passed && r.blocking_findings.is_empty()),
            "write denial or input digest checks must reject a preexisting script independently of configuration scanning: {results:?}"
        );
        if super::qa_write_deny_available() {
            assert!(results.iter().all(|r| r.private_test_unchanged && r.private_test_integrity == "write-denied"),
                "the build failed because immutable source was denied, and original bytes survived");
        } else {
            assert!(
                results.iter().all(|r| !r.private_test_unchanged
                    && r.private_test_integrity.starts_with("reduced")),
                "digests must not be presented as OS write-denial evidence"
            );
        }
    }

    #[test]
    fn source_bearing_panic_messages_are_redacted_even_without_observation_lines() {
        let test = private_test("redaction_probe", FAILING_TEST);
        let suite = [test.clone()];
        let run = kaisa_sys::CapturedRun { exit: Some(ProcessExit::Code(1)), truncated: false,
            output: format!("kaisa-observation:{{\"input\":{PRIVATE_MARKER:?}}}\nthread 'probe' panicked at private.rs:1:1:\nprefix {PRIVATE_MARKER} suffix\n").into_bytes() };
        let feedback = super::observable_diagnostics(&test, &suite, &run, true, false, &[]);
        assert!(
            feedback
                .iter()
                .all(|f| f.message == "[private source redacted]"),
            "assertion excerpts must pass private-source overlap scanning"
        );
        assert_eq!(
            super::redact_private("actual", &suite),
            "[private source redacted]",
            "a complete source-bearing field is redacted even below the embedded-fragment threshold"
        );
        let shuffled_whitespace = "assert ! ( ! actual ,";
        assert_eq!(
            super::redact_private(shuffled_whitespace, &suite),
            "[private source redacted]",
            "normalization must catch source fragments despite inserted whitespace"
        );
        let sentence = private_test(
            "shingle_probe",
            "one two three four five six seven eight nine",
        );
        assert_eq!(
            super::redact_private(
                "prefix one,two;three/four:five-six+seven=eight suffix",
                &[sentence]
            ),
            "[private source redacted]",
            "eight-token overlaps must be removed despite changed punctuation"
        );
    }

    #[test]
    fn a_suite_that_omits_a_frozen_criterion_is_explicitly_coverage_blocked() {
        let fixture = Fixture::new();
        let developer = fixture.developer(true);
        let mut spec = fixture.spec();
        let uncovered = "AC-uncovered";
        spec["criteria"]
            .as_array_mut()
            .expect("criteria")
            .push(json!({"id":uncovered}));
        let author = fixture.fake(private_test("covered_scenario", PASSING_TEST));
        let outcome = run_private_qa(&QaRequest {
            repository: &fixture.repository,
            base: &fixture.base,
            state_directory: &fixture.state,
            run_id: "run_coverage",
            candidate: &fixture.candidate,
            frozen_spec: &spec,
            developer: &developer,
            author: &author,
            blind: &author,
            toolchain: &fixture.toolchain(),
        })
        .expect("flag incomplete suite");
        assert_eq!(
            outcome.uncovered_criteria,
            vec![uncovered.to_owned()],
            "an untested criterion must never silently disappear from gate evidence"
        );
        let ledger = EventStore::open_read_only(&fixture.state.join("kaisa.db")).expect("ledger");
        assert!(
            ledger
                .read_from(Sequence::FIRST)
                .expect("events")
                .iter()
                .any(
                    |event| event.recorded.event.kind == EventKind::QaSuiteRecorded && {
                        let payload: Value = serde_json::from_str(&event.recorded.event.payload)
                            .expect("suite event");
                        payload["kind"] == "acceptance"
                            && payload["uncovered_criteria"] == json!([uncovered])
                            && payload["coverage_blocked"] == true
                    }
                ),
            "the suite event must carry the same blocking coverage finding as the outcome"
        );
    }

    #[test]
    fn a_revision_suite_uses_the_revision_attempts_isolation_receipt() {
        let fixture = Fixture::new();
        let mut revision = fixture.developer(true);
        revision.observed = true;
        revision.attempt = AttemptId::try_new("att_revision").expect("revision id");
        let suite = record_qa_suite(
            &fixture.state,
            "run_revision",
            QaKind::Acceptance,
            &[private_test("passing_control", PASSING_TEST)],
        )
        .expect("suite");
        let results = run_qa_suite(&super::QaSuiteRequest {
            repository: &fixture.repository,
            base: &fixture.base,
            state: &fixture.state,
            run_id: "run_revision",
            candidate: &fixture.candidate,
            suite: &suite,
            toolchain: &fixture.toolchain(),
            attempt: &revision,
        })
        .expect("revision QA");
        assert!(
            results
                .iter()
                .all(|r| matches!(r.qa_isolation, QaIsolation::Reduced(_))),
            "observed revision evidence must not inherit the first developer's full isolation"
        );
    }
    #[test]
    fn changed_qa_configuration_and_new_library_termination_calls_require_reviewer_evidence() {
        for (path, source) in [
            (".cargo/config", "[build]\nrustc-wrapper = '/usr/bin/true'"),
            ("rust-toolchain.toml", "[toolchain]\nchannel = 'stable'"),
            ("build.rs", "fn main() {}"),
            ("package.json", r#"{"scripts":{"test":"exit 0"}}"#),
            ("package.json", r#"{"jest":{"testMatch":["trivial.js"]}}"#),
            (".npmrc", "node-options=--require ./skip.js"),
        ] {
            let mut fixture = Fixture::new();
            review_recommit(&mut fixture, &[(path, source)]);
            let findings = super::qa_influencing_findings(
                &fixture.repository,
                &fixture.base,
                &fixture.candidate,
            )
            .expect("configuration scan");
            assert!(
                findings
                    .iter()
                    .any(|f| f.contains("QA-influencing configuration")),
                "candidate-controlled {path} requires reviewer evidence: {findings:?}"
            );
        }
        for source in [
            "pub fn accept(_: &str) -> bool { std::process::exit(0) }",
            "pub fn accept(_: &str) -> bool { std::process::abort() }",
            "export function accept(s) { process.exit(0); }",
            "export function accept(s) { process.kill(process.pid); }",
        ] {
            let mut fixture = Fixture::new();
            let path = if source.starts_with("pub ") {
                "src/lib.rs"
            } else {
                "index.js"
            };
            review_recommit(&mut fixture, &[(path, source)]);
            let findings = super::qa_influencing_findings(
                &fixture.repository,
                &fixture.base,
                &fixture.candidate,
            )
            .expect("termination scan");
            assert!(
                findings.iter().any(|f| f.contains("process-termination")),
                "added library termination calls require reviewer evidence: {findings:?}"
            );
        }
    }

    #[test]
    fn named_results_require_a_single_consistent_summary_and_every_expected_test() {
        let expected = vec!["first_scenario".to_owned(), "second_scenario".to_owned()];
        let complete = "test first_scenario ... ok\ntest second_scenario ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        let run = |output: &str| kaisa_sys::CapturedRun {
            exit: Some(ProcessExit::Code(0)),
            output: output.as_bytes().to_vec(),
            truncated: false,
        };
        let toolchain = QaToolchain::Rust {
            cargo: PathBuf::from("unused"),
        };
        assert!(
            super::positive_test_evidence(&run(complete), &expected, &toolchain),
            "the exact expected name set with its zero-failure summary is positive evidence"
        );
        for output in [
            complete.replace("test second_scenario ... ok\n", ""),
            complete.replace("2 passed", "1 passed"),
            complete.replace("0 ignored", "1 ignored"),
            format!("{complete}{complete}"),
            "".into(),
        ] {
            assert!(
                !super::positive_test_evidence(&run(&output), &expected, &toolchain),
                "missing names, inconsistent counts, ignored tests and duplicate summaries cannot pass"
            );
        }
        let node = QaToolchain::Node {
            node: PathBuf::from("unused"),
        };
        let tap = "TAP version 13\nok 1 - first_scenario\nok 2 - second_scenario\n1..2\n# tests 2\n# pass 2\n# fail 0\n# cancelled 0\n# skipped 0\n# todo 0\n";
        assert!(
            super::positive_test_evidence(&run(tap), &expected, &node),
            "named Node TAP tests require a complete matching summary"
        );
        assert!(
            !super::positive_test_evidence(
                &run(&tap.replace("second_scenario", "/private/qa/test.mjs")),
                &expected,
                &node
            ),
            "a Node file wrapper cannot substitute for the expected test name"
        );
    }
}
