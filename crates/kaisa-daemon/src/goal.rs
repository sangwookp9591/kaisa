//! Running one goal end to end in M0: one mission, one run, one task, one attempt
//! (architecture §5.1–§5.9, PRD FR-12 and FR-23).
//!
//! The steps are the ones the daemon will perform for every attempt: record the mission, derive
//! the project profile, assemble and record the context pack, create the worktree, start the
//! agent on its pack with only its MCP bridge, record how it ended, then preserve its work and
//! reclaim the worktree.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kaisa_agents::first_message;
use kaisa_context::{ContextPack, PackInputs, ProjectProfile, derive_profile};
use kaisa_domain::{
    Admission, AttemptId, AttemptState, Cleanup, ContainmentAgentKind, ContainmentCapability,
    ContentDigest, DefaultReducedPolicy, Event, EventKind, Execution, ExitStatus, MissionId,
    ReducedModeBasis, RepoPath, Role, RunId, TaskId, WriteSet, admit_agent_on, admit_on, codes,
};
use kaisa_keeper::{ExitRecord, write_exit_record};
use kaisa_ledger::EventStore;
use kaisa_mcp::BridgeConfig;
use kaisa_supervisor::{Reclamation, reclaim};
use kaisa_sys::{CapturedRun, ProcessExit, ProcessIdentity, ProcessSpec, run_captured_reporting};
use kaisa_workspace::{IgnoredPolicy, Worktree, create_worktree};
use serde_json::json;

use crate::ids::{fresh_suffix, now_ms};
#[cfg(unix)]
use crate::sandbox::agent_sandbox;
use crate::sandbox::{AgentSandbox, beyond_admitted, expected_missing};

/// Payload schema version of the events this module writes.
const PAYLOAD_VERSION: u16 = 1;
/// The first start of an attempt.
const FIRST_INCARNATION: u64 = 1;
/// How long one agent run may take in M0.
const AGENT_DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);
/// How much of the agent's output is kept as its transcript, in bytes.
const TRANSCRIPT_CAP: usize = 4 * 1024 * 1024;
/// Build output a worktree may contain without being preserved.
pub(crate) const BUILD_OUTPUT: [&str; 3] = ["target", "node_modules", ".venv"];

/// Builds the agent's command from the path of its MCP config.
pub type AgentLauncher<'launcher> = &'launcher dyn Fn(&Path) -> ProcessSpec;

/// What to run.
#[derive(Clone, Copy)]
pub struct GoalRequest<'request> {
    /// The repository the goal is about.
    pub repository: &'request Path,
    /// What the user asked for.
    pub goal: &'request str,
    /// The commit the attempt starts from: `HEAD` for a lone goal, a dependency's preserved
    /// commit for a task of a mission.
    pub base: &'request str,
    /// Kaisa's state directory (ledger, attempts, worktrees).
    pub state_directory: &'request Path,
    /// The paths the attempt may change.
    pub write_set: &'request WriteSet,
    /// Who does the work: it decides the attempt's tools and sandbox.
    pub role: Role,
    /// How to start the agent.
    pub launch: AgentLauncher<'request>,
    /// The `kaisa` executable the agent's MCP config runs as its bridge.
    pub kaisa_executable: &'request Path,
    /// Whether the agent's own process runs under the OS sandbox; a run without it is reported
    /// as reduced.
    pub confine_agent: bool,
    /// Hosts the agent may reach, only through its egress broker (for example its model
    /// provider). Empty means no network at all.
    pub egress_hosts: &'request [&'request str],
    /// Paths outside the worktree the agent must be able to change (for example its own
    /// session files), besides its scratch directory.
    pub agent_writable: &'request [PathBuf],
    /// The loopback port of a local model server the agent talks to (tier 0, T28b); the
    /// sandbox allows exactly that port when no egress broker is in use.
    pub local_model_port: Option<u16>,
    /// An observed agent (Codex, T32c): it reads the pack as plain text and writes straight
    /// into its worktree under its own sandbox, so Kaisa does not confine it and checks its
    /// changes against the write set afterwards instead of on each write.
    pub observed: bool,
    /// What may admit the attempt if its sandbox cannot give full containment (D02): a
    /// person's scoped, expiring Decision or a default policy fixed in code. An observed
    /// attempt is always admitted on the observed default policy (T32c).
    pub reduced_mode: &'request ReducedModeBasis,
}

