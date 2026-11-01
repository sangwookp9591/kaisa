//! A planned mission end to end: tasks run in dependency order, each starting from the work of
//! the task it depends on (T20d).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_daemon::{MissionOutcome, MissionRequest, run_mission};
use kaisa_domain::{Plan, PlannedTask, RepoPath, Role, TaskId, WriteSet};
use kaisa_sys::ProcessSpec;

/// Where the scripted agents write their step files.
const STEP_PREFIX: &str = "src/step";

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .expect("git is installed on every CI runner");
    assert!(output.status.success(), "git {args:?} failed: {output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn task(id: &str) -> TaskId {
    TaskId::try_new(id).expect("a valid task id")
}

#[test]
fn a_two_task_mission_runs_both_attempts_in_dependency_order() {
    let base: PathBuf = std::env::temp_dir().join(format!("kaisa-mission-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
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
    let src = WriteSet::new(vec![RepoPath::try_new("src").expect("valid")]);
    let plan = Plan::new(vec![
        PlannedTask {
            id: task("task_first"),
            role: Role::Developer,
            write_set: src.clone(),
            depends_on: Vec::new(),
        },
        PlannedTask {
            id: task("task_second"),
            role: Role::Developer,
            write_set: src,
            depends_on: vec![task("task_first")],
        },
    ])
    .expect("a valid plan");
    let instructions: BTreeMap<TaskId, String> = [
        (task("task_first"), "Write the first step.".to_owned()),
        (task("task_second"), "Write the next step.".to_owned()),
    ]
    .into_iter()
    .collect();
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let launch = |mcp_config: &Path| ProcessSpec {
        program: fake_agent.clone(),
        args: vec![
            "--mcp-config".to_owned(),
            mcp_config.to_string_lossy().into_owned(),
            "--fake-write-next".to_owned(),
            STEP_PREFIX.to_owned(),
        ],
        working_directory: std::env::temp_dir(),
        env: Vec::new(),
    };

    let policy = kaisa_domain::Policy::personal_default()
        .snapshot()
        .expect("valid policy");
    let checks = vec![trusted_check("pass")];
    let reviewer = |config: &Path| fake_spec(config, &["--fake-review-require", "src/lib.rs"]);
    let outcome: MissionOutcome = run_mission(&MissionRequest {
        repository: &repository,
        state_directory: &base.join("state"),
        plan: &plan,
        instructions: &instructions,
        launch: &launch,
        reviewer: &reviewer,
        checks: &checks,
        policy: &policy,
        observed: false,
        egress_hosts: &[],
        agent_writable: &[],
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("the mission runs");
    let order: Vec<TaskId> = outcome.ran.iter().map(|(id, _)| id.clone()).collect();
    let last_branch: String = outcome
        .ran
        .last()
        .map(|(_, attempt)| attempt.branch.clone())
        .unwrap_or_default();
    let delivered: String = git(&repository, &["ls-tree", "-r", "--name-only", &last_branch]);
    let _ = std::fs::remove_dir_all(&base);

    assert_eq!(
        (order, outcome.stopped_at),
        (vec![task("task_first"), task("task_second")], None),
        "both tasks ran, the dependency first"
    );
    assert!(
        delivered.contains("src/step-1.md") && delivered.contains("src/step-2.md"),
        "the second task started from the first task's work and added its own: {delivered:?}"
    );
}

/// Fake agents sleep long enough that sequential execution cannot accidentally pass overlap.
const WORKER_SLEEP_MS: &str = "350";
const PARALLEL_SLOTS: usize = 2;
static NEXT_REPOSITORY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct MissionRepository {
    root: PathBuf,
    repository: PathBuf,
    state: PathBuf,
}
impl MissionRepository {
    fn new() -> Self {
        use std::sync::atomic::Ordering;
        let root = std::env::temp_dir().join(format!(
            "kaisa-parallel-{}-{}",
            std::process::id(),
            NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed)
        ));
        let repository = root.join("repo");
        std::fs::create_dir_all(repository.join("src")).expect("temporary repository");
        std::fs::write(repository.join("src/lib.rs"), "pub fn baseline() {}\n")
            .expect("baseline source");
        git(&repository, &["init", "--quiet", "--initial-branch=main"]);
        git(&repository, &["add", "src/lib.rs"]);
        git(
            &repository,
            &[
                "-c",
                "user.name=Kaisa Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "baseline",
            ],
        );
        let state = root.join("state");
        Self {
            root,
            repository,
            state,
        }
    }
    fn run(
        &self,
        plan: &Plan,
        policy: &kaisa_domain::PolicySnapshot,
        launch: kaisa_daemon::ParallelAgentLauncher<'_>,
        checks: &[kaisa_context::DetectedCommand],
    ) -> MissionOutcome {
        let instructions = plan
            .tasks()
            .iter()
            .map(|task| {
                (
                    task.id.clone(),
                    format!("Implement {} within its declared paths.", task.id),
                )
            })
            .collect();
        let reviewer = |config: &Path| fake_spec(config, &["--fake-review-require", "src/lib.rs"]);
        run_mission(&MissionRequest {
            repository: &self.repository,
            state_directory: &self.state,
            plan,
            instructions: &instructions,
            launch,
            reviewer: &reviewer,
            checks,
            policy,
            kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
            confine_agent: true,
            observed: false,
            egress_hosts: &[],
            agent_writable: &[],
            reduced_mode: &kaisa_domain::ReducedModeBasis::None,
        })
        .expect("mission infrastructure runs")
    }
    fn events(&self, kind: kaisa_domain::EventKind) -> Vec<kaisa_domain::Event> {
        kaisa_ledger::EventStore::open_read_only(&self.state.join("kaisa.db"))
            .expect("ledger exists")
            .read_from(kaisa_domain::Sequence::FIRST)
            .expect("events readable")
            .into_iter()
            .map(|event| event.recorded.event)
            .filter(|event| event.kind == kind)
            .collect()
    }
    fn transcript(&self, attempt: &kaisa_daemon::GoalOutcome) -> String {
        std::fs::read_to_string(
            self.state
                .join("attempts")
                .join(attempt.attempt.as_str())
                .join("transcript.log"),
        )
        .expect("saved transcript")
    }
}
impl Drop for MissionRepository {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fake_spec(config: &Path, args: &[&str]) -> ProcessSpec {
    ProcessSpec {
        program: PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
            .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX)),
        args: std::iter::once("--mcp-config".to_owned())
            .chain(std::iter::once(config.to_string_lossy().into_owned()))
            .chain(args.iter().map(|arg| (*arg).to_owned()))
            .collect(),
        working_directory: std::env::temp_dir(),
        env: vec![],
    }
}

fn trusted_check(condition: &str) -> kaisa_context::DetectedCommand {
    let command = match condition {
        "pass" => {
            if cfg!(windows) {
                "exit 0"
            } else {
                "true"
            }
        }
        "fail" => {
            if cfg!(windows) {
                "exit 1"
            } else {
                "false"
            }
        }
        "incompatible pair" => {
            if cfg!(windows) {
                "if exist src\\a.md if exist src\\b.md exit 1"
            } else {
                "! { test -f src/a.md && test -f src/b.md; }"
            }
        }
        _ => panic!("unknown domain check"),
    };
    kaisa_context::DetectedCommand {
        language: kaisa_context::Language::Rust,
        purpose: kaisa_context::Purpose::Test,
        argv: vec![
            if cfg!(windows) {
                "cmd".into()
            } else {
                "sh".into()
            },
            if cfg!(windows) {
                "/C".into()
            } else {
                "-c".into()
            },
            command.into(),
        ],
        directory: None,
        confirmation: kaisa_context::Confirmation::Confirmed,
    }
}

fn a_task(name: &str, path: &str, dependencies: &[&str]) -> PlannedTask {
    PlannedTask {
        id: task(name),
        role: Role::Developer,
        write_set: WriteSet::new(vec![RepoPath::try_new(path).expect("valid path")]),
        depends_on: dependencies.iter().map(|name| task(name)).collect(),
    }
}
fn parallel_policy(failure: kaisa_domain::OnFailure) -> kaisa_domain::PolicySnapshot {
    let mut policy = kaisa_domain::Policy::personal_default();
    policy.max_parallel = PARALLEL_SLOTS;
    policy.on_failure = failure;
    policy.revision_limit = 0;
    policy.snapshot().expect("valid parallel policy")
}
fn declared_target(config: &Path) -> String {
    let bridge: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(config.with_file_name("bridge.json"))
            .expect("bridge configuration"),
    )
    .expect("bridge JSON");
    bridge["write_set"][0]
        .as_str()
        .expect("one owned file")
        .to_owned()
}
fn a_declared_writer(config: &Path) -> ProcessSpec {
    let target = declared_target(config);
    fake_spec(
        config,
        &[
            "--fake-write",
            &target,
            "--fake-content",
            "task output\n",
            "--fake-delay-ms",
            WORKER_SLEEP_MS,
        ],
    )
}
fn worker_interval(transcript: &str) -> (u128, u128) {
    let time = |prefix: &str| {
        transcript
            .lines()
            .find_map(|line| line.strip_prefix(prefix))
            .expect("fake timestamp")
            .parse()
            .expect("numeric timestamp")
    };
    (time("fake-start-ns: "), time("fake-end-ns: "))
}
fn delivered(outcome: &MissionOutcome) -> bool {
    outcome
        .verification
        .as_ref()
        .is_some_and(|verified| matches!(verified.delivery, kaisa_daemon::Delivery::Deliver { .. }))
}

#[test]
fn independent_ready_agents_overlap_but_never_exceed_the_slot_limit() {
    let repository = MissionRepository::new();
    let plan = Plan::new(vec![
        a_task("task_a", "src/a.md", &[]),
        a_task("task_b", "src/b.md", &[]),
        a_task("task_c", "src/c.md", &[]),
    ])
    .expect("disjoint plan");
    let outcome = repository.run(
        &plan,
        &parallel_policy(kaisa_domain::OnFailure::Stop),
        &a_declared_writer,
        &[trusted_check("pass")],
    );
    let intervals: Vec<_> = outcome
        .ran
        .iter()
        .map(|(_, attempt)| worker_interval(&repository.transcript(attempt)))
        .collect();
    assert!(
        intervals[0].0 < intervals[1].1 && intervals[1].0 < intervals[0].1,
        "independent fake agents must actually overlap, not just be marked running: {intervals:?}"
    );
    assert!(
        intervals[PARALLEL_SLOTS].0
            > intervals[..PARALLEL_SLOTS]
                .iter()
                .map(|interval| interval.1)
                .max()
                .expect("first wave"),
        "the next worker waits for a slot and the deterministic wave barrier"
    );
    assert!(
        delivered(&outcome),
        "all task gates and integrated verification passed: {outcome:?}"
    );
}

#[test]
fn a_diamond_dependent_waits_for_both_parents_and_inherits_both_results() {
    let repository = MissionRepository::new();
    let plan = Plan::new(vec![
        a_task("task_a", "src/a.md", &[]),
        a_task("task_b", "src/b.md", &[]),
        a_task("task_join", "src/join.md", &["task_a", "task_b"]),
    ])
    .expect("diamond DAG");
    let outcome = repository.run(
        &plan,
        &parallel_policy(kaisa_domain::OnFailure::Stop),
        &a_declared_writer,
        &[trusted_check("pass")],
    );
    let last = outcome
        .ran
        .last()
        .expect("dependent ran")
        .1
        .commit
        .as_deref()
        .expect("preserved result");
    let tree = git(
        &repository.repository,
        &["ls-tree", "-r", "--name-only", last],
    );
    for parent_file in ["src/a.md", "src/b.md", "src/join.md"] {
        assert!(
            tree.contains(parent_file),
            "dependent base must contain every parent's work: {tree}"
        );
    }
    let finished = repository.events(kaisa_domain::EventKind::TaskFinished);
    let started = repository.events(kaisa_domain::EventKind::TaskStarted);
    let join_time =
        serde_json::from_str::<serde_json::Value>(&started.last().expect("join start").payload)
            .expect("event JSON")["at_ms"]
            .as_u64()
            .expect("time");
    for parent in finished.iter().take(PARALLEL_SLOTS) {
        let finish_time = serde_json::from_str::<serde_json::Value>(&parent.payload)
            .expect("event JSON")["at_ms"]
            .as_u64()
            .expect("time");
        assert!(
            join_time >= finish_time,
            "join starts only after all parent pipelines settle"
        );
    }
    assert!(
        delivered(&outcome),
        "the merged diamond passes the final gate"
    );
}

#[test]
fn integrated_check_failure_blocks_delivery_even_when_each_task_passed_alone() {
    let repository = MissionRepository::new();
    let plan = Plan::new(vec![
        a_task("task_a", "src/a.md", &[]),
        a_task("task_b", "src/b.md", &[]),
    ])
    .expect("disjoint plan");
    let outcome = repository.run(
        &plan,
        &parallel_policy(kaisa_domain::OnFailure::Stop),
        &a_declared_writer,
        &[trusted_check("incompatible pair")],
    );
    assert!(
        outcome.stopped_at.is_none(),
        "both isolated candidates passed their checks and reviews"
    );
    assert!(
        !delivered(&outcome),
        "only the combined tree exposes the integration regression"
    );
    assert!(
        repository
            .events(kaisa_domain::EventKind::MissionVerified)
            .is_empty(),
        "there is no final verified receipt for failing integration"
    );
    assert!(
        repository
            .events(kaisa_domain::EventKind::KnowledgePromoted)
            .is_empty(),
        "task eligibility cannot teach a lesson before final mission delivery"
    );
}

#[test]
fn stop_policy_blocks_new_siblings_while_continue_independent_runs_them() {
    for failure in [
        kaisa_domain::OnFailure::Stop,
        kaisa_domain::OnFailure::ContinueIndependent,
    ] {
        let repository = MissionRepository::new();
        let plan = Plan::new(vec![
            a_task("task_bad", "src/bad.md", &[]),
            a_task("task_sibling", "src/sibling.md", &[]),
            a_task("task_independent", "src/independent.md", &[]),
            a_task("task_dependent", "src/dependent.md", &["task_bad"]),
        ])
        .expect("failure DAG");
        let writer = |config: &Path| {
            let target = declared_target(config);
            fake_spec(
                config,
                &[
                    "--fake-write",
                    &target,
                    "--fake-content",
                    "task output\n",
                    "--fake-delay-ms",
                    WORKER_SLEEP_MS,
                    if target.contains("bad") {
                        "--crash-after-write"
                    } else {
                        "--fake-spawn-child"
                    },
                ],
            )
        };
        let outcome = repository.run(
            &plan,
            &parallel_policy(failure),
            &writer,
            &[trusted_check("pass")],
        );
        let started: Vec<_> = repository
            .events(kaisa_domain::EventKind::TaskStarted)
            .into_iter()
            .map(|event| event.subject)
            .collect();
        assert!(
            !started.contains(&"task_dependent".to_owned()),
            "a failed parent never satisfies a dependency"
        );
        assert_eq!(
            started.contains(&"task_independent".to_owned()),
            failure == kaisa_domain::OnFailure::ContinueIndependent,
            "failure policy controls admission after the failed wave"
        );
        assert!(!delivered(&outcome), "a partial mission never delivers");
        for (_, attempt) in &outcome.ran {
            let identity = std::fs::read_to_string(
                repository
                    .state
                    .join("attempts")
                    .join(attempt.attempt.as_str())
                    .join(kaisa_daemon::AGENT_IDENTITY_FILE),
            )
            .expect("recorded process");
            let identity =
                kaisa_sys::ProcessIdentity::from_line(&identity).expect("inspectable fake process");
            assert!(
                !identity.is_running().expect("process table"),
                "drained siblings and failing agents leave no live root process"
            );
            for pid in repository
                .transcript(attempt)
                .lines()
                .filter_map(|line| line.strip_prefix("fake-child-pid: "))
            {
                let pid = pid.parse().expect("child pid");
                let still_running = kaisa_sys::ProcessIdentity::of(pid)
                    .expect("child process table")
                    .is_some();
                // Mutation runs deliberately remove cleanup. Reap their owned fixture before
                // asserting, so a failed security regression does not itself leave an orphan.
                if still_running {
                    #[cfg(unix)]
                    let _ = Command::new("kill")
                        .args(["-KILL", &pid.to_string()])
                        .status();
                    #[cfg(windows)]
                    let _ = Command::new("taskkill")
                        .args(["/F", "/T", "/PID", &pid.to_string()])
                        .status();
                }
                assert!(
                    !still_running,
                    "the supervisor also ends each sibling's descendant"
                );
            }
        }
    }
}

#[test]
fn repeating_a_plan_replays_the_same_deterministic_start_order() {
    let plan = Plan::new(vec![
        a_task("task_z", "src/z.md", &[]),
        a_task("task_a", "src/a.md", &[]),
        a_task("task_j", "src/j.md", &["task_z", "task_a"]),
    ])
    .expect("replay DAG");
    let replay = || {
        let repository = MissionRepository::new();
        repository.run(
            &plan,
            &parallel_policy(kaisa_domain::OnFailure::Stop),
            &a_declared_writer,
            &[trusted_check("pass")],
        );
        repository
            .events(kaisa_domain::EventKind::TaskStarted)
            .into_iter()
            .map(|event| event.subject)
            .collect::<Vec<_>>()
    };
    let expected = vec![
        "task_z".to_owned(),
        "task_a".to_owned(),
        "task_j".to_owned(),
    ];
    assert_eq!(
        replay(),
        expected,
        "admission starts follow the documented plan order"
    );
    assert_eq!(
        replay(),
        expected,
        "completion timing must not reorder reproducible admission"
    );
}

#[test]
fn best_of_many_delivers_one_comparatively_verified_candidate_and_none_when_all_fail() {
    const CANDIDATES: usize = 3;
    for check in ["pass", "fail"] {
        let repository = MissionRepository::new();
        let policy = parallel_policy(kaisa_domain::OnFailure::Stop);
        let write_set = WriteSet::new(vec![
            RepoPath::try_new("src/candidate.md").expect("owned file"),
        ]);
        let checks = [trusted_check(check)];
        let developer = |config: &Path| {
            fake_spec(
                config,
                &[
                    "--fake-write",
                    "src/candidate.md",
                    "--fake-content",
                    "equivalent alternative\n",
                    "--fake-delay-ms",
                    WORKER_SLEEP_MS,
                ],
            )
        };
        let reviewer =
            |config: &Path| fake_spec(config, &["--fake-review-require", "src/candidate.md"]);
        let request = kaisa_daemon::VerifiedRequest {
            repository: &repository.repository,
            state_directory: &repository.state,
            goal: "Produce a verified candidate",
            write_set: &write_set,
            developer: &developer,
            reviewer: &reviewer,
            kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
            confine_agent: true,
            checks: &checks,
            policy: &policy,
            observed: false,
            first_pass: None,
            egress_hosts: &[],
            agent_writable: &[],
            reduced_mode: &kaisa_domain::ReducedModeBasis::None,
        };
        let outcome =
            kaisa_daemon::run_candidates(&request, CANDIDATES).expect("candidate infrastructure");
        assert_eq!(
            outcome.candidates.len(),
            CANDIDATES,
            "all independently requested alternatives are attempted"
        );
        let successful = check == "pass";
        assert_eq!(
            outcome.selected.is_some(),
            successful,
            "unverified alternatives can never be selected"
        );
        let receipts = repository.events(kaisa_domain::EventKind::MissionVerified);
        let expected_deliveries = usize::from(successful);
        assert_eq!(
            receipts.len(),
            expected_deliveries,
            "exactly one final mission delivery is recorded, or none"
        );
        if successful {
            let first_index = 0;
            assert_eq!(
                outcome.selected,
                Some(first_index),
                "equivalent alternatives use the documented candidate-index tie-break"
            );
            let intervals: Vec<_> = outcome
                .candidates
                .iter()
                .filter_map(|candidate| candidate.as_ref().ok())
                .filter_map(|candidate| candidate.attempts.first())
                .map(|attempt| worker_interval(&repository.transcript(attempt)))
                .collect();
            assert!(
                intervals[0].0 < intervals[1].1 && intervals[1].0 < intervals[0].1,
                "independent candidate developers really run concurrently"
            );
        }
    }
}

#[test]
fn a_plan_file_runs_verified_dependencies_through_the_cli_and_status_reads_the_result() {
    let repository = MissionRepository::new();
    let plan_file = repository.root.join("mission.json");
    let plan = serde_json::json!({"tasks":[
        {"id":"task_a","instruction":"Add first step","write_set":["src"]},
        {"id":"task_b","instruction":"Add next step","write_set":["src"],"depends_on":["task_a"]}
    ]});
    std::fs::write(&plan_file, plan.to_string()).expect("plan file");
    let binary = env!("CARGO_BIN_EXE_kaisa");
    let fake = PathBuf::from(binary)
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let checks = trusted_check("pass").argv;
    let mut command = Command::new(binary);
    command
        .current_dir(&repository.repository)
        .env("KAISA_STATE_DIR", &repository.state)
        .arg("Execute the explicit mission")
        .arg("--plan")
        .arg(&plan_file)
        .arg("--max-parallel")
        .arg(PARALLEL_SLOTS.to_string())
        .arg("--agent-program")
        .arg(fake)
        .args([
            "--agent-arg",
            "--fake-write-next",
            "--agent-arg",
            STEP_PREFIX,
            "--reviewer-arg",
            "--fake-review-require",
            "--reviewer-arg",
            "src/lib.rs",
        ])
        .arg("--check-program")
        .arg(&checks[0]);
    for argument in &checks[1..] {
        command.arg("--check-arg").arg(argument);
    }
    let output = command.output().expect("CLI starts");
    assert!(
        output.status.success(),
        "explicit plan uses integrated checks and review: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("integration: verified") && stdout.contains("retrieve with git merge"),
        "only the integrated verified tree is presented as the result: {stdout}"
    );
    let status = Command::new(binary)
        .arg("status")
        .current_dir(&repository.repository)
        .env("KAISA_STATE_DIR", &repository.state)
        .output()
        .expect("status reads ledger");
    assert!(
        status.status.success(),
        "status is a working read-only command: {status:?}"
    );
}

#[test]
fn an_observed_task_writing_outside_its_declared_set_reports_a_blocking_file_conflict() {
    let repository = MissionRepository::new();
    let plan = Plan::new(vec![a_task("task_outside", "src/allowed.md", &[])])
        .expect("valid declared ownership");
    let instructions =
        BTreeMap::from([(task("task_outside"), "Change only the allowed file".into())]);
    let policy = parallel_policy(kaisa_domain::OnFailure::Stop);
    let checks = [trusted_check("pass")];
    let developer = |config: &Path| fake_spec(config, &["--fake-observed-write", "src/outside.md"]);
    let reviewer = |config: &Path| fake_spec(config, &["--fake-review-require", "src/lib.rs"]);
    let outcome = run_mission(&MissionRequest {
        repository: &repository.repository,
        state_directory: &repository.state,
        plan: &plan,
        instructions: &instructions,
        launch: &developer,
        reviewer: &reviewer,
        checks: &checks,
        policy: &policy,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        observed: true,
        egress_hosts: &[],
        agent_writable: &[],
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("outside writes are blocking outcomes, not infrastructure errors");
    assert!(
        matches!(outcome.conflict, Some(kaisa_daemon::integrate::IntegrationError::OutsideWriteSet { ref files }) if files == &["src/outside.md"]),
        "the exact outside file is a typed conflict: {outcome:?}"
    );
    assert!(
        !delivered(&outcome),
        "observed containment never bypasses declared task ownership"
    );
    assert!(
        repository
            .events(kaisa_domain::EventKind::IntegrationConflict)
            .iter()
            .any(|event| event.payload.contains("src/outside.md")),
        "blocking conflicts retain affected files in the ledger"
    );
    assert!(
        repository.events(kaisa_domain::EventKind::GateDecided).iter().all(|event| {
            serde_json::from_str::<serde_json::Value>(&event.payload).expect("gate JSON")["decision"] != "Eligible"
        }),
        "ownership must pass before the task can publish eligibility"
    );
}

#[test]
fn a_rejected_task_review_blocks_mission_delivery_before_integration() {
    let repository = MissionRepository::new();
    let plan = Plan::new(vec![
        a_task("task_a", "src/a.md", &[]),
        a_task("task_b", "src/b.md", &[]),
    ])
    .expect("independent writers");
    let instructions = BTreeMap::from([
        (task("task_a"), "Create a".into()),
        (task("task_b"), "Create b".into()),
    ]);
    let policy = parallel_policy(kaisa_domain::OnFailure::Stop);
    let checks = [trusted_check("pass")];
    // A would pass this reviewer on the merged tree, but must fail on its own candidate.
    let reviewer = |config: &Path| fake_spec(config, &["--fake-review-require", "src/b.md"]);
    let outcome = run_mission(&MissionRequest {
        repository: &repository.repository,
        state_directory: &repository.state,
        plan: &plan,
        instructions: &instructions,
        launch: &a_declared_writer,
        reviewer: &reviewer,
        checks: &checks,
        policy: &policy,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: true,
        observed: false,
        egress_hosts: &[],
        agent_writable: &[],
        reduced_mode: &kaisa_domain::ReducedModeBasis::None,
    })
    .expect("task review outcome");
    assert_eq!(
        outcome.stopped_at,
        Some(task("task_a")),
        "the failing task is named even though every trusted command passed"
    );
    assert!(
        outcome.verification.is_none(),
        "a rejected per-task gate cannot be repaired by merely combining it with a sibling"
    );
}

#[test]
fn status_and_the_tui_show_live_mission_task_ids_from_the_ledger() {
    use std::time::{Duration, Instant};
    const ADMISSION_PATIENCE: Duration = Duration::from_secs(10);
    const LEDGER_POLL: Duration = Duration::from_millis(10);
    let repository = MissionRepository::new();
    let plan = Plan::new(vec![
        a_task("task_livea", "src/a.md", &[]),
        a_task("task_liveb", "src/b.md", &[]),
    ])
    .expect("live DAG");
    let policy = parallel_policy(kaisa_domain::OnFailure::Stop);
    let checks = [trusted_check("pass")];
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| repository.run(&plan, &policy, &a_declared_writer, &checks));
        let until = Instant::now() + ADMISSION_PATIENCE;
        loop {
            let running =
                kaisa_ledger::EventStore::open_read_only(&repository.state.join("kaisa.db"))
                    .and_then(|ledger| ledger.task_progress())
                    .map(|progress| progress.running.len())
                    .unwrap_or_default();
            if running == plan.tasks().len() {
                break;
            }
            assert!(
                Instant::now() < until,
                "the fake mission reaches its recorded running wave before the deadline"
            );
            std::thread::sleep(LEDGER_POLL);
        }
        for args in [vec!["status"], vec!["tui", "--plain"]] {
            let output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
                .args(args)
                .current_dir(&repository.repository)
                .env("KAISA_STATE_DIR", &repository.state)
                .output()
                .expect("read-only live status");
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success()
                    && text.contains("task_livea")
                    && text.contains("task_liveb"),
                "both currently running tasks are visible: {text}"
            );
        }
        assert!(
            delivered(&worker.join().expect("worker drained")),
            "status readers do not interfere with execution"
        );
    });
}

