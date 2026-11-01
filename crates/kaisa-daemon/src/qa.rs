//! Independent private QA after a developer has exited. QA sees only a frozen spec and an
//! allow-listed public interface, proposes files through one strict fence, and never writes
//! the candidate. Each private scenario executes on its own disposable candidate checkout.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kaisa_domain::{
    AttemptId, ContainmentCapability, ContentDigest, EventKind, Execution, ExitStatus,
};
use kaisa_ledger::EventStore;
use kaisa_sys::{CapturedRun, ProcessExit, ProcessSpec, run_captured};
use kaisa_workspace::{Candidate, CandidateCheckout, QaKind, QaSuite, QaTestFile, record_qa_suite};
use serde_json::{Value, json};

use crate::goal::{GoalError, GoalOutcome, at, record, record_execution};
use crate::ids::fresh_suffix;
use crate::qa_interface::{PublicInterface, extract_public_interface, node_bins, rust_bins};
use crate::sandbox::AgentSandbox;
use crate::verify::{CheckedSources, check_sandbox, review_output_tail};

const QA_DEADLINE: Duration = Duration::from_secs(30 * 60);
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

/// Inputs to two independent, tool-less QA attempts. Agent processes read plain stdin and
/// return raw or provider-stream final answers. The caller supplies trusted launcher argv.
pub struct QaRequest<'a> {
    /// Repository containing the frozen commit.
    pub repository: &'a Path,
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
    /// True only on successful exit with an unchanged candidate source tree.
    pub passed: bool,
    /// BLAKE3 digest of the captured output.
    pub output_digest: String,
    /// Frozen tree tested.
    pub candidate_tree: String,
    /// Whether the check actually ran with OS confinement.
    pub confined: bool,
    /// Source changes invalidate evidence even if the test returned zero.
    pub source_unchanged: bool,
    /// The installed copy still matches the private receipt after the check.
    pub private_test_unchanged: bool,
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
    /// Input as reported by the test's structured diagnostic, or an unavailable label.
    pub input: String,
    /// Expected observable result, never an assertion's source code.
    pub expected: String,
    /// Actual observable result.
    pub actual: String,
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
    add_cli_help(
        request,
        checkout.path(),
        &sources,
        &temporary.0,
        &mut interface,
    )?;
    let mut suites = Vec::new();
    let mut attempts = Vec::new();
    // Both authoring sessions finish before any test executes. Neither author gets the other
    // suite, developer diagnostics, project profile, lessons, diff, or revision history.
    for (kind, process) in [
        (QaKind::Acceptance, request.author),
        (QaKind::Adversarial, request.blind),
    ] {
        let pack = qa_context(request.frozen_spec, &interface, kind, request.toolchain);
        let (attempt, answer) = author_suite(
            request.state_directory,
            &temporary.0,
            process,
            &pack,
            &mut ledger,
        )?;
        let tests = parse_qa_answer(&answer, &criteria)?;
        let suite = record_qa_suite(request.state_directory, request.run_id, kind, &tests)
            .map_err(at("sealing private QA suite"))?;
        record(
            &mut ledger,
            EventKind::QaSuiteRecorded,
            request.run_id,
            json!({"run_id":request.run_id,"kind":kind.code(),"candidate_tree":request.candidate.tree,
                "manifest_digest":suite.manifest_digest(),"attempt":attempt.attempt.as_str(),
                "manifest":tests.iter().map(|t| json!({"test_id":t.test_id,"criterion_ids":t.criterion_ids,
                    "file":t.file,"digest":blake3::hash(t.contents.as_bytes()).to_hex().to_string()})).collect::<Vec<_>>()}),
        )?;
        suites.push(suite);
        attempts.push(attempt);
    }
    let mut results = Vec::new();
    for suite in &suites {
        results.extend(run_qa_suite(
            request.repository,
            request.state_directory,
            request.run_id,
            request.candidate,
            suite,
            request.toolchain,
        )?);
    }
    Ok(QaOutcome {
        suites,
        attempts,
        results,
        qa_isolation: qa_isolation(request.developer),
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
            "Rust integration tests run as tests/kaisa_qa_<id>.rs in the package root."
        }
        QaToolchain::Node { .. } => {
            "Node tests run with node --test in a temporary directory one level beneath the package root; import public modules with ../ paths."
        }
    };
    format!(
        "{mission}\nYou are a read-only QA author. Use only the quoted frozen_spec and public_interface data below. Do not inspect implementation, development tests, transcripts, developer answers, Git history or other QA suites. Do not write files or invoke tools. Your only output is exactly one fenced kaisa-qa JSON block, with no surrounding prose. Schema: {{\"tests\":[{{\"test_id\":\"scenario_name\",\"criterion_ids\":[\"AC-1\"],\"file\":\"scenario.rs\",\"contents\":\"test source\"}}]}}. Each file is one scenario; use stable descriptive test ids and assert the criterion's observable result. {placement} On failure emit a single line beginning kaisa-observation: followed by JSON with exactly scenario, input, expected, actual string fields; those fields describe observable behaviour only, never test source.\nUntrusted contract data (JSON):\n{}\n",
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

fn author_suite(
    state: &Path,
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
    let digest = ContentDigest::try_new(blake3::hash(pack.as_bytes()).to_hex().as_str())
        .map_err(at("hashing QA context"))?;
    record(
        ledger,
        EventKind::AttemptAdmitted,
        attempt.as_str(),
        json!({"task":format!("task_{suffix}"),"role":"qa","read_only":true}),
    )?;
    ledger
        .record_context_pack(&attempt, &digest, pack.as_bytes())
        .map_err(at("recording isolated QA context"))?;
    let sandbox = author_sandbox(state, &public, &scratch, process)
        .map_err(at("confining read-only QA author"))?;
    let missing = sandbox.missing();
    if missing.contains(&ContainmentCapability::WriteConfinement) {
        return Err(error("read-only QA author confinement unavailable"));
    }
    let mut process = process.clone();
    process.working_directory = public;
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

#[cfg(unix)]
fn author_sandbox(
    state: &Path,
    public: &Path,
    scratch: &Path,
    _: &ProcessSpec,
) -> io::Result<AgentSandbox> {
    crate::sandbox::agent_sandbox(
        state,
        public,
        scratch,
        &[],
        &scratch.join("unused.sock"),
        None,
        false,
    )
}
#[cfg(windows)]
fn author_sandbox(
    _: &Path,
    public: &Path,
    scratch: &Path,
    process: &ProcessSpec,
) -> io::Result<AgentSandbox> {
    let mut container =
        crate::sandbox::ContainedAttempt::create(scratch, &format!("kaisaQa{}", fresh_suffix()))?;
    container.grant_for_agent(public, public, scratch, &[&process.program])?;
    Ok(AgentSandbox::AppContainer(container))
}
#[cfg(not(any(unix, windows)))]
fn author_sandbox(_: &Path, _: &Path, _: &Path, _: &ProcessSpec) -> io::Result<AgentSandbox> {
    Ok(AgentSandbox::Unconfined)
}

/// Runs a previously recorded suite, verifying all digests again before *every* scenario.
/// Injected tests go only into a disposable checkout, never the developer or frozen source.
///
/// # Errors
/// Returns an error before execution on tampering; failed tests are recorded failing evidence.
pub fn run_qa_suite(
    repository: &Path,
    state: &Path,
    run_id: &str,
    candidate: &Candidate,
    suite: &QaSuite,
    toolchain: &QaToolchain,
) -> Result<Vec<QaResult>, GoalError> {
    let mut results = Vec::new();
    let mut ledger =
        EventStore::open(&state.join("kaisa.db")).map_err(at("opening QA result ledger"))?;
    let initial = suite
        .verified_tests()
        .map_err(at("verifying private QA digest"))?;
    for original in &initial {
        let tests = suite
            .verified_tests()
            .map_err(at("rechecking private QA digest before execution"))?;
        let test = tests
            .iter()
            .find(|test| test.test_id == original.test_id)
            .ok_or_else(|| error("QA manifest changed"))?;
        let temporary = QaTemporary::create(state)?;
        let checkout =
            CandidateCheckout::create(repository, candidate, &temporary.0.join("candidate"))
                .map_err(at("checking out private QA candidate"))?;
        let sources = CheckedSources::record(checkout.path())
            .map_err(at("pinning private QA source tree"))?;
        let (spec, installed) = install_test(checkout.path(), test, toolchain)?;
        let (run, confined, unchanged) = candidate_command(
            state,
            checkout.path(),
            &temporary.0,
            &sources,
            &candidate.tree,
            &spec,
        )?;
        let private_test_unchanged =
            std::fs::read(&installed).is_ok_and(|bytes| bytes == test.contents.as_bytes());
        let passed = !run.truncated
            && unchanged
            && private_test_unchanged
            && run.exit == Some(ProcessExit::Code(0));
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
            failures: if passed {
                Vec::new()
            } else {
                observable_diagnostics(test, &run, unchanged && private_test_unchanged)
            },
        };
        record(
            &mut ledger,
            EventKind::QaResult,
            &test.test_id,
            json!({"run_id":run_id,"test_id":result.test_id,
            "kind":result.kind.code(),"criterion_ids":result.criterion_ids,"passed":result.passed,
            "output_digest":result.output_digest,"candidate_tree":result.candidate_tree,"confined":result.confined,
            "source_unchanged":result.source_unchanged,"private_test_unchanged":result.private_test_unchanged}),
        )?;
        results.push(result);
    }
    Ok(results)
}

fn install_test(
    checkout: &Path,
    test: &QaTestFile,
    toolchain: &QaToolchain,
) -> Result<(ProcessSpec, PathBuf), GoalError> {
    let name = format!(
        "kaisa_qa_{}",
        blake3::hash(test.test_id.as_bytes()).to_hex()
    );
    let (program, args, path) = match toolchain {
        QaToolchain::Rust { cargo } => {
            if !test.file.ends_with(".rs") {
                return Err(error("Rust private QA requires .rs files"));
            }
            let directory = checkout.join("tests");
            safe_scratch_directory(&directory)?;
            (
                cargo.clone(),
                vec![
                    "test".into(),
                    "--offline".into(),
                    "--test".into(),
                    name.clone(),
                    "--".into(),
                    "--nocapture".into(),
                ],
                directory.join(format!("{name}.rs")),
            )
        }
        QaToolchain::Node { node } => {
            let extension = Path::new(&test.file)
                .extension()
                .and_then(|s| s.to_str())
                .filter(|s| ["js", "mjs", "cjs"].contains(s))
                .ok_or_else(|| error("Node private QA requires JS files"))?;
            let directory = checkout.join(format!(".kaisa-qa_{}", fresh_suffix()));
            std::fs::create_dir(&directory).map_err(at("creating private Node test scratch"))?;
            let path = directory.join(format!("{name}.{extension}"));
            (
                node.clone(),
                vec!["--test".into(), path.to_string_lossy().into_owned()],
                path,
            )
        }
    };
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(at("installing private QA in disposable checkout"))?;
    file.write_all(test.contents.as_bytes())
        .map_err(at("writing disposable QA test"))?;
    Ok((
        ProcessSpec {
            program,
            args,
            working_directory: checkout.to_owned(),
            env: Vec::new(),
        },
        path,
    ))
}

fn safe_scratch_directory(path: &Path) -> Result<(), GoalError> {
    if path.try_exists().map_err(at("checking QA scratch"))? {
        let metadata =
            std::fs::symlink_metadata(path).map_err(at("checking QA scratch metadata"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(error("QA scratch directory is not a regular directory"));
        }
    } else {
        std::fs::create_dir(path).map_err(at("creating QA scratch"))?;
    }
    Ok(())
}

fn candidate_command(
    state: &Path,
    checkout: &Path,
    temporary: &Path,
    sources: &CheckedSources,
    tree: &str,
    process: &ProcessSpec,
) -> Result<(CapturedRun, bool, bool), GoalError> {
    let sandbox = check_sandbox(state, checkout, temporary)
        .map_err(at("building private QA check sandbox"))?;
    let mut process = process.clone();
    process.env.push((
        "KAISA_STATE_DIR".to_owned(),
        temporary.join("test-state").to_string_lossy().into_owned(),
    ));
    for key in ["TMPDIR", "TEMP", "TMP"] {
        process
            .env
            .push((key.to_owned(), temporary.to_string_lossy().into_owned()));
    }
    let run = run_captured(
        &process,
        &sandbox.confinement(),
        &[],
        QA_DEADLINE,
        QA_OUTPUT_CAP,
    )
    .map_err(at("running private QA check"))?;
    let unchanged = sources
        .matches(checkout, tree)
        .map_err(at("checking private QA candidate integrity"))?;
    Ok((run, sandbox.is_confined(), unchanged))
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
                            "run".into(),
                            "--offline".into(),
                            "--bin".into(),
                            name,
                            "--".into(),
                            "--help".into(),
                        ],
                        working_directory: checkout.to_owned(),
                        env: Vec::new(),
                    },
                )
            })
            .collect(),
    };
    for (name, spec) in commands {
        let (run, confined, unchanged) = candidate_command(
            request.state_directory,
            checkout,
            temporary,
            sources,
            &request.candidate.tree,
            &spec,
        )?;
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

fn observable_diagnostics(
    test: &QaTestFile,
    run: &CapturedRun,
    unchanged: bool,
) -> Vec<ObservableFailure> {
    // Raw compiler/panic output can echo test source. Copy only the dedicated observable
    // diagnostic's scalar fields, never output snippets, stack traces or assertion lines.
    let output = review_output_tail(run);
    let observed = output
        .lines()
        .filter_map(|line| {
            line.trim_start_matches("# ")
                .strip_prefix("kaisa-observation:")
        })
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find(|value| {
            exact_keys(value, &["scenario", "input", "expected", "actual"]).is_ok()
                && ["scenario", "input", "expected", "actual"]
                    .iter()
                    .all(|key| value[*key].is_string())
        });
    test.criterion_ids
        .iter()
        .map(|id| {
            let field = |key: &str, fallback: &str| {
                observed
                    .as_ref()
                    .and_then(|v| v[key].as_str())
                    .unwrap_or(fallback)
                    .chars()
                    .take(512)
                    .collect::<String>()
            };
            ObservableFailure {
                criterion_id: id.clone(),
                scenario: field("scenario", &test.test_id),
                input: field("input", "not reported"),
                expected: field("expected", "scenario exits successfully"),
                actual: if !unchanged {
                    "test changed candidate or installed private test sources".into()
                } else {
                    field(
                        "actual",
                        &format!("test process {:?}; truncated={}", run.exit, run.truncated),
                    )
                },
            }
        })
        .collect()
}

/// Source-free revision feedback: criterion, scenario, input, expected and actual only.
#[must_use]
pub fn revision_feedback(results: &[QaResult]) -> Vec<ObservableFailure> {
    results
        .iter()
        .filter(|result| !result.passed)
        .flat_map(|result| result.failures.clone())
        .collect()
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
            feedback.iter().any(|f| f.criterion_id == CRITERION
                && f.input == "empty string"
                && f.expected == "rejection"
                && f.actual == "accepted"),
            "revision feedback carries the observed input and expected versus actual outcome: {feedback:?}"
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
        let result = run_qa_suite(
            &fixture.repository,
            &fixture.state,
            "run_tamper",
            &fixture.candidate,
            &suite,
            &fixture.toolchain(),
        );
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
        let results = run_qa_suite(
            &fixture.repository,
            &fixture.state,
            "run_mutator",
            &fixture.candidate,
            &suite,
            &fixture.toolchain(),
        )
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
        std::fs::write(fixture.repository.join("package.json"), json!({"type":"module","main":"index.js","bin":{"accept":"cli.mjs"},"scripts":{"test":DEVELOPER_MARKER}}).to_string()).expect("public Node entry points");
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
                .any(|failure| failure.expected == "rejection" && failure.actual == "accepted"),
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
        let results = run_qa_suite(
            &fixture.repository,
            &fixture.state,
            "run_private_mutator",
            &fixture.candidate,
            &suite,
            &fixture.toolchain(),
        )
        .expect("record source tamper evidence");
        assert!(
            results
                .iter()
                .all(|result| !result.passed && !result.private_test_unchanged),
            "installed private test mutation must fail even when the process exits successfully: {results:?}"
        );
        assert!(
            suite.verified_tests().is_ok(),
            "the private original remains sealed outside the disposable checkout"
        );
    }
}