/// The file in an attempt directory naming the agent process Kaisa started, so that after a
/// crash Kaisa can tell whether that same process still runs.
pub const AGENT_IDENTITY_FILE: &str = "agent.identity";

/// Durably writes the agent's identity before it is given any work. An agent whose identity
/// cannot be told (an OS without support) is recorded as unknown, which recovery treats as
/// lost track rather than as gone.
fn record_identity(path: &Path, identity: Option<ProcessIdentity>) -> std::io::Result<()> {
    let line: String = identity.map_or_else(|| "unknown".to_owned(), |known| known.to_line());
    let mut file = std::fs::File::create(path)?;
    std::io::Write::write_all(&mut file, line.as_bytes())?;
    file.sync_all()
}

/// Variables that point an agent's HTTP clients at its egress broker.
const PROXY_VARIABLES: [&str; 4] = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"];

/// What came of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalOutcome {
    /// The attempt that ran.
    pub attempt: AttemptId,
    /// How the agent ended; `None` if it ran past its deadline.
    pub exit: Option<ProcessExit>,
    /// True if the saved transcript lost output at the capture limit.
    pub truncated: bool,
    /// The branch holding the preserved work.
    pub branch: String,
    /// The WIP commit with the work, if there was any to preserve.
    pub commit: Option<String>,
    /// Paths that could not be preserved (for example, files that look like credentials); the
    /// worktree was kept because of them.
    pub kept_back: Vec<String>,
    /// The digest of the context pack the agent started from.
    pub context_pack: String,
    /// False if the agent's process ran without full OS confinement (a reduced run): true only
    /// when `missing` is empty.
    pub confined: bool,
    /// What the agent's sandbox could not guarantee; empty for a fully contained run.
    pub missing: Vec<ContainmentCapability>,
    /// What admitted the run although `missing` is not empty.
    pub admitted_on: ReducedModeBasis,
    /// True if the agent was observed rather than brokered.
    pub observed: bool,
}

impl GoalOutcome {
    /// The one line `kaisa "<goal>"` prints.
    #[must_use]
    pub fn summary(&self) -> String {
        let ending: String = match self.exit {
            Some(ProcessExit::Code(code)) => format!("exited with code {code}"),
            Some(ProcessExit::Terminated) => "was ended".to_owned(),
            None => "ran past its deadline and was ended".to_owned(),
        };
        let work: String = match (&self.commit, self.kept_back.is_empty()) {
            (Some(commit), true) => {
                format!("work preserved on {} at {}", self.branch, short(commit))
            }
            (None, true) => "no changes to preserve".to_owned(),
            (_, false) => format!(
                "worktree kept: {} could not be preserved automatically",
                self.kept_back.join(", ")
            ),
        };
        let reduced: String = if self.confined {
            String::new()
        } else {
            let how: &str = if self.missing.len() == ContainmentCapability::ALL.len() {
                "without OS confinement"
            } else {
                "without full OS confinement"
            };
            format!(
                "; reduced: the agent process ran {how} (missing {}; admitted on {})",
                codes(&self.missing),
                self.admitted_on
            )
        };
        let observed: &str = if self.observed {
            "; observed: the agent wrote under its own sandbox, so its changes were checked afterwards, not on each write"
        } else {
            ""
        };
        format!(
            "{} {ending}; {work}; context pack {}{reduced}{observed}",
            self.attempt,
            short(&self.context_pack)
        )
    }
}

fn short(text: &str) -> &str {
    text.get(..12).unwrap_or(text)
}

/// Which step failed, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalError {
    /// The step that failed.
    pub step: &'static str,
    /// What went wrong.
    pub detail: String,
}

impl fmt::Display for GoalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed: {}", self.step, self.detail)
    }
}