#[test]
fn the_candidates_flag_prints_exactly_one_verified_result_or_refuses_all_candidates() {
    const CANDIDATE_COUNT: &str = "2";
    for condition in ["pass", "fail"] {
        let repository = MissionRepository::new();
        let binary = env!("CARGO_BIN_EXE_kaisa");
        let fake = PathBuf::from(binary)
            .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
        let checks = trusted_check(condition).argv;
        let mut command = Command::new(binary);
        command
            .current_dir(&repository.repository)
            .env("KAISA_STATE_DIR", &repository.state)
            .arg("Choose the best verified alternative")
            .args([
                "--candidates",
                CANDIDATE_COUNT,
                "--max-parallel",
                CANDIDATE_COUNT,
            ])
            .arg("--agent-program")
            .arg(fake)
            .args([
                "--agent-arg",
                "--fake-write",
                "--agent-arg",
                "src/alternative.md",
                "--agent-arg",
                "--fake-content",
                "--agent-arg",
                "candidate output",
                "--reviewer-arg",
                "--fake-review-require",
                "--reviewer-arg",
                "src/alternative.md",
            ])
            .arg("--check-program")
            .arg(&checks[0]);
        for argument in &checks[1..] {
            command.arg("--check-arg").arg(argument);
        }
        let output = command.output().expect("candidate CLI starts");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            output.status.success(),
            condition == "pass",
            "failing candidates cannot yield a successful CLI result: {stdout}"
        );
        let expected_results = usize::from(condition == "pass");
        assert_eq!(
            stdout.matches("Result:").count(),
            expected_results,
            "the CLI exposes exactly one final retrieval command or none: {stdout}"
        );
    }
}

