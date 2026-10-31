//! `run_goal` end to end with a scripted agent: mission, pack, worktree, agent through the real
//! bridge, exit record, preservation, reclamation.

use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_daemon::{AGENT_IDENTITY_FILE, GoalOutcome, GoalRequest, run_goal};
use kaisa_domain::{RepoPath, WriteSet};
use kaisa_sys::{ProcessExit, ProcessSpec};

/// What the user asks for; the fake agent writes the start of its pack into a file.
const GOAL: &str = "Add notes about the login flow.";
/// The file the fake agent writes.
const NOTES: &str = "src/notes.md";

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .expect("git is installed on every CI runner");
    assert!(output.status.success(), "git {args:?} failed: {output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repository with one commit and a `src` directory, plus a state directory, removed on drop.
struct Workbench {
    base: PathBuf,
}

impl Workbench {
    fn new(name: &str) -> Self {
        let base: PathBuf =
            std::env::temp_dir().join(format!("kaisa-goal-{name}-{}", std::process::id()));
        let repository: PathBuf = base.join("repo");
        std::fs::create_dir_all(repository.join("src")).expect("writable temp dir");
        std::fs::write(repository.join("src/lib.rs"), "pub fn login() {}\n").expect("written");
        git(&repository, &["init", "--quiet", "--initial-branch=main"]);
        git(&repository, &["add", "."]);
        git(
            &repository,
            &[
                "-c",
                "user.name=Kaisa Test",
                "-c",
                "user.email=kaisa-test@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "initial",
            ],
        );
        Self { base }
    }

    fn repository(&self) -> PathBuf {
        self.base.join("repo")
    }

    fn state(&self) -> PathBuf {
        self.base.join("state")
    }
}

impl Drop for Workbench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn the_fake_agent() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX))
}

