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