#[test]
fn an_explicitly_disabled_per_task_gate_still_requires_integrated_checks_and_review() {
    let repository = MissionRepository::new();
    let plan = Plan::new(vec![
        a_task("task_a", "src/a.md", &[]),
        a_task("task_b", "src/b.md", &[]),
    ])
    .expect("independent writers");
    let mut policy = kaisa_domain::Policy::personal_default();
    policy.verify_each_task = false;
    policy.max_parallel = PARALLEL_SLOTS;
    let policy = policy.snapshot().expect("explicit policy snapshot");
    let outcome = repository.run(&plan, &policy, &a_declared_writer, &[trusted_check("pass")]);
    assert!(
        delivered(&outcome),
        "the explicit opt-out skips task gates but retains integrated verification"
    );
    let review_count = repository
        .events(kaisa_domain::EventKind::ReviewVerdict)
        .len();
    let one_integrated_review = 1;
    assert_eq!(
        review_count, one_integrated_review,
        "no hidden per-task reviewer runs when the policy explicitly disables it"
    );
}

#[test]
fn a_finished_task_leaves_status_and_tui_before_its_slow_sibling() {
    use std::time::{Duration, Instant};
    let repository = MissionRepository::new();
    // Slow first also catches a completion receiver that waits in admission order.
    let plan = Plan::new(vec![
        a_task("task_slow", "src/slow.md", &[]),
        a_task("task_fast", "src/fast.md", &[]),
    ])
    .expect("independent tasks");
    let policy = parallel_policy(kaisa_domain::OnFailure::Stop);
    let checks = [trusted_check("pass")];
    let writer = |config: &Path| {
        let target = declared_target(config);
        fake_spec(
            config,
            &[
                "--fake-write",
                &target,
                "--fake-content",
                "output",
                "--fake-delay-ms",
                if target.contains("slow") { "5000" } else { "0" },
            ],
        )
    };
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| repository.run(&plan, &policy, &writer, &checks));
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            let eligible =
                kaisa_ledger::EventStore::open_read_only(&repository.state.join("kaisa.db"))
                    .and_then(|ledger| ledger.read_from(kaisa_domain::Sequence::FIRST))
                    .unwrap_or_default()
                    .iter()
                    .any(|event| {
                        event.recorded.event.kind == kaisa_domain::EventKind::GateDecided
                            && serde_json::from_str::<serde_json::Value>(
                                &event.recorded.event.payload,
                            )
                            .expect("gate JSON")["decision"]
                                == "Eligible"
                    });
            if eligible {
                break;
            }
            assert!(
                Instant::now() < until,
                "fast task must complete verification"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // Seeing an eligible task gate must imply its settlement is already visible.
        let ledger = kaisa_ledger::EventStore::open_read_only(&repository.state.join("kaisa.db"))
            .expect("ledger");
        let progress = ledger.task_progress().expect("progress");
        let events = ledger
            .read_from(kaisa_domain::Sequence::FIRST)
            .expect("events");
        let finished_at = events
            .iter()
            .position(|event| {
                event.recorded.event.kind == kaisa_domain::EventKind::TaskFinished
                    && event.recorded.event.subject == "task_fast"
            })
            .expect("fast settlement precedes eligibility");
        let eligible_at = events
            .iter()
            .position(|event| {
                event.recorded.event.kind == kaisa_domain::EventKind::GateDecided
                    && serde_json::from_str::<serde_json::Value>(&event.recorded.event.payload)
                        .expect("gate JSON")["decision"]
                        == "Eligible"
            })
            .expect("eligible gate");
        assert!(
            finished_at < eligible_at,
            "task settlement must precede its eligible gate"
        );
        assert!(
            !progress.running.contains(&task("task_fast")),
            "finished verified task must leave running state before slow sibling: {progress:?}"
        );
        assert!(
            progress.running.contains(&task("task_slow")),
            "slow sibling is still running"
        );
        let finished = repository.events(kaisa_domain::EventKind::TaskFinished);
        assert_eq!(finished.len(), 1, "only fast task has settled");
        assert_eq!(finished[0].subject, "task_fast");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&finished[0].payload).expect("finish JSON")["succeeded"],
            true
        );
        for args in [vec!["status"], vec!["tui", "--plain"]] {
            let output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
                .args(args)
                .current_dir(&repository.repository)
                .env("KAISA_STATE_DIR", &repository.state)
                .output()
                .expect("live status");
            let text = String::from_utf8_lossy(&output.stdout);
            let running = text
                .lines()
                .find_map(|line| line.strip_prefix("Running mission tasks: "));
            assert!(
                output.status.success() && running == Some("task_slow"),
                "only the unsettled task is reported running: {text}"
            );
        }
        assert!(delivered(&worker.join().expect("mission drains")));
    });
}