impl std::error::Error for GoalError {}

/// The step named when an attempt is not admitted because its containment falls short.
const ADMISSION_STEP: &str = "admitting the attempt";

impl GoalError {
    /// True when the attempt never started because it lacked containment that no Decision or
    /// default policy accepts (D02): a person has to decide, so `kaisa` exits 3.
    #[must_use]
    pub fn not_admitted(&self) -> bool {
        self.step == ADMISSION_STEP
    }

    fn refused(missing: &[ContainmentCapability], basis: &ReducedModeBasis) -> Self {
        Self {
            step: ADMISSION_STEP,
            detail: format!(
                "the agent would run without {}; {basis} does not accept that (needs: {})",
                missing
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                codes(missing)
            ),
        }
    }
}

pub(crate) fn at<E: fmt::Display>(step: &'static str) -> impl Fn(E) -> GoalError {
    move |error| GoalError {
        step,
        detail: error.to_string(),
    }
}

fn record(
    ledger: &mut EventStore,
    kind: EventKind,
    subject: &str,
    payload: serde_json::Value,
) -> Result<(), GoalError> {
    ledger
        .append(Event {
            kind,
            subject: subject.to_owned(),
            payload_version: PAYLOAD_VERSION,
            payload: payload.to_string(),
        })
        .map(|_| ())
        .map_err(at("recording an event"))
}

pub(crate) fn record_execution(
    ledger: &mut EventStore,
    attempt: &AttemptId,
    execution: Execution,
) -> Result<(), GoalError> {
    record(
        ledger,
        EventKind::AttemptStateChanged,
        attempt.as_str(),
        json!({"execution": execution.code()}),
    )
}

pub(crate) fn record_cleanup(
    ledger: &mut EventStore,
    attempt: &AttemptId,
    cleanup: Cleanup,
) -> Result<(), GoalError> {
    record(
        ledger,
        EventKind::AttemptStateChanged,
        attempt.as_str(),
        json!({"cleanup": cleanup.code()}),
    )
}

/// A Decision's first observation survives reviewer/revision dispatches in this daemon.
/// The wall clock can shorten this budget, never extend its monotonic deadline.
fn decision_budget(basis: &ReducedModeBasis) -> Option<Duration> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CLOCKS: OnceLock<Mutex<HashMap<u64, (u64, Instant)>>> = OnceLock::new();
    let ReducedModeBasis::Decision(decision) = basis else {
        return Some(AGENT_DEADLINE);
    };
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|time| u64::try_from(time.as_millis()).ok())?;
    let mut clocks = CLOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()?;
    let (started, instant) = clocks
        .entry(decision.expires_at_ms)
        .or_insert_with(|| (wall, Instant::now()));
    let elapsed = u64::try_from(instant.elapsed().as_millis()).ok()?;
    decision
        .remaining_ms(*started, wall, elapsed)
        .map(|ms| Duration::from_millis(ms).min(AGENT_DEADLINE))
}

fn executable_kind(request: &GoalRequest<'_>, agent: &ProcessSpec) -> ContainmentAgentKind {
    if request.observed {
        ContainmentAgentKind::Observed
    } else if request.local_model_port.is_some() {
        ContainmentAgentKind::LocalModel
    } else if agent
        .program
        .file_stem()
        .is_some_and(|name| name == "claude")
    {
        ContainmentAgentKind::Claude
    } else {
        ContainmentAgentKind::Other
    }
}

/// Runs `request` to a preserved result.
///
/// # Errors
///
/// Returns [`GoalError`] naming the step that failed; everything recorded until then stays in
/// the ledger, and nothing is deleted that was not preserved.
pub fn run_goal(request: &GoalRequest<'_>) -> Result<GoalOutcome, GoalError> {
    let mut reduced_notice = None;
    run_goal_inner(request, &mut reduced_notice).map_err(|mut error| {
        if let Some(notice) = reduced_notice {
            error.detail = format!("{notice}\n{}", error.detail);
        }
        error
    })
}