#[test]
fn a_goal_runs_to_a_preserved_result_with_a_scripted_agent() {
    let bench = Workbench::new("preserved");
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let launch = |mcp_config: &Path| ProcessSpec {
        program: the_fake_agent(),
        args: vec![
            "--mcp-config".to_owned(),
            mcp_config.to_string_lossy().into_owned(),
            "--fake-write".to_owned(),
            NOTES.to_owned(),
        ],
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    };

    let outcome: GoalOutcome = run_goal(&GoalRequest {
        repository: &bench.repository(),
        goal: GOAL,
        base: "HEAD",
        state_directory: &bench.state(),
        write_set: &write_set,
        role: kaisa_domain::Role::Developer,
        launch: &launch,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        egress_hosts: &[],
        agent_writable: &[],
        local_model_port: None,
        observed: false,
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("the goal runs");
    let preserved_notes: String = git(
        &bench.repository(),
        &["show", &format!("{}:{NOTES}", outcome.branch)],
    );

    assert_eq!(
        (
            outcome.exit,
            outcome.commit.is_some(),
            outcome.kept_back.is_empty()
        ),
        (Some(ProcessExit::Code(0)), true, true),
        "the agent finished and its work was preserved, got {outcome:?}"
    );
    assert!(
        preserved_notes.contains(GOAL),
        "the preserved file shows the agent started from the context pack with the goal"
    );
    assert!(
        !bench
            .state()
            .join("worktrees")
            .join(outcome.attempt.as_str())
            .exists(),
        "the worktree is reclaimed once the work is on its branch"
    );
    let recorded_identity: String = std::fs::read_to_string(
        bench
            .state()
            .join("attempts")
            .join(outcome.attempt.as_str())
            .join(AGENT_IDENTITY_FILE),
    )
    .unwrap_or_default();
    assert!(
        !recorded_identity.is_empty(),
        "the agent's identity was recorded so a crash of Kaisa can be recovered from"
    );
}

/// A fake CLI renamed to select the Claude adapter; no real provider process is started.
#[cfg(target_os = "macos")]
fn a_scripted_claude_adapter(bench: &Workbench) -> PathBuf {
    let program = bench.base.join("claude");
    std::fs::copy(the_fake_agent(), &program).expect("copy only the fake agent, never real Claude");
    program
}

#[cfg(target_os = "macos")]
#[test]
fn a_confined_claude_adapter_records_its_login_keychain_exception_in_the_ledger() {
    const SUCCESS: ProcessExit = ProcessExit::Code(0);
    let bench = Workbench::new("claude-keychain-notice");
    let program = a_scripted_claude_adapter(&bench);
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let launch = |mcp_config: &Path| ProcessSpec {
        program: program.clone(),
        args: vec![
            "--mcp-config".into(),
            mcp_config.to_string_lossy().into_owned(),
            "--fake-write".into(),
            NOTES.into(),
        ],
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    };
    let outcome = run_goal(&GoalRequest {
        repository: &bench.repository(),
        goal: GOAL,
        base: "HEAD",
        state_directory: &bench.state(),
        write_set: &write_set,
        role: kaisa_domain::Role::Developer,
        launch: &launch,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        egress_hosts: &[],
        agent_writable: &[],
        local_model_port: None,
        observed: false,
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("the scripted Claude adapter runs confined");
    assert_eq!(
        outcome.exit,
        Some(SUCCESS),
        "the fake adapter must complete"
    );
    let mut ledger =
        kaisa_ledger::EventStore::open(&bench.state().join("kaisa.db")).expect("ledger");
    let events = ledger
        .read_from(kaisa_domain::Sequence::FIRST)
        .expect("ledger events");
    let notices: Vec<serde_json::Value> = events
        .iter()
        .filter_map(|event| {
            let payload: serde_json::Value =
                serde_json::from_str(&event.recorded.event.payload).expect("JSON event");
            (payload.get("login_keychain_access") == Some(&serde_json::Value::Bool(true)))
                .then_some(payload)
        })
        .collect();
    assert!(
        notices.iter().any(|payload| payload["notice"]
            .as_str()
            .is_some_and(|text| text.contains("login keychain"))),
        "the ledger must disclose the confined agent's keychain exception"
    );
    ledger
        .rebuild_attempts()
        .expect("the notice must preserve the state event schema");
}

/// Where the probe tries to write around the broker, relative to the agent's worktree.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
const DIRECT_WRITE: &str = "src/direct.md";

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[test]
fn a_brokered_agent_process_cannot_write_the_worktree_directly() {
    let bench = Workbench::new("confined");
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let launch = |mcp_config: &Path| ProcessSpec {
        program: the_fake_agent(),
        args: vec![
            "--mcp-config".to_owned(),
            mcp_config.to_string_lossy().into_owned(),
            "--fake-write".to_owned(),
            NOTES.to_owned(),
            "--try-direct-write".to_owned(),
            DIRECT_WRITE.to_owned(),
        ],
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    };

    let outcome: GoalOutcome = run_goal(&GoalRequest {
        repository: &bench.repository(),
        goal: GOAL,
        base: "HEAD",
        state_directory: &bench.state(),
        write_set: &write_set,
        role: kaisa_domain::Role::Developer,
        launch: &launch,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        egress_hosts: &[],
        agent_writable: &[],
        local_model_port: None,
        observed: false,
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("the goal runs");
    let preserved: String = git(
        &bench.repository(),
        &["ls-tree", "-r", "--name-only", &outcome.branch],
    );

    let transcript: String = std::fs::read_to_string(
        bench
            .state()
            .join("attempts")
            .join(outcome.attempt.as_str())
            .join("transcript.log"),
    )
    .unwrap_or_default();
    assert!(
        outcome.confined && outcome.exit == Some(ProcessExit::Code(0)),
        "the probe's own write was refused, so the agent went on through the broker: \
         {outcome:?}; agent said {transcript:?}"
    );
    assert!(
        preserved.contains(NOTES) && !preserved.contains(DIRECT_WRITE),
        "only the brokered change reached the preserved branch, got {preserved:?}"
    );
}

/// The model provider an agent may reach; resolving it is never needed by the test.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MODEL_PROVIDER: &str = "api.model-provider.example";
/// A host the agent was not given.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const UNLISTED_HOST: &str = "exfiltration.example";

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_confined_agent_reaches_the_network_only_through_its_egress_broker() {
    let bench = Workbench::new("egress");
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let elsewhere = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("loopback works");
    elsewhere
        .set_nonblocking(true)
        .expect("the listener can be polled");
    let elsewhere_address: String = elsewhere.local_addr().expect("bound").to_string();
    let launch = |mcp_config: &Path| ProcessSpec {
        program: the_fake_agent(),
        args: vec![
            "--mcp-config".to_owned(),
            mcp_config.to_string_lossy().into_owned(),
            "--fake-write".to_owned(),
            NOTES.to_owned(),
            "--try-direct-connect".to_owned(),
            elsewhere_address.clone(),
            "--expect-proxy-refuses".to_owned(),
            UNLISTED_HOST.to_owned(),
        ],
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    };

    let outcome: GoalOutcome = run_goal(&GoalRequest {
        repository: &bench.repository(),
        goal: GOAL,
        base: "HEAD",
        state_directory: &bench.state(),
        write_set: &write_set,
        role: kaisa_domain::Role::Developer,
        launch: &launch,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        egress_hosts: &[MODEL_PROVIDER],
        agent_writable: &[],
        local_model_port: None,
        observed: false,
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("the goal runs");

    assert!(
        outcome.confined && outcome.exit == Some(ProcessExit::Code(0)),
        "the agent reached its egress broker, was refused an unlisted host, and could not \
         connect around it: {outcome:?}"
    );
    assert!(
        elsewhere.accept().is_err(),
        "nothing outside the broker received a connection from the agent"
    );
}

/// True if any process's command line names `attempt_directory` (Unix `ps`).
#[cfg(unix)]
fn a_process_still_serves(attempt_directory: &str) -> bool {
    let listing = Command::new("ps")
        .args(["-A", "-o", "command="])
        .output()
        .expect("ps is on every Unix runner");
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .any(|line| line.contains(attempt_directory) && !line.contains("ps -A"))
}

/// How long the bridge of a dead agent may take to notice its closed input.
#[cfg(unix)]
const BRIDGE_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(unix)]
#[test]
fn an_agent_that_crashes_after_writing_loses_no_work_and_leaves_no_bridge_behind() {
    let bench = Workbench::new("crash");
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let launch = |mcp_config: &Path| ProcessSpec {
        program: the_fake_agent(),
        args: vec![
            "--mcp-config".to_owned(),
            mcp_config.to_string_lossy().into_owned(),
            "--fake-write".to_owned(),
            NOTES.to_owned(),
            "--crash-after-write".to_owned(),
        ],
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    };

    let outcome: GoalOutcome = run_goal(&GoalRequest {
        repository: &bench.repository(),
        goal: GOAL,
        base: "HEAD",
        state_directory: &bench.state(),
        write_set: &write_set,
        role: kaisa_domain::Role::Developer,
        launch: &launch,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        egress_hosts: &[],
        agent_writable: &[],
        local_model_port: None,
        observed: false,
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("a crashing agent is an outcome, not an error");
    let attempt_directory: String = bench
        .state()
        .join("attempts")
        .join(outcome.attempt.as_str())
        .to_string_lossy()
        .into_owned();
    let deadline = std::time::Instant::now() + BRIDGE_EXIT_GRACE;
    while a_process_still_serves(&attempt_directory) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let preserved: String = git(
        &bench.repository(),
        &["ls-tree", "-r", "--name-only", &outcome.branch],
    );

    assert!(
        outcome.exit != Some(ProcessExit::Code(0)),
        "the crash is recorded as a failed run: {outcome:?}"
    );
    assert!(
        preserved.contains(NOTES),
        "the change made before the crash is preserved on the attempt's branch"
    );
    assert!(
        !a_process_still_serves(&attempt_directory),
        "the crashed agent's bridge does not outlive it"
    );
}

/// A passing trusted command's process exit.
const SUCCESSFUL_CHECK_EXIT: i32 = 0;
/// An approving fake review reports no findings.
const NO_REVIEW_FINDINGS: usize = 0;
/// The initial candidate exists before any revisions.
const ORIGINAL_CANDIDATES: usize = 1;

/// Runs the actual CLI, with a trusted file check and an independent fake reviewer.
fn a_verified_cli_session(bench: &Workbench, review_requires: &str) -> std::process::Output {
    a_verified_cli_session_in_state(bench, review_requires, &bench.state())
}

fn a_verified_cli_session_in_state(
    bench: &Workbench,
    review_requires: &str,
    state: &Path,
) -> std::process::Output {
    let (program, flag, check) = if cfg!(windows) {
        (
            "cmd",
            "/c",
            format!(
                "if exist {} (exit 0) else (exit 1)",
                NOTES.replace('/', "\\")
            ),
        )
    } else {
        ("sh", "-c", format!("test -f {NOTES}"))
    };
    Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args([
            "--agent-program",
            &the_fake_agent().to_string_lossy(),
            "--agent-arg",
            "--fake-write",
            "--agent-arg",
            NOTES,
            "--reviewer-arg",
            "--fake-review-require",
            "--reviewer-arg",
            review_requires,
            "--check-program",
            program,
            "--check-arg",
            flag,
            "--check-arg",
            &check,
            GOAL,
        ])
        .current_dir(bench.repository())
        .env("KAISA_STATE_DIR", state)
        .output()
        .expect("the CLI runs")
}

/// Reads the real process's hash-chained history, verifying it before inspecting receipts.
fn verified_history(bench: &Workbench) -> Vec<kaisa_domain::RecordedEvent> {
    let ledger =
        kaisa_ledger::EventStore::open(&bench.state().join("kaisa.db")).expect("the ledger opens");
    ledger
        .verify()
        .expect("verification receipts preserve the hash chain");
    ledger
        .read_from(kaisa_domain::Sequence::FIRST)
        .expect("readable")
        .into_iter()
        .map(|chained| chained.recorded)
        .collect()
}

#[test]
fn a_second_session_needs_no_re_explanation() {
    use kaisa_domain::EventKind;
    let bench = Workbench::new("second-session");
    let first = a_verified_cli_session(&bench, NOTES);
    assert!(
        first.status.success(),
        "the first CLI run delivers: {first:?}"
    );
    let history = verified_history(&bench);
    let promoted = history
        .iter()
        .find(|recorded| recorded.event.kind == EventKind::KnowledgePromoted)
        .expect("the delivery path must teach a lesson without a direct learning call");
    let taught: serde_json::Value = serde_json::from_str(&promoted.event.payload).expect("JSON");
    let lesson = taught["text"].as_str().expect("lesson text");
    let before_second = history.last().expect("recorded run").sequence;
    assert!(
        lesson.contains(GOAL)
            && lesson.contains(NOTES)
            && !lesson.contains("test -f")
            && matches!(
                taught["provenance"].as_str(),
                Some("confined" | "reduced" | "observed")
            ),
        "the lesson describes the verified goal and observed changed paths: {taught}"
    );

    let second = a_verified_cli_session(&bench, NOTES);
    assert!(
        second.status.success(),
        "the second CLI run delivers: {second:?}"
    );
    let ledger = kaisa_ledger::EventStore::open(&bench.state().join("kaisa.db")).expect("reopens");
    let history = verified_history(&bench);
    let packed = history
        .iter()
        .find(|recorded| {
            recorded.sequence > before_second
                && recorded.event.kind == EventKind::ContextPackRecorded
        })
        .expect("second session pack");
    let payload: serde_json::Value = serde_json::from_str(&packed.event.payload).expect("JSON");
    let digest = kaisa_domain::ContentDigest::try_new(payload["digest"].as_str().expect("digest"))
        .expect("valid digest");
    let pack = ledger
        .context_pack(&digest)
        .expect("readable")
        .expect("stored pack");
    let attempt = kaisa_domain::AttemptId::try_new(&packed.event.subject).expect("attempt");
    assert!(
        String::from_utf8_lossy(&pack).contains(&lesson.replace('\n', " ")),
        "the second CLI process starts from the first delivery's lesson"
    );
    assert_eq!(
        ledger.injections_for(&attempt).expect("readable"),
        vec![kaisa_domain::ContentDigest::try_new(&promoted.event.subject).expect("lesson digest")],
        "the lesson in the second pack has an accountable knowledge.injected event"
    );

    let receipts: Vec<_> = history
        .iter()
        .filter(|recorded| {
            recorded.sequence < promoted.sequence
                && matches!(
                    recorded.event.kind,
                    EventKind::CandidateFrozen
                        | EventKind::CheckEvidence
                        | EventKind::ReviewVerdict
                        | EventKind::GateDecided
                )
        })
        .collect();
    assert_eq!(
        receipts
            .iter()
            .map(|recorded| recorded.event.kind)
            .collect::<Vec<_>>(),
        vec![
            EventKind::CandidateFrozen,
            EventKind::CheckEvidence,
            EventKind::ReviewVerdict,
            EventKind::GateDecided
        ],
        "freeze, check, review and delivery receipts must precede lesson promotion in execution order"
    );
    for recorded in &receipts {
        let receipt: serde_json::Value =
            serde_json::from_str(&recorded.event.payload).expect("JSON");
        assert_eq!(
            receipt["candidate_tree"], taught["source_tree"],
            "every receipt and the learned lesson must refer to the same frozen content"
        );
        assert!(
            receipt["run_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("run_"))
                && receipt["mission_id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("mis_"))
                && receipt["repository"] == taught["repository"],
            "every verification receipt names its run, mission and repository: {receipt}"
        );
        assert!(
            history
                .iter()
                .any(|event| event.event.kind == EventKind::RunStarted
                    && Some(event.event.subject.as_str()) == receipt["run_id"].as_str())
                && history
                    .iter()
                    .any(|event| event.event.kind == EventKind::MissionCreated
                        && Some(event.event.subject.as_str()) == receipt["mission_id"].as_str()),
            "receipt ownership resolves to recorded runs and missions"
        );
        match recorded.event.kind {
            EventKind::CandidateFrozen => {
                let commit = receipt["commit"].as_str().expect("commit");
                assert_eq!(
                    git(
                        &bench.repository(),
                        &["rev-parse", &format!("{commit}^{{tree}}")]
                    )
                    .trim(),
                    taught["source_tree"].as_str().expect("tree"),
                    "the frozen commit really names the receipt's tree"
                );
            }
            EventKind::CheckEvidence => {
                assert!(
                    receipt["command"]
                        .as_array()
                        .is_some_and(|command| !command.is_empty())
                        && receipt["exit"]["code"] == serde_json::json!(SUCCESSFUL_CHECK_EXIT),
                    "the evidence records the actual passing command and exit: {receipt}"
                );
            }
            EventKind::ReviewVerdict => {
                assert!(
                    receipt["approve"] == true
                        && receipt["blocking"] == false
                        && receipt["finding_count"] == serde_json::json!(NO_REVIEW_FINDINGS)
                        && receipt["reviewer_attempt"].as_str()
                            == Some(recorded.event.subject.as_str()),
                    "approval records who reviewed and what they found: {receipt}"
                );
            }
            EventKind::GateDecided => assert!(
                receipt["decision"] == "Deliver"
                    && receipt["reason"]
                        .as_str()
                        .is_some_and(|reason| !reason.is_empty()),
                "the durable gate explains why the candidate was delivered: {receipt}"
            ),
            _ => unreachable!("filtered receipt kinds"),
        }
    }
}

/// The documented failure exit for exhausted revisions.
const REVISIONS_EXHAUSTED: i32 = 4;
/// One revision after the original developer attempt.
const ONE_REVISION: usize = 1;
/// A path the developer never creates, so every independent review blocks.
const MISSING_REVIEW_REQUIREMENT: &str = "src/missing.md";

#[test]
fn exhausted_revisions_record_the_loop_and_exit_four_without_teaching_a_lesson() {
    use kaisa_domain::EventKind;
    let bench = Workbench::new("exhausted-revisions");
    let policy = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args(["policy", "set", "revision_limit", &ONE_REVISION.to_string()])
        .env("KAISA_STATE_DIR", bench.state())
        .output()
        .expect("policy runs");
    assert!(
        policy.status.success(),
        "the revision limit is set: {policy:?}"
    );
    let run = a_verified_cli_session(&bench, MISSING_REVIEW_REQUIREMENT);
    assert_eq!(
        run.status.code(),
        Some(REVISIONS_EXHAUSTED),
        "README and CI contracts reserve exit 3 for a human decision: {run:?}"
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains(&format!("{MISSING_REVIEW_REQUIREMENT} is missing"))
            && stdout.contains("Next: run kaisa again")
            && stdout.contains("preserved branches"),
        "exhaustion prints the remaining finding and actions available in M1: {stdout}"
    );
    let history = verified_history(&bench);
    let revisions: Vec<_> = history
        .iter()
        .filter(|event| event.event.kind == EventKind::RevisionStarted)
        .collect();
    assert_eq!(
        revisions.len(),
        ONE_REVISION,
        "only the permitted revision starts"
    );
    assert!(
        !history
            .iter()
            .any(|event| event.event.kind == EventKind::KnowledgePromoted),
        "undelivered candidates never become lessons"
    );
    let gates: Vec<serde_json::Value> = history
        .iter()
        .filter(|event| event.event.kind == EventKind::GateDecided)
        .map(|event| serde_json::from_str(&event.event.payload).expect("JSON"))
        .collect();
    assert_eq!(
        gates.len(),
        ONE_REVISION + ORIGINAL_CANDIDATES,
        "the original and revised candidates each get a durable gate decision"
    );
    assert!(
        gates
            .first()
            .is_some_and(|gate| gate["decision"] == "revision")
            && gates
                .last()
                .is_some_and(|gate| gate["decision"] == "revision_exhausted"),
        "blocking reviews remain undelivered, including at the revision limit: {gates:?}"
    );
    assert!(
        history
            .iter()
            .filter(|event| event.event.kind == EventKind::ReviewVerdict)
            .all(|event| {
                let verdict: serde_json::Value =
                    serde_json::from_str(&event.event.payload).expect("JSON");
                verdict["approve"] == false && verdict["blocking"] == true
            }),
        "blocking review verdicts are retained for both candidates"
    );
}

#[test]
fn an_attempt_interrupted_before_its_agent_started_is_tidied_on_the_next_run() {
    let bench = Workbench::new("never-started");
    let state: PathBuf = bench.state();
    std::fs::create_dir_all(state.join("worktrees")).expect("writable");
    let attempt = "att_interrupted";
    let mut ledger =
        kaisa_ledger::EventStore::open(&state.join("kaisa.db")).expect("the ledger opens");
    ledger
        .append(kaisa_domain::Event {
            kind: kaisa_domain::EventKind::AttemptAdmitted,
            subject: attempt.to_owned(),
            payload_version: 1,
            payload: r#"{"task":"task_login"}"#.to_owned(),
        })
        .expect("recorded");
    drop(ledger);
    // Killed right after the worktree was made, before `starting` was recorded.
    kaisa_workspace::create_worktree(
        &bench.repository(),
        "HEAD",
        &format!("kaisa/{attempt}"),
        &state.join("worktrees").join(attempt),
    )
    .expect("the worktree is created");

    let recovered =
        kaisa_daemon::recover_abandoned(&bench.repository(), &state).expect("recovery runs");

    assert!(
        matches!(
            recovered.as_slice(),
            [kaisa_daemon::Recovered::Preserved { started: false, .. }]
        ),
        "an attempt whose agent never started is tidied and reported as such: {recovered:?}"
    );
    assert!(
        !state.join("worktrees").join(attempt).exists(),
        "its worktree does not outlive the interrupted run"
    );
}

/// What the scripted local model writes, through the broker.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const LOCAL_MODEL_NOTES: &str = "written by a local model";

/// A loopback OpenAI-compatible server that answers each request with the next scripted reply.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn a_scripted_local_model(replies: Vec<String>) -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("loopback works");
    let port: u16 = listener.local_addr().expect("bound").port();
    std::thread::spawn(move || {
        for reply in replies {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request: Vec<u8> = Vec::new();
            let mut buffer = [0_u8; 4096];
            while let Ok(read) = stream.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request);
                if let Some(end) = text.find("\r\n\r\n") {
                    let length: usize = text[..end]
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + "\r\n\r\n".len() + length {
                        break;
                    }
                }
            }
            let body = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": reply}}]})
                .to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_local_model_drives_a_confined_attempt_through_the_broker() {
    let bench = Workbench::new("local-model");
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let port: u16 = a_scripted_local_model(vec![
        serde_json::json!({"tool": "fs_write", "arguments": {"path": NOTES, "content": LOCAL_MODEL_NOTES}}).to_string(),
        serde_json::json!({"done": "wrote the notes"}).to_string(),
    ]);
    let endpoint: String = format!("http://127.0.0.1:{port}/v1");
    let launch = |mcp_config: &Path| ProcessSpec {
        program: PathBuf::from(env!("CARGO_BIN_EXE_kaisa")),
        args: vec![
            "local-agent".to_owned(),
            "--mcp-config".to_owned(),
            mcp_config.to_string_lossy().into_owned(),
            "--endpoint".to_owned(),
            endpoint.clone(),
            "--model".to_owned(),
            "tiny-local".to_owned(),
        ],
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    };

    let outcome: GoalOutcome = run_goal(&GoalRequest {
        repository: &bench.repository(),
        goal: GOAL,
        base: "HEAD",
        state_directory: &bench.state(),
        write_set: &write_set,
        role: kaisa_domain::Role::Developer,
        launch: &launch,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        egress_hosts: &[],
        agent_writable: &[],
        local_model_port: Some(port),
        observed: false,
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("the goal runs");
    let preserved: String = git(
        &bench.repository(),
        &["show", &format!("{}:{NOTES}", outcome.branch)],
    );

    assert!(
        outcome.confined && outcome.exit == Some(ProcessExit::Code(0)),
        "the local model finished inside the sandbox: {outcome:?}"
    );
    assert_eq!(
        preserved, LOCAL_MODEL_NOTES,
        "the model's change reached the branch only through the broker"
    );
}

#[test]
fn an_observed_agent_writes_directly_and_is_reported_as_observed() {
    let bench = Workbench::new("observed");
    let write_set = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let launch = |_mcp_config: &Path| ProcessSpec {
        program: fake_agent.clone(),
        args: vec!["--fake-observed-write".to_owned(), NOTES.to_owned()],
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    };

    let outcome: GoalOutcome = run_goal(&GoalRequest {
        repository: &bench.repository(),
        goal: GOAL,
        base: "HEAD",
        state_directory: &bench.state(),
        write_set: &write_set,
        role: kaisa_domain::Role::Developer,
        launch: &launch,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        egress_hosts: &[],
        agent_writable: &[],
        local_model_port: None,
        observed: true,
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("the goal runs");
    let files: String = git(
        &bench.repository(),
        &["ls-tree", "-r", "--name-only", &outcome.branch],
    );

    assert_eq!(
        outcome.exit,
        Some(ProcessExit::Code(0)),
        "the observed agent read the plain pack and finished: {outcome:?}"
    );
    assert!(
        outcome.observed && !outcome.confined,
        "an observed agent runs under its own sandbox, never Kaisa's, even when confinement was asked"
    );
    assert!(
        files.contains(NOTES),
        "the observed agent's direct write was preserved on the attempt branch: {files:?}"
    );
    assert!(
        outcome.summary().contains("observed"),
        "the summary tells the person the run was observed: {}",
        outcome.summary()
    );
}

#[test]
fn a_lesson_from_repository_a_never_enters_repository_bs_pack() {
    use kaisa_domain::EventKind;
    let repository_a = Workbench::new("cross-repository-a");
    let repository_b = Workbench::new("cross-repository-b");
    let first = a_verified_cli_session(&repository_a, NOTES);
    assert!(first.status.success(), "repository A delivers: {first:?}");
    let original_history = verified_history(&repository_a);
    let learned = original_history
        .iter()
        .find(|event| event.event.kind == EventKind::KnowledgePromoted)
        .expect("A taught a lesson");
    let before_b = original_history.last().expect("A history").sequence;
    let second = a_verified_cli_session_in_state(&repository_b, NOTES, &repository_a.state());
    assert!(second.status.success(), "repository B delivers: {second:?}");
    let ledger =
        kaisa_ledger::EventStore::open(&repository_a.state().join("kaisa.db")).expect("B ledger");
    let history = verified_history(&repository_a);
    for packed in history.iter().filter(|event| {
        event.sequence > before_b && event.event.kind == EventKind::ContextPackRecorded
    }) {
        let attempt = kaisa_domain::AttemptId::try_new(&packed.event.subject).expect("attempt");
        assert!(
            !ledger
                .injections_for(&attempt)
                .expect("injections")
                .iter()
                .any(|digest| digest.as_str() == learned.event.subject),
            "neither B developer nor reviewer receives A's lesson"
        );
        let payload: serde_json::Value =
            serde_json::from_str(&packed.event.payload).expect("payload");
        let digest =
            kaisa_domain::ContentDigest::try_new(payload["digest"].as_str().expect("digest"))
                .expect("pack digest");
        let pack = ledger.context_pack(&digest).expect("pack").expect("stored");
        assert!(
            !String::from_utf8_lossy(&pack).contains("goal_summary"),
            "the actual B context pack has no A observation text"
        );
    }
}