#[cfg(unix)]
mod cancellation {
    use std::collections::BTreeSet;
    use std::process::{Child, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    #[derive(Debug)]
    struct ProcessRow {
        pid: u32,
        parent: u32,
        state: String,
        command: String,
    }

    fn process_table() -> Vec<ProcessRow> {
        let output = Command::new("ps")
            .args(["-axo", "pid=,ppid=,stat=,command="])
            .output()
            .expect("process table");
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| {
                let mut fields = line.split_whitespace();
                ProcessRow {
                    pid: fields.next().expect("pid").parse().expect("numeric pid"),
                    parent: fields
                        .next()
                        .expect("parent")
                        .parse()
                        .expect("numeric parent"),
                    state: fields.next().expect("state").into(),
                    command: fields.collect::<Vec<_>>().join(" "),
                }
            })
            .collect()
    }

    // A failing regression must clean up its fixtures too, including after owner death.
    struct Fixture {
        owner: Child,
        owned: BTreeSet<u32>,
        identities: BTreeMap<u32, kaisa_sys::ProcessIdentity>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            for identity in self.identities.values() {
                if identity.is_running().unwrap_or(false) {
                    let _ = Command::new("kill")
                        .args(["-KILL", &identity.pid.to_string()])
                        .status();
                }
            }
            let _ = self.owner.kill();
            let _ = self.owner.wait();
        }
    }

    fn cancel(mode: &str, signal: &str) {
        let repository = MissionRepository::new();
        let plan = repository.root.join("plan.json");
        std::fs::write(
            &plan,
            serde_json::json!({"tasks":[
                {"id":"task_a","instruction":"A","write_set":["src/a.md"]},
                {"id":"task_b","instruction":"B","write_set":["src/b.md"]},
                {"id":"task_pending","instruction":"Never admitted","write_set":["src/pending.md"]}
            ]})
            .to_string(),
        )
        .expect("plan");
        let binary = env!("CARGO_BIN_EXE_kaisa");
        let mut command = Command::new(binary);
        command
            .current_dir(&repository.repository)
            .env("KAISA_STATE_DIR", &repository.state)
            .arg("Cancel all parallel work");
        if mode == "mission" {
            command.arg("--plan").arg(&plan);
        } else {
            command.args(["--candidates", "3"]);
        }
        command
            .args(["--max-parallel", "2", "--agent-program"])
            .arg(PathBuf::from(binary).with_file_name("kaisa-fake-agent"))
            .args([
                "--agent-arg",
                "--fake-spawn-child",
                "--agent-arg",
                "--fake-delay-ms",
                "--agent-arg",
                "30000",
                "--agent-arg",
                "--fake-write-next",
                "--agent-arg",
                "src/step",
                "--reviewer-arg",
                "--fake-review-require",
                "--reviewer-arg",
                "src/lib.rs",
            ]);
        let checks = trusted_check("pass").argv;
        command.arg("--check-program").arg(&checks[0]);
        for argument in &checks[1..] {
            command.arg("--check-arg").arg(argument);
        }
        let log = std::fs::File::create(repository.root.join("cli.log")).expect("CLI log");
        command
            .stdout(Stdio::from(log.try_clone().expect("clone log")))
            .stderr(Stdio::from(log));
        let mut fixture = Fixture {
            owner: command.spawn().expect("CLI"),
            owned: BTreeSet::new(),
            identities: BTreeMap::new(),
        };
        let until = Instant::now() + Duration::from_secs(20);
        let roots = loop {
            let table = process_table();
            let mut descendants = BTreeSet::from([fixture.owner.id()]);
            loop {
                let before = descendants.len();
                for row in &table {
                    if descendants.contains(&row.parent) {
                        descendants.insert(row.pid);
                    }
                }
                if descendants.len() == before {
                    break;
                }
            }
            descendants.remove(&fixture.owner.id());
            for pid in &descendants {
                if let Ok(Some(identity)) = kaisa_sys::ProcessIdentity::of(*pid) {
                    fixture.identities.entry(*pid).or_insert(identity);
                }
            }
            fixture.owned.extend(descendants);
            let roots: Vec<_> = table
                .iter()
                .filter(|row| {
                    fixture.owned.contains(&row.pid)
                        && row.command.contains("kaisa-fake-agent")
                        && !row.command.contains("--fake-child-sleep")
                })
                .map(|row| row.pid)
                .collect();
            let children = table
                .iter()
                .filter(|row| {
                    fixture.owned.contains(&row.pid) && row.command.contains("--fake-child-sleep")
                })
                .count();
            if roots.len() == 2 && children == 2 {
                break roots;
            }
            assert!(
                fixture.owner.try_wait().expect("owner state").is_none(),
                "owner must launch workers: {}",
                std::fs::read_to_string(repository.root.join("cli.log")).expect("log")
            );
            assert!(
                Instant::now() < until,
                "both agents and descendants must appear: {table:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(
            Command::new("kill")
                .args([signal, &fixture.owner.id().to_string()])
                .status()
                .expect("send signal")
                .success()
        );
        let until = Instant::now() + Duration::from_secs(10);
        let exit = loop {
            if let Some(exit) = fixture.owner.try_wait().expect("owner exit") {
                break exit;
            }
            assert!(Instant::now() < until, "cancellation must drain promptly");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(!exit.success(), "cancelled work cannot deliver");
        let until = Instant::now() + Duration::from_secs(1);
        let survivors = loop {
            let survivors: Vec<_> = process_table()
                .into_iter()
                .filter(|row| fixture.owned.contains(&row.pid) && !row.state.starts_with('Z'))
                .collect();
            if survivors.is_empty() || Instant::now() >= until {
                break survivors;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(
            survivors.is_empty(),
            "{mode} {signal} left orphan agents/descendants in process table: {survivors:?}"
        );
        let attempts = std::fs::read_dir(repository.state.join("attempts"))
            .expect("attempts")
            .map(|entry| entry.expect("attempt").path())
            .collect::<Vec<_>>();
        assert_eq!(
            attempts.len(),
            roots.len(),
            "no pending candidate, reviewer or task is admitted after cancellation"
        );
        let events = repository.events(kaisa_domain::EventKind::AttemptStateChanged);
        for attempt in attempts {
            let subject = attempt
                .file_name()
                .expect("attempt id")
                .to_str()
                .expect("UTF-8");
            assert!(
                events
                    .iter()
                    .any(|event| event.subject == subject && event.payload.contains("terminated")),
                "every running agent has a stopped ledger outcome: {events:?}"
            );
            assert!(
                events
                    .iter()
                    .any(|event| event.subject == subject && event.payload.contains("reclaimed")),
                "every stopped attempt finishes cleanup"
            );
        }
        assert!(
            repository
                .events(kaisa_domain::EventKind::MissionVerified)
                .is_empty(),
            "cancelled missions never deliver"
        );
        if mode == "mission" {
            assert!(
                kaisa_ledger::EventStore::open_read_only(&repository.state.join("kaisa.db"))
                    .expect("ledger")
                    .task_progress()
                    .expect("progress")
                    .running
                    .is_empty(),
                "cancelled tasks settle in status"
            );
        }
        fixture.owned.clear();
        fixture.identities.clear();
    }

    #[test]
    fn sigint_stops_parallel_mission_trees() {
        cancel("mission", "-INT");
    }
    #[test]
    fn sigterm_stops_parallel_mission_trees() {
        cancel("mission", "-TERM");
    }
    #[test]
    fn sigint_stops_candidate_trees() {
        cancel("candidates", "-INT");
    }
    #[test]
    fn sigterm_stops_candidate_trees() {
        cancel("candidates", "-TERM");
    }
}