fn run_goal_inner(
    request: &GoalRequest<'_>,
    reduced_notice: &mut Option<String>,
) -> Result<GoalOutcome, GoalError> {
    // An observed agent writes directly, which Kaisa's confinement would forbid.
    let confine_agent: bool = request.confine_agent && !request.observed;
    kaisa_sys::ensure_private_directory(request.state_directory)
        .map_err(at("preparing the state directory"))?;
    let mut ledger: EventStore = EventStore::open(&request.state_directory.join("kaisa.db"))
        .map_err(at("opening the ledger"))?;
    // Admission (D02): what this attempt's sandbox cannot give must be accepted by a scoped,
    // unexpired Decision or a default policy before anything starts.
    let observed_policy =
        ReducedModeBasis::DefaultPolicy(DefaultReducedPolicy::ObservedCheckedAfterwards);
    let basis: &ReducedModeBasis = if request.observed {
        &observed_policy
    } else {
        request.reduced_mode
    };
    let admission: Admission = admit_on(&expected_missing(confine_agent), basis, now_ms());
    if decision_budget(basis).is_none() {
        return Err(GoalError::refused(admission.missing(), basis));
    }
    if let Admission::Refused { missing } = &admission {
        return Err(GoalError::refused(missing, basis));
    }
    let suffix: String = fresh_suffix();
    let id = |prefix: &str| format!("{prefix}_{suffix}");
    let mission = MissionId::try_new(&id(MissionId::PREFIX)).map_err(at("naming the mission"))?;
    let run = RunId::try_new(&id(RunId::PREFIX)).map_err(at("naming the run"))?;
    let task = TaskId::try_new(&id(TaskId::PREFIX)).map_err(at("naming the task"))?;
    let attempt = AttemptId::try_new(&id(AttemptId::PREFIX)).map_err(at("naming the attempt"))?;

    record(
        &mut ledger,
        EventKind::MissionCreated,
        mission.as_str(),
        json!({"goal": request.goal}),
    )?;
    record(
        &mut ledger,
        EventKind::RunStarted,
        run.as_str(),
        json!({"mission": mission.as_str()}),
    )?;
    record(
        &mut ledger,
        EventKind::AttemptAdmitted,
        attempt.as_str(),
        json!({
            "task": task.as_str(),
            "containment": {
                "admission": admission.code(),
                "missing": admission.missing().iter().map(|capability| capability.code()).collect::<Vec<_>>(),
                "basis": matches!(admission, Admission::Reduced { .. }).then(|| basis.code()),
                "expires_at_ms": matches!(admission, Admission::Reduced { .. }).then(|| basis.expires_at_ms()).flatten(),
            },
        }),
    )?;

    let profile: ProjectProfile =
        derive_profile(request.repository).map_err(at("deriving the project profile"))?;
    let recalled: Vec<(ContentDigest, String)> =
        crate::recall::relevant_lessons(&ledger, request.goal, request.write_set)
            .map_err(at("recalling lessons"))?;
    let lesson_texts: Vec<String> = recalled.iter().map(|(_, text)| text.clone()).collect();
    let pack: ContextPack = ContextPack::assemble(PackInputs {
        spec: request.goal,
        acceptance: &[],
        write_set: request.write_set,
        profile: &profile,
        findings: &[],
        impact: &[],
        diff: "",
        lessons: &lesson_texts,
    })
    .fit(kaisa_context::budget_for(request.role))
    .map_err(at("fitting the context pack"))?;
    let digest = pack.digest();
    ledger
        .record_context_pack(&attempt, &digest, &pack.canonical_bytes())
        .map_err(at("recording the context pack"))?;
    // Only lessons that survived the budget were given; only those are on the record (T26a).
    if !pack.cut().contains(&kaisa_context::SectionKind::Lessons) {
        let given: Vec<ContentDigest> = recalled.into_iter().map(|(digest, _)| digest).collect();
        ledger
            .record_injection(&attempt, &given)
            .map_err(at("recording injected lessons"))?;
    }

    let branch: String = format!("kaisa/{attempt}");
    let worktree_path: PathBuf = request
        .state_directory
        .join("worktrees")
        .join(attempt.as_str());
    std::fs::create_dir_all(request.state_directory.join("worktrees"))
        .map_err(at("preparing worktrees"))?;
    let worktree: Worktree =
        create_worktree(request.repository, request.base, &branch, &worktree_path)
            .map_err(at("creating the worktree"))?;
    let attempt_directory: PathBuf = request
        .state_directory
        .join("attempts")
        .join(attempt.as_str());
    std::fs::create_dir_all(&attempt_directory).map_err(at("preparing the attempt directory"))?;
    BridgeConfig {
        worktree: worktree.path().to_owned(),
        write_set: request.write_set.clone(),
        role: request.role,
        scratch: None,
        broker_port: None,
    }
    .write_to(&attempt_directory)
    .map_err(at("writing the bridge config"))?;
    kaisa_mcp::AttemptToken::issue(&attempt_directory).map_err(at("issuing the attempt token"))?;
    // On Windows the container exists before the broker, whose pipes must admit it.
    #[cfg(windows)]
    let mut contained: Option<crate::sandbox::ContainedAttempt> = if confine_agent {
        Some(
            crate::sandbox::ContainedAttempt::create(
                &attempt_directory,
                &format!("kaisa.{attempt}"),
            )
            .map_err(at("creating the agent's container"))?,
        )
    } else {
        None
    };
    // The broker runs here, outside the agent's sandbox; the agent's bridge only relays to it.
    #[cfg(any(unix, windows))]
    let broker = {
        let endpoint = kaisa_mcp::BrokerEndpoint::bind(&attempt_directory)
            .map_err(at("binding the attempt's broker"))?;
        let socket: PathBuf = endpoint.socket().to_owned();
        (std::thread::spawn(move || endpoint.serve_one()), socket)
    };
    let scratch: PathBuf = attempt_directory.join("scratch");
    std::fs::create_dir_all(&scratch).map_err(at("preparing the agent's scratch"))?;
    let kaisa: &Path = request.kaisa_executable;
    let mcp_config: PathBuf = attempt_directory.join("mcp.json");
    std::fs::write(
        &mcp_config,
        json!({"mcpServers": {"kaisa": {
            "command": kaisa.to_string_lossy(),
            "args": ["mcp", "--attempt", attempt_directory.to_string_lossy()],
        }}})
        .to_string(),
    )
    .map_err(at("writing the MCP config"))?;

    let mut state: AttemptState = AttemptState::admitted();
    let mut agent: ProcessSpec = (request.launch)(&mcp_config);
    agent.working_directory = worktree.path().to_owned();
    let egress: Option<kaisa_egress::EgressBroker> = if request.egress_hosts.is_empty() {
        None
    } else {
        let broker = kaisa_egress::EgressBroker::start(
            kaisa_egress::AllowList::new(request.egress_hosts),
            kaisa_egress::SystemNetwork,
        )
        .map_err(at("starting the egress broker"))?;
        let proxy: String = format!("http://{}", broker.address());
        agent.env.extend(
            PROXY_VARIABLES
                .iter()
                .map(|name| ((*name).to_owned(), proxy.clone())),
        );
        Some(broker)
    };
    let egress_port: Option<u16> = egress
        .as_ref()
        .map(|broker| broker.address().port())
        .or(request.local_model_port);
    let input: String = if request.observed {
        format!("{}\n", pack.render())
    } else {
        format!("{}\n", first_message(&pack))
    };
    #[cfg(unix)]
    let sandbox = if confine_agent {
        agent_sandbox(&scratch, request.agent_writable, &broker.1, egress_port)
            .map_err(at("building the agent's sandbox"))?
    } else {
        AgentSandbox::Unconfined
    };
    // A container cannot reach loopback (spike check 4a), so the egress broker is out of its
    // reach: on Windows a confined agent has no network.
    #[cfg(windows)]
    let sandbox = {
        let _ = egress_port;
        match contained.take() {
            Some(mut contained) => {
                contained
                    .grant_for_agent(
                        worktree.path(),
                        &attempt_directory,
                        &scratch,
                        &[agent.program.as_path(), request.kaisa_executable],
                    )
                    .map_err(at("granting the agent's paths"))?;
                AgentSandbox::AppContainer(contained)
            }
            None => AgentSandbox::Unconfined,
        }
    };
    #[cfg(not(any(unix, windows)))]
    let sandbox = {
        let _ = egress_port;
        AgentSandbox::Unconfined
    };
    // The sandbox actually built may be weaker than the machine promised at admission; then
    // the agent does not start.
    let missing: Vec<ContainmentCapability> = sandbox.missing();
    let unaccepted: Vec<ContainmentCapability> = beyond_admitted(&missing, admission.missing());
    let spawn_admission =
        admit_agent_on(&missing, basis, executable_kind(request, &agent), now_ms());
    let deadline = decision_budget(basis);
    if !unaccepted.is_empty()
        || matches!(spawn_admission, Admission::Refused { .. })
        || deadline.is_none()
    {
        sandbox.release();
        #[cfg(any(unix, windows))]
        finish_broker(broker)?;
        return Err(GoalError::refused(&missing, basis));
    }
    if !missing.is_empty() {
        *reduced_notice = Some(format!(
            "reduced: missing {}; admitted on {basis}",
            codes(&missing)
        ));
    }
    state.start().map_err(at("starting the attempt"))?;
    record_execution(&mut ledger, &attempt, Execution::Starting)?;
    state.confirm_running().map_err(at("running the attempt"))?;
    record_execution(&mut ledger, &attempt, Execution::Running)?;
    let identity_file: PathBuf = attempt_directory.join(AGENT_IDENTITY_FILE);
    let finished: CapturedRun = run_captured_reporting(
        &agent,
        &sandbox.confinement(),
        input.as_bytes(),
        deadline.expect("the spawn guard checked the Decision budget"),
        TRANSCRIPT_CAP,
        &mut |identity| record_identity(&identity_file, identity),
    )
    .map_err(at("running the agent"))?;
    if finished.exit.is_none() && deadline.is_some_and(|budget| budget < AGENT_DEADLINE) {
        record(
            &mut ledger,
            EventKind::AttemptStateChanged,
            attempt.as_str(),
            json!({"reason": "Decision execution deadline reached", "expires_at_ms": basis.expires_at_ms()}),
        )?;
    }
    std::fs::write(attempt_directory.join("transcript.log"), &finished.output)
        .map_err(at("saving the transcript"))?;
    std::fs::write(
        attempt_directory.join("transcript-meta.json"),
        serde_json::json!({"truncated": finished.truncated}).to_string(),
    )
    .map_err(at("saving transcript metadata"))?;
    if let Some(usage) = kaisa_agents::parse_usage(&String::from_utf8_lossy(&finished.output)) {
        ledger
            .record_usage(
                &attempt,
                &kaisa_ledger::AttemptUsage {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cost_usd: usage.cost_usd,
                },
            )
            .map_err(at("recording usage"))?;
    }
    #[cfg(any(unix, windows))]
    finish_broker(broker)?;
    sandbox.release();

    let status: ExitStatus = match finished.exit {
        Some(ProcessExit::Code(code)) => ExitStatus::Code(code),
        Some(ProcessExit::Terminated) | None => ExitStatus::Terminated,
    };
    write_exit_record(
        &attempt_directory,
        &ExitRecord {
            attempt: attempt.clone(),
            incarnation: FIRST_INCARNATION,
            status,
            finished_at_ms: now_ms(),
        },
    )
    .map_err(at("writing the exit record"))?;
    state
        .record_exit(status)
        .map_err(at("recording the exit"))?;
    record_execution(&mut ledger, &attempt, Execution::Exited(status))?;

    let reclamation: Reclamation = reclaim(
        &mut state,
        request.repository,
        &worktree,
        &IgnoredPolicy::with_build_output(&BUILD_OUTPUT),
    )
    .map_err(at("preserving and reclaiming"))?;
    record_cleanup(&mut ledger, &attempt, state.cleanup())?;
    let (commit, kept_back) = match reclamation {
        Reclamation::Reclaimed(receipt) => (receipt.commit().map(str::to_owned), Vec::new()),
        Reclamation::Blocked { uncovered } => (None, uncovered),
        Reclamation::Retained => (None, Vec::new()),
    };
    Ok(GoalOutcome {
        attempt,
        exit: finished.exit,
        truncated: finished.truncated,
        branch,
        commit,
        kept_back,
        context_pack: digest.as_str().to_owned(),
        confined: missing.is_empty(),
        admitted_on: if missing.is_empty() {
            ReducedModeBasis::None
        } else {
            basis.clone()
        },
        missing,
        observed: request.observed,
    })
}

