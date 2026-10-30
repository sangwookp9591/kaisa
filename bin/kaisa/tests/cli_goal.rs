//! `kaisa "<goal>"` from a user's terminal, end to end, with a scripted agent.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// What the user asks for.
const GOAL: &str = "Add notes about the login flow.";
/// Where the summary says the work went.
const PRESERVED_ON: &str = "work preserved on kaisa/att_";

fn git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(directory)
        .status()
        .expect("git is installed on every CI runner");
    assert!(status.success(), "git {args:?} failed");
}

/// A repository with one commit and a `src` directory, removed on drop.
struct Workbench {
    base: PathBuf,
}

impl Workbench {
    fn new(name: &str) -> Self {
        let base: PathBuf =
            std::env::temp_dir().join(format!("kaisa-cli-goal-{name}-{}", std::process::id()));
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
}

impl Drop for Workbench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[test]
fn kaisa_with_a_goal_prints_where_the_preserved_work_is() {
    let bench = Workbench::new("summary");
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));

    let run: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args([
            "--agent-program",
            &fake_agent.to_string_lossy(),
            "--agent-arg",
            "--fake-write",
            "--agent-arg",
            "src/notes.md",
            GOAL,
        ])
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", bench.base.join("state"))
        .output()
        .expect("kaisa runs");
    let stdout: String = String::from_utf8_lossy(&run.stdout).into_owned();

    assert!(
        run.status.success() && stdout.contains(PRESERVED_ON),
        "one line tells the user the agent finished and where its work is; got {stdout:?} {:?}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// How long the killed run may take to reach the agent's brokered write.
#[cfg(unix)]
const WRITE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

/// The goal of the run that gets killed.
#[cfg(unix)]
const INTERRUPTED_GOAL: &str = "Write the session notes.";

#[cfg(unix)]
#[test]
fn an_attempt_abandoned_by_a_killed_kaisa_is_recovered_on_the_next_run() {
    let bench = Workbench::new("recovery");
    let repository: PathBuf = bench.base.join("repo");
    let state: PathBuf = bench.base.join("state");
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let kaisa_running = |goal: &str, extra: &[&str]| {
        let mut args: Vec<String> = vec![
            "--agent-program".to_owned(),
            fake_agent.to_string_lossy().into_owned(),
            "--agent-arg".to_owned(),
            "--fake-write".to_owned(),
            "--agent-arg".to_owned(),
            "src/notes.md".to_owned(),
        ];
        for flag in extra {
            args.extend(["--agent-arg".to_owned(), (*flag).to_owned()]);
        }
        args.push(goal.to_owned());
        Command::new(env!("CARGO_BIN_EXE_kaisa"))
            .args(args)
            .current_dir(&repository)
            .env("KAISA_STATE_DIR", &state)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("kaisa starts")
    };
    let mut interrupted = kaisa_running(INTERRUPTED_GOAL, &["--hang-after-write"]);
    let deadline = std::time::Instant::now() + WRITE_PATIENCE;
    let written = || {
        std::fs::read_dir(state.join("worktrees"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|worktree| worktree.path().join("src/notes.md").exists())
    };
    while !written() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    interrupted.kill().expect("kaisa is killed mid-attempt");
    interrupted.wait().expect("reaped");

    let next: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args([
            "--agent-program",
            &fake_agent.to_string_lossy(),
            "--agent-arg",
            "--fake-write",
            "--agent-arg",
            "src/more.md",
            GOAL,
        ])
        .current_dir(&repository)
        .env("KAISA_STATE_DIR", &state)
        .output()
        .expect("kaisa runs again");
    let stdout: String = String::from_utf8_lossy(&next.stdout).into_owned();
    let leftover_worktrees: usize = std::fs::read_dir(state.join("worktrees"))
        .map(|entries| entries.count())
        .unwrap_or(0);

    assert!(
        stdout.contains("recovered att_") && stdout.contains("was ended"),
        "the next run ends the abandoned agent and says so; got {stdout:?}"
    );
    assert!(
        stdout.contains("work preserved on kaisa/att_") && leftover_worktrees == 0,
        "the abandoned attempt's work is preserved and no worktree is left; got {stdout:?}"
    );
}

/// Rounds of starting Kaisa and killing it at a random moment.
#[cfg(unix)]
const KILL_ROUNDS: u64 = 5;
/// The latest moment a round kills Kaisa, in milliseconds after starting it.
#[cfg(unix)]
const LATEST_KILL_MS: u64 = 1_500;
/// Seed of the kill schedule; printed on failure so a run can be replayed.
#[cfg(unix)]
const KILL_SEED: u64 = 0x5eed_c4a0;

/// The next value of a linear congruential generator (deterministic, dependency-free).
#[cfg(unix)]
fn next_random(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state >> 33
}

/// True if a live process's command line mentions `needle`.
#[cfg(unix)]
fn a_process_mentions(needle: &str) -> bool {
    let listing = Command::new("ps")
        .args(["-A", "-o", "command="])
        .output()
        .expect("ps is on every Unix runner");
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .any(|line| line.contains(needle) && !line.contains("ps -A"))
}

#[cfg(unix)]
#[test]
fn no_resource_is_orphaned_after_random_kills() {
    let bench = Workbench::new("chaos");
    let repository: PathBuf = bench.base.join("repo");
    let state: PathBuf = bench.base.join("state");
    let fake_agent: String = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX))
        .to_string_lossy()
        .into_owned();
    let kaisa = |goal: &str, hang: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kaisa"));
        command.args([
            "--agent-program",
            &fake_agent,
            "--agent-arg",
            "--fake-write",
        ]);
        command.args(["--agent-arg", "src/notes.md"]);
        if hang {
            command.args(["--agent-arg", "--hang-after-write"]);
        }
        command
            .arg(goal)
            .current_dir(&repository)
            .env("KAISA_STATE_DIR", &state);
        command
    };
    let mut seed: u64 = KILL_SEED;
    let mut schedule: Vec<u64> = Vec::new();
    for round in 0..KILL_ROUNDS {
        let delay: u64 = next_random(&mut seed) % LATEST_KILL_MS;
        schedule.push(delay);
        let mut running = kaisa(&format!("Chaos round {round}."), true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("kaisa starts");
        std::thread::sleep(std::time::Duration::from_millis(delay));
        let _ = running.kill();
        let _ = running.wait();
    }

    let recovery: Output = kaisa(GOAL, false).output().expect("kaisa runs again");
    let report: String = String::from_utf8_lossy(&recovery.stdout).into_owned();
    let leftover: Vec<String> = std::fs::read_dir(state.join("worktrees"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    let replay: String = format!("seed {KILL_SEED:#x}, kills at {schedule:?} ms");

    assert!(
        !a_process_mentions(&state.to_string_lossy()),
        "no agent or bridge of a killed run is still alive ({replay})"
    );
    assert!(
        leftover
            .iter()
            .all(|attempt| report.contains(&format!("kept {attempt} untouched"))),
        "every worktree still on disk was reported to the user as kept; left {leftover:?}, \
         report {report:?} ({replay})"
    );
}

#[test]
fn watch_shows_each_attempts_state_from_the_ledger() {
    let bench = Workbench::new("watch");
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let state: PathBuf = bench.base.join("state");
    let ran: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args([
            "--agent-program",
            &fake_agent.to_string_lossy(),
            "--agent-arg",
            "--fake-write",
            "--agent-arg",
            "src/notes.md",
            GOAL,
        ])
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", &state)
        .output()
        .expect("kaisa runs");
    let attempt: String = String::from_utf8_lossy(&ran.stdout)
        .split_whitespace()
        .find(|word| word.starts_with("att_"))
        .expect("the run names its attempt")
        .to_owned();

    let watched: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .arg("watch")
        .env("KAISA_STATE_DIR", &state)
        .output()
        .expect("kaisa watch runs");
    let view: String = String::from_utf8_lossy(&watched.stdout).into_owned();
    let line: &str = view
        .lines()
        .find(|line| line.starts_with(&attempt))
        .unwrap_or_default();

    assert!(
        watched.status.success() && line.contains("exited") && line.contains("reclaimed"),
        "the finished attempt is listed as exited and reclaimed: {view:?}"
    );
}

/// Runs `kaisa agent <args>` against `state`.
fn kaisa_agent(state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .arg("agent")
        .args(args)
        .env("KAISA_STATE_DIR", state)
        .output()
        .expect("kaisa runs")
}

#[test]
fn a_personal_agent_is_added_from_the_cli_without_an_admin() {
    let bench = Workbench::new("agent-add");
    let state: PathBuf = bench.base.join("state");

    let added = kaisa_agent(
        &state,
        &[
            "add",
            "ollama",
            "--endpoint",
            "http://localhost:11434/v1",
            "--model",
            "llama3=T1",
        ],
    );
    let leaky = kaisa_agent(
        &state,
        &[
            "add",
            "remote",
            "--endpoint",
            "http://models.example/v1",
            "--model",
            "big=T2",
        ],
    );
    let listed = kaisa_agent(&state, &["list"]);

    assert!(
        added.status.success(),
        "a local model server is registered by its user: {:?}",
        String::from_utf8_lossy(&added.stderr)
    );
    assert!(
        !leaky.status.success(),
        "an unencrypted remote endpoint would leak prompts and is refused"
    );
    let listing: String = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(
        listing.contains("ollama llama3=T1") && !listing.contains("remote"),
        "the list shows what was accepted and nothing else: {listing:?}"
    );
}

/// The CLI arguments of a scripted verified run: a developer writing step files, a reviewer
/// requiring the first one, and optionally the user's own check requiring it too.
fn a_verified_run(fake_agent: &Path, with_check: bool) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--agent-program".to_owned(),
        fake_agent.to_string_lossy().into_owned(),
        "--agent-arg".to_owned(),
        "--fake-write-next".to_owned(),
        "--agent-arg".to_owned(),
        "src/step".to_owned(),
        "--reviewer-arg".to_owned(),
        "--fake-review-require".to_owned(),
        "--reviewer-arg".to_owned(),
        "src/step-1.md".to_owned(),
    ];
    if with_check {
        let (program, check) = if cfg!(windows) {
            ("cmd", r"if exist src\step-1.md (exit 0) else (exit 1)")
        } else {
            ("sh", "test -f src/step-1.md")
        };
        let flag = if cfg!(windows) { "/c" } else { "-c" };
        args.extend(
            [
                "--check-program",
                program,
                "--check-arg",
                flag,
                "--check-arg",
                check,
            ]
            .iter()
            .map(|arg| (*arg).to_owned()),
        );
    }
    args.push(GOAL.to_owned());
    args
}