/// Ends the broker thread: if the agent never connected, a throwaway connection releases it.
#[cfg(any(unix, windows))]
fn finish_broker(
    (broker, socket): (std::thread::JoinHandle<std::io::Result<()>>, PathBuf),
) -> Result<(), GoalError> {
    if !broker.is_finished() {
        #[cfg(unix)]
        let _ = std::os::unix::net::UnixStream::connect(socket);
        #[cfg(windows)]
        {
            // The Windows broker accepts its up pipe, then its down pipe.
            let base: String = socket.to_string_lossy().into_owned();
            let _up = std::fs::OpenOptions::new()
                .write(true)
                .open(format!("{base}-up"));
            let _down = std::fs::OpenOptions::new()
                .read(true)
                .open(format!("{base}-down"));
        }
    }
    broker
        .join()
        .map_err(|_| GoalError {
            step: "serving the broker",
            detail: "the broker thread panicked".to_owned(),
        })?
        .map_err(at("serving the broker"))
}

/// The default M0 write set: every top-level entry of the repository except `.git`.
///
/// # Errors
///
/// Returns the I/O error if the repository cannot be listed.
pub fn whole_repository(repository: &Path) -> std::io::Result<WriteSet> {
    let mut entries: Vec<RepoPath> = Vec::new();
    for entry in std::fs::read_dir(repository)? {
        let name: String = entry?.file_name().to_string_lossy().into_owned();
        if name != ".git"
            && let Ok(path) = RepoPath::try_new(&name)
        {
            entries.push(path);
        }
    }
    entries.sort();
    Ok(WriteSet::new(entries))
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{
        AttemptId, ContainmentCapability, ReducedModeBasis, ReducedModeDecision, landlock_missing,
    };

    use super::GoalOutcome;

    /// A kernel whose Landlock handles writes but neither truncation nor TCP (ABI 2).
    const LANDLOCK_ABI_TWO: i64 = 2;
    /// When the person's Decision ends, in milliseconds.
    const DECISION_ENDS_MS: u64 = 1_793_354_400_000;

    /// An attempt that ran in a sandbox missing `missing`, admitted on a Decision.
    fn an_attempt_missing(missing: Vec<ContainmentCapability>) -> GoalOutcome {
        GoalOutcome {
            attempt: AttemptId::try_new("att_landlock2").expect("a valid attempt id"),
            exit: None,
            branch: "kaisa/att_landlock2".to_owned(),
            commit: None,
            kept_back: Vec::new(),
            context_pack: "c0ffee".to_owned(),
            confined: missing.is_empty(),
            admitted_on: ReducedModeBasis::Decision(ReducedModeDecision {
                accepts_missing: missing.clone(),
                expires_at_ms: DECISION_ENDS_MS,
            }),
            missing,
            observed: false,
            truncated: false,
        }
    }

    #[test]
    fn an_attempt_on_an_old_landlock_abi_is_labelled_reduced_with_what_it_lacks() {
        let outcome = an_attempt_missing(landlock_missing(Some(LANDLOCK_ABI_TWO), true));
        let summary: String = outcome.summary();

        assert!(
            !outcome.confined
                && summary.contains("reduced: the agent process ran without full OS confinement")
                && summary.contains("write,tcp"),
            "a partial Landlock sandbox is never shown as confined: {summary}"
        );
    }
}