#[test]
fn kaisa_with_a_verified_goal_reports_verified_and_exits_zero() {
    let bench = Workbench::new("verified");
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));

    let run: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args(a_verified_run(&fake_agent, true))
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", bench.base.join("state"))
        .output()
        .expect("kaisa runs");
    let stdout: String = String::from_utf8_lossy(&run.stdout).into_owned();

    assert!(
        run.status.success() && stdout.contains("verified:"),
        "checks passed and the review approved, so it is delivered as verified; got {stdout:?} {:?}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// Exit status of a goal that was not delivered (CI contract code 4).
const NOT_DELIVERED: i32 = 4;

#[test]
fn kaisa_without_evidence_delivers_nothing_and_exits_four() {
    let bench = Workbench::new("no-evidence");
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));

    let run: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args(a_verified_run(&fake_agent, false))
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", bench.base.join("state"))
        .output()
        .expect("kaisa runs");
    let stdout: String = String::from_utf8_lossy(&run.stdout).into_owned();

    assert_eq!(
        run.status.code(),
        Some(NOT_DELIVERED),
        "a repository with no trusted check gives no evidence, so nothing is delivered: {stdout:?}"
    );
    assert!(
        stdout.contains("not delivered"),
        "the user is told plainly: {stdout:?}"
    );
}
