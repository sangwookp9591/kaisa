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
            &format!("http://127.0.0.1:{}/v1", a_local_model_server()),
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
        run.status.success()
            && stdout
                .lines()
                .any(|line| line.starts_with("verified:") || line.starts_with("verified (reduced")),
        "checks passed and the review approved, so it is delivered as verified (reduced where checks \
         run without OS confinement); got {stdout:?} {:?}",
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

#[test]
fn a_cli_agent_that_does_not_run_is_not_registered() {
    let bench = Workbench::new("agent-conformance");
    let state: PathBuf = bench.base.join("state");
    let missing: String = bench
        .base
        .join("no-such-agent")
        .to_string_lossy()
        .into_owned();

    let refused = kaisa_agent(
        &state,
        &["add", "ghost", "--program", &missing, "--model", "m=T1"],
    );
    let accepted = kaisa_agent(
        &state,
        &["add", "vcs", "--program", "git", "--model", "m=T1"],
    );

    assert!(
        !refused.status.success(),
        "a program that cannot start is never offered to the router"
    );
    assert!(
        accepted.status.success(),
        "a program that answers --version is registered: {:?}",
        String::from_utf8_lossy(&accepted.stderr)
    );
}

/// Starts a one-shot OpenAI-compatible server on loopback that answers "ok"; returns its port.
fn a_local_model_server() -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("loopback works");
    let port: u16 = listener.local_addr().expect("bound").port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            // Read the whole request (headers, then Content-Length bytes) before answering, so
            // closing the connection never resets a client still sending its body.
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
            let body = r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

#[test]
fn an_unreachable_model_server_is_not_registered() {
    let bench = Workbench::new("model-unreachable");
    let state: PathBuf = bench.base.join("state");
    let closed = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("loopback works");
    let port: u16 = closed.local_addr().expect("bound").port();
    drop(closed);

    let refused = kaisa_agent(
        &state,
        &[
            "add",
            "silent",
            "--endpoint",
            &format!("http://127.0.0.1:{port}/v1"),
            "--model",
            "m=T1",
        ],
    );

    assert!(
        !refused.status.success(),
        "a model server that does not answer is never offered to the router"
    );
}

/// Usage the scripted agent reports: input tokens, output tokens and cost.
const REPORTED_USAGE: &str = "1200/340/0.0123";

#[test]
fn watch_shows_recorded_usage_per_attempt() {
    let bench = Workbench::new("usage");
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let state: PathBuf = bench.base.join("state");
    Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args([
            "--agent-program",
            &fake_agent.to_string_lossy(),
            "--agent-arg",
            "--fake-usage",
            "--agent-arg",
            REPORTED_USAGE,
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

    let watched: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .arg("watch")
        .env("KAISA_STATE_DIR", &state)
        .output()
        .expect("kaisa watch runs");
    let view: String = String::from_utf8_lossy(&watched.stdout).into_owned();

    assert!(
        view.contains("1200/340") && view.contains("$0.0123"),
        "the attempt's tokens and cost are shown from the ledger: {view:?}"
    );
}

/// What the fake agent writes when it is started the way Codex is.
const WRITTEN_BY_CODEX: &str = "src/written-by-codex.md";

#[test]
fn a_codex_goal_is_reported_as_observed() {
    let bench = Workbench::new("codex");
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));

    let run: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args([
            "--agent",
            "codex",
            "--agent-program",
            &fake_agent.to_string_lossy(),
            "--reviewer-arg",
            "--fake-review-require",
            "--reviewer-arg",
            WRITTEN_BY_CODEX,
            GOAL,
        ])
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", bench.base.join("state"))
        .output()
        .expect("kaisa runs");
    let stdout: String = String::from_utf8_lossy(&run.stdout).into_owned();

    assert!(
        stdout
            .lines()
            .next()
            .is_some_and(|_| stdout.contains("observed")),
        "the Codex attempt is labelled observed, not brokered: {stdout:?} {:?}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn an_unknown_agent_is_a_usage_error() {
    let bench = Workbench::new("unknown-agent");

    let run: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args(["--agent", "nobody", GOAL])
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", bench.base.join("state"))
        .output()
        .expect("kaisa runs");

    assert!(
        String::from_utf8_lossy(&run.stderr).contains("unknown agent"),
        "an agent Kaisa does not know is named, not silently replaced: {:?}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// The line of a review pack naming the candidate's tree.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const CANDIDATE_TREE_LABEL: &str = "candidate_tree: ";

/// The local reviewer's final answer, carried in its one-line `done` protocol.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Clone, Copy)]
enum LocalReviewAnswer {
    Dedicated,
    Malformed,
}

/// A loopback OpenAI-compatible server standing in for a registered local model. It answers
/// the registration probe with a greeting and every review pack with an approving verdict for
/// the tree the pack names, counting the reviews it was asked for.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn a_local_reviewer_model(
    reviews: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    answer: LocalReviewAnswer,
) -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("loopback works");
    let port: u16 = listener.local_addr().expect("bound").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut request: Vec<u8> = Vec::new();
            let mut buffer = [0_u8; 4096];
            let mut body_start: Option<(usize, usize)> = None;
            while let Ok(read) = stream.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request);
                if body_start.is_none()
                    && let Some(end) = text.find("\r\n\r\n")
                {
                    let length: usize = text[..end]
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    body_start = Some((end + "\r\n\r\n".len(), length));
                }
                if let Some((start, length)) = body_start
                    && request.len() >= start + length
                {
                    break;
                }
            }
            let body: serde_json::Value = body_start
                .and_then(|(start, _)| serde_json::from_slice(&request[start..]).ok())
                .unwrap_or_default();
            let tree: Option<String> = body["messages"].as_array().and_then(|messages| {
                messages.iter().find_map(|message| {
                    message["content"].as_str()?.lines().find_map(|line| {
                        line.trim_start_matches("> ")
                            .strip_prefix(CANDIDATE_TREE_LABEL)
                            .map(|tree| tree.trim().to_owned())
                    })
                })
            });
            let content: String = match tree {
                Some(tree) => {
                    reviews.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let verdict = serde_json::json!({"candidate_tree": tree, "approve": true, "findings": []});
                    let summary = match answer {
                        LocalReviewAnswer::Dedicated => format!("kaisa-verdict: {verdict}"),
                        LocalReviewAnswer::Malformed => "kaisa-verdict: {BROKEN".to_owned(),
                    };
                    serde_json::json!({"done": summary}).to_string()
                }
                None => "ready".to_owned(),
            };
            let reply = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": content}}]})
                .to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    port
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_light_task_runs_on_the_registered_local_model() {
    a_local_first_pass_that_reaches_full_review(LocalReviewAnswer::Dedicated);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_malformed_local_first_pass_leaves_the_decision_to_the_full_reviewer() {
    a_local_first_pass_that_reaches_full_review(LocalReviewAnswer::Malformed);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn a_local_first_pass_that_reaches_full_review(answer: LocalReviewAnswer) {
    const EXPECTED_FIRST_PASSES: usize = 1;
    let bench = Workbench::new(match answer {
        LocalReviewAnswer::Dedicated => "light-work",
        LocalReviewAnswer::Malformed => "malformed-first-pass",
    });
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let reviews = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let port: u16 = a_local_reviewer_model(std::sync::Arc::clone(&reviews), answer);
    let state: PathBuf = bench.base.join("state");
    let registered: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args([
            "agent",
            "add",
            "local",
            "--endpoint",
            &format!("http://127.0.0.1:{port}/v1"),
            "--model",
            "tiny=T0",
        ])
        .env("KAISA_STATE_DIR", &state)
        .output()
        .expect("kaisa agent add runs");
    assert!(
        registered.status.success(),
        "the local model passes its registration check: {:?}",
        String::from_utf8_lossy(&registered.stderr)
    );

    let run: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args(a_verified_run(&fake_agent, true))
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", &state)
        .output()
        .expect("kaisa runs");
    let stdout: String = String::from_utf8_lossy(&run.stdout).into_owned();

    assert!(
        stdout.contains("model: local (light work)"),
        "the person is told the first pass runs on the local model: {stdout:?}"
    );
    assert!(
        reviews.load(std::sync::atomic::Ordering::SeqCst) == EXPECTED_FIRST_PASSES,
        "the local model was actually asked for the first-pass review"
    );
    assert!(
        run.status.success(),
        "the full review still decided and the candidate was delivered: {stdout:?} {:?}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// The revision limit the person sets: no revisions at all.
const NO_REVISIONS: &str = "0";

#[test]
fn a_run_uses_the_personal_policy() {
    let bench = Workbench::new("personal-policy");
    let state: PathBuf = bench.base.join("state");
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let set: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args(["policy", "set", "revision_limit", NO_REVISIONS])
        .env("KAISA_STATE_DIR", &state)
        .output()
        .expect("kaisa policy runs");
    assert!(set.status.success(), "the personal policy is saved");

    let run: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args(a_verified_run(&fake_agent, true))
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", &state)
        .output()
        .expect("kaisa runs");

    let adopted = kaisa_ledger::EventStore::open(&state.join("kaisa.db"))
        .and_then(|ledger| ledger.adopted_policies())
        .expect("the ledger reads");
    assert!(
        adopted.iter().any(|policy| policy
            .canonical
            .contains(&format!("revision_limit={NO_REVISIONS}\n"))),
        "the run adopted the person's policy, not the built-in default: {adopted:?} {:?}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn a_trial_run_is_labelled() {
    let bench = Workbench::new("trial");
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let mut args: Vec<String> = vec!["--confine-claude".to_owned()];
    args.extend(a_verified_run(&fake_agent, true));

    let run: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args(args)
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", bench.base.join("state"))
        .output()
        .expect("kaisa runs");
    let stdout: String = String::from_utf8_lossy(&run.stdout).into_owned();

    assert!(
        stdout.contains("trial (T16.0)") && stdout.contains("api.anthropic.com"),
        "the person is told this is a trial and what Claude Code may reach: {stdout:?}"
    );
}

/// Exit status of a run that waits for a person (CI contract code 3).
const WAITING_FOR_YOU: i32 = 3;
/// Exit status of a command used with the wrong arguments.
const USAGE_ERROR: i32 = 64;
/// How long a person's Decision lasts in these tests: one hour, in milliseconds.
const DECISION_LIFETIME_MS: u64 = 60 * 60 * 1000;
/// Every capability a run without an OS sandbox is missing, as a person types them.
const EVERY_CAPABILITY: &str = "write,tcp,udp,descendants";

/// Milliseconds since the Unix epoch, the ledger's clock.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).expect("a date this century"))
        .expect("the clock is after 1970")
}

/// A state directory whose personal policy turns agent confinement off, so a scripted agent
/// would run without any OS sandbox.
fn a_state_with_confinement_turned_off(bench: &Workbench) -> PathBuf {
    let state: PathBuf = bench.base.join("state");
    let set: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args(["policy", "set", "confine_agents", "false"])
        .env("KAISA_STATE_DIR", &state)
        .output()
        .expect("kaisa policy runs");
    assert!(set.status.success(), "the personal policy is saved");
    state
}

/// `--accept-reduced <capabilities> --accept-until <ms>`: a person's scoped Decision.
fn a_decision_accepting(capabilities: &str, until_ms: u64) -> Vec<String> {
    vec![
        "--accept-reduced".to_owned(),
        capabilities.to_owned(),
        "--accept-until".to_owned(),
        until_ms.to_string(),
    ]
}

/// A scripted verified run with extra arguments in front, against `state`.
fn kaisa_verified_with(bench: &Workbench, state: &Path, extra: &[String]) -> Output {
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let mut args: Vec<String> = extra.to_vec();
    args.extend(a_verified_run(&fake_agent, true));
    Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args(args)
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", state)
        .output()
        .expect("kaisa runs")
}

/// The payloads of every `attempt.admitted` event in the ledger under `state`.
fn admitted_attempts(state: &Path) -> Vec<serde_json::Value> {
    let ledger = kaisa_ledger::EventStore::open(&state.join("kaisa.db")).expect("the ledger opens");
    ledger
        .read_from(kaisa_domain::Sequence::FIRST)
        .expect("the ledger reads")
        .into_iter()
        .filter(|chained| chained.recorded.event.kind == kaisa_domain::EventKind::AttemptAdmitted)
        .map(|chained| {
            serde_json::from_str(&chained.recorded.event.payload).expect("payloads are JSON")
        })
        .collect()
}

#[test]
fn a_run_missing_containment_without_a_decision_waits_for_a_person_and_lists_what_is_missing() {
    let bench = Workbench::new("refused");
    let state: PathBuf = a_state_with_confinement_turned_off(&bench);

    let run: Output = kaisa_verified_with(&bench, &state, &[]);
    let stdout: String = String::from_utf8_lossy(&run.stdout).into_owned();

    assert_eq!(
        run.status.code(),
        Some(WAITING_FOR_YOU),
        "weaker containment is never the silent default: a person must decide; {stdout:?} {:?}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        stdout.contains("waiting for you") && stdout.contains(EVERY_CAPABILITY),
        "the person is told exactly what a Decision would have to accept: {stdout:?}"
    );
    assert!(
        admitted_attempts(&state).is_empty(),
        "no attempt was admitted, so no agent ran"
    );
}

#[test]
fn a_decision_accepting_what_is_missing_runs_reduced_and_records_why() {
    let bench = Workbench::new("decided");
    let state: PathBuf = a_state_with_confinement_turned_off(&bench);
    let until_ms: u64 = now_ms() + DECISION_LIFETIME_MS;

    let run: Output = kaisa_verified_with(
        &bench,
        &state,
        &a_decision_accepting(EVERY_CAPABILITY, until_ms),
    );
    let stdout: String = String::from_utf8_lossy(&run.stdout).into_owned();
    let admitted: Vec<serde_json::Value> = admitted_attempts(&state);

    assert!(
        run.status.success()
            && stdout.contains("reduced: the agent process ran without OS confinement"),
        "the Decision admits the run and every attempt line says it was reduced: {stdout:?} {:?}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        !admitted.is_empty()
            && admitted.iter().all(|payload| {
                payload["containment"]["admission"] == "reduced"
                    && payload["containment"]["basis"] == "decision"
                    && payload["containment"]["expires_at_ms"] == until_ms
                    && payload["containment"]["missing"].as_array().map(Vec::len)
                        == Some(EVERY_CAPABILITY.split(',').count())
            }),
        "the ledger keeps what each attempt lacked, what admitted it and until when: {admitted:?}"
    );
}

#[test]
fn an_expired_decision_does_not_admit_a_reduced_run() {
    let bench = Workbench::new("expired");
    let state: PathBuf = a_state_with_confinement_turned_off(&bench);
    let ended_ms: u64 = now_ms() - DECISION_LIFETIME_MS;

    let run: Output = kaisa_verified_with(
        &bench,
        &state,
        &a_decision_accepting(EVERY_CAPABILITY, ended_ms),
    );
    let stdout: String = String::from_utf8_lossy(&run.stdout).into_owned();

    assert_eq!(
        run.status.code(),
        Some(WAITING_FOR_YOU),
        "yesterday's permission is not today's: {stdout:?}"
    );
    assert!(
        admitted_attempts(&state).is_empty(),
        "no attempt was admitted on an expired Decision"
    );
}

#[test]
fn a_decision_covering_only_part_of_what_is_missing_does_not_admit_the_run() {
    let bench = Workbench::new("partial-decision");
    let state: PathBuf = a_state_with_confinement_turned_off(&bench);
    let until_ms: u64 = now_ms() + DECISION_LIFETIME_MS;

    let run: Output = kaisa_verified_with(&bench, &state, &a_decision_accepting("udp", until_ms));
    let stdout: String = String::from_utf8_lossy(&run.stdout).into_owned();

    assert_eq!(
        run.status.code(),
        Some(WAITING_FOR_YOU),
        "a Decision is scoped: accepting UDP does not accept unconfined writes: {stdout:?}"
    );
}

#[test]
fn an_unknown_capability_in_a_decision_is_a_usage_error() {
    let bench = Workbench::new("unknown-capability");
    let state: PathBuf = bench.base.join("state");

    let run: Output = kaisa_verified_with(
        &bench,
        &state,
        &a_decision_accepting("everything", now_ms() + DECISION_LIFETIME_MS),
    );

    assert_eq!(
        run.status.code(),
        Some(USAGE_ERROR),
        "a Decision cannot accept a capability Kaisa does not know: {:?}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn a_codex_goal_records_the_default_policy_that_admitted_it_reduced() {
    let bench = Workbench::new("codex-basis");
    let state: PathBuf = bench.base.join("state");
    let fake_agent: PathBuf = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));

    let run: Output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args([
            "--agent",
            "codex",
            "--agent-program",
            &fake_agent.to_string_lossy(),
            "--reviewer-arg",
            "--fake-review-require",
            "--reviewer-arg",
            WRITTEN_BY_CODEX,
            GOAL,
        ])
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", &state)
        .output()
        .expect("kaisa runs");
    let stdout: String = String::from_utf8_lossy(&run.stdout).into_owned();
    let admitted: Vec<serde_json::Value> = admitted_attempts(&state);

    assert!(
        stdout.contains("reduced: the agent process ran without OS confinement"),
        "the documented default flow keeps running and stays labelled reduced: {stdout:?} {:?}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        admitted
            .iter()
            .any(|payload| payload["containment"]["admission"] == "reduced"
                && payload["containment"]["basis"] == "observed-checked-afterwards"),
        "the ledger names the default policy that admitted the observed attempt: {admitted:?}"
    );
}

/// A direct daemon request, deliberately unconfined, using the same real agent as CLI tests.
fn a_reduced_daemon_request<'a>(
    repository: &'a Path,
    state: &'a Path,
    launch: kaisa_daemon::AgentLauncher<'a>,
    basis: &'a kaisa_domain::ReducedModeBasis,
    write_set: &'a kaisa_domain::WriteSet,
) -> kaisa_daemon::GoalRequest<'a> {
    kaisa_daemon::GoalRequest {
        repository,
        goal: GOAL,
        base: "HEAD",
        state_directory: state,
        write_set,
        role: kaisa_domain::Role::Developer,
        launch,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: false,
        egress_hosts: &[],
        agent_writable: &[],
        local_model_port: None,
        observed: false,
        reduced_mode: basis,
    }
}

/// A short but ample setup budget; the delayed launcher deliberately consumes all of it.
const SHORT_DECISION_MS: u64 = 3_000;

fn a_short_decision() -> kaisa_domain::ReducedModeBasis {
    kaisa_domain::ReducedModeBasis::Decision(kaisa_domain::ReducedModeDecision {
        accepts_missing: kaisa_domain::ContainmentCapability::ALL.to_vec(),
        expires_at_ms: now_ms() + SHORT_DECISION_MS,
    })
}

#[test]
fn a_decision_expiring_inside_the_launcher_refuses_spawn() {
    let bench = Workbench::new("expiry-before-spawn");
    let repository = bench.base.join("repo");
    let state = bench.base.join("state");
    let basis = a_short_decision();
    let write_set = kaisa_domain::WriteSet::default();
    let reached_launcher = std::cell::Cell::new(false);
    let launch = |_: &Path| {
        reached_launcher.set(true);
        let remaining = basis
            .expires_at_ms()
            .expect("a Decision")
            .saturating_sub(now_ms());
        std::thread::sleep(std::time::Duration::from_millis(remaining + 1));
        kaisa_sys::ProcessSpec {
            program: PathBuf::from(env!("CARGO_BIN_EXE_kaisa")),
            args: vec!["--version".to_owned()],
            working_directory: repository.clone(),
            env: Vec::new(),
        }
    };
    let result = kaisa_daemon::run_goal(&a_reduced_daemon_request(
        &repository,
        &state,
        &launch,
        &basis,
        &write_set,
    ));

    assert!(
        reached_launcher.get(),
        "the initial admission passed before the deliberate delay"
    );
    assert!(
        result.is_err_and(|error| error.not_admitted()),
        "the daemon must recheck after launch preparation and refuse the expired spawn"
    );
    let attempts = state.join("attempts");
    assert!(
        std::fs::read_dir(attempts)
            .expect("prepared attempts")
            .all(|entry| !entry
                .expect("an attempt directory")
                .path()
                .join(kaisa_daemon::AGENT_IDENTITY_FILE)
                .exists()),
        "no agent process may have started after expiry"
    );
}

#[test]
fn a_reduced_attempt_followed_by_a_check_infrastructure_error_keeps_its_label() {
    let bench = Workbench::new("reduced-check-error");
    let state = a_state_with_confinement_turned_off(&bench);
    std::fs::write(state.join("checks"), "a file blocks check preparation")
        .expect("a blocked check directory");
    let extra = a_decision_accepting(EVERY_CAPABILITY, now_ms() + DECISION_LIFETIME_MS);
    let fake_agent = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
        .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
    let mut args = extra;
    args.extend(a_verified_run(&fake_agent, false));
    args.splice(
        0..0,
        [
            "--check-program".to_owned(),
            bench
                .base
                .join("missing-check")
                .to_string_lossy()
                .into_owned(),
        ],
    );
    let run = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args(args)
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", &state)
        .output()
        .expect("kaisa runs");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        !run.status.success()
            && output.contains("reduced: the agent process ran without OS confinement")
            && output.contains("preparing checks"),
        "an infrastructure error must preserve the reduced attempt label: {output}"
    );
}

#[test]
fn a_running_decision_ends_at_expiry_and_cannot_launch_a_followup_attempt() {
    let bench = Workbench::new("expiry-during-execution");
    let repository = bench.base.join("repo");
    let state = bench.base.join("state");
    let basis = a_short_decision();
    let write_set = kaisa_domain::WriteSet::new(vec![
        kaisa_domain::RepoPath::try_new("src").expect("a path"),
    ]);
    let launch = |config: &Path| kaisa_sys::ProcessSpec {
        program: PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
            .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX)),
        args: vec![
            "--mcp-config".to_owned(),
            config.to_string_lossy().into_owned(),
            "--fake-write".to_owned(),
            "src/notes.md".to_owned(),
            "--hang-after-write".to_owned(),
        ],
        working_directory: repository.clone(),
        env: Vec::new(),
    };
    let request = a_reduced_daemon_request(&repository, &state, &launch, &basis, &write_set);
    let started = std::time::Instant::now();
    let outcome =
        kaisa_daemon::run_goal(&request).expect("the expired process is ended and preserved");
    const DEADLINE_TOLERANCE_MS: u64 = 2_000;
    assert!(
        outcome.exit.is_none()
            && started.elapsed()
                < std::time::Duration::from_millis(SHORT_DECISION_MS + DEADLINE_TOLERANCE_MS),
        "a hanging agent must be killed by its Decision deadline"
    );
    let followup = kaisa_daemon::run_goal(&request);
    assert!(
        followup.is_err_and(|error| error.not_admitted()),
        "reviewer and revision dispatches must not reuse the expired permission"
    );
    let ledger = kaisa_ledger::EventStore::open(&state.join("kaisa.db")).expect("the ledger opens");
    let events = ledger
        .read_from(kaisa_domain::Sequence::FIRST)
        .expect("events read");
    assert!(
        events.iter().any(|event| event
            .recorded
            .event
            .payload
            .contains("Decision execution deadline reached")),
        "the durable record must explain why the running agent was ended"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_claude_reduced_developer_keeps_the_local_first_pass_confined() {
    let bench = Workbench::new("claude-local-scope");
    let repository = bench.base.join("repo");
    let state = bench.base.join("state");
    let fake = PathBuf::from(env!("CARGO_BIN_EXE_kaisa")).with_file_name("kaisa-fake-agent");
    let claude = bench.base.join("claude");
    std::os::unix::fs::symlink(&fake, &claude).expect("a test Claude executable");
    let scripted = |program: PathBuf, config: &Path, words: &[&str]| kaisa_sys::ProcessSpec {
        program,
        args: [
            vec![
                "--mcp-config".to_owned(),
                config.to_string_lossy().into_owned(),
            ],
            words.iter().map(|word| (*word).to_owned()).collect(),
        ]
        .concat(),
        working_directory: repository.clone(),
        env: Vec::new(),
    };
    let developer =
        |config: &Path| scripted(claude.clone(), config, &["--fake-write-next", "src/step"]);
    let reviewer = |config: &Path| {
        scripted(
            claude.clone(),
            config,
            &["--fake-review-require", "src/step-1.md"],
        )
    };
    let local = |config: &Path| {
        scripted(
            fake.clone(),
            config,
            &["--fake-review-require", "src/step-1.md"],
        )
    };
    let basis = kaisa_domain::ReducedModeBasis::DefaultPolicy(
        kaisa_domain::DefaultReducedPolicy::ClaudeReducedUntilT16,
    );
    const LOCAL_MODEL_PORT: u16 = 12345;
    let policy = kaisa_domain::Policy::personal_default()
        .snapshot()
        .expect("a valid policy");
    let write_set = kaisa_domain::WriteSet::new(vec![
        kaisa_domain::RepoPath::try_new("src").expect("a path"),
    ]);
    let outcome = kaisa_daemon::run_verified_goal(&kaisa_daemon::VerifiedRequest {
        repository: &repository,
        state_directory: &state,
        goal: GOAL,
        write_set: &write_set,
        developer: &developer,
        reviewer: &reviewer,
        kaisa_executable: Path::new(env!("CARGO_BIN_EXE_kaisa")),
        confine_agent: false,
        checks: &[],
        policy: &policy,
        observed: false,
        first_pass: Some(kaisa_daemon::FirstPass {
            launch: &local,
            model_port: Some(LOCAL_MODEL_PORT),
        }),
        egress_hosts: &[],
        agent_writable: &[],
        reduced_mode: &basis,
    })
    .expect("Claude's policy does not leak into the local first pass");
    const FIRST_PASS_INDEX: usize = 1;
    let first_pass = &outcome.attempts[FIRST_PASS_INDEX];
    assert!(
        first_pass.confined && first_pass.admitted_on == kaisa_domain::ReducedModeBasis::None,
        "the first pass must use its own loopback-only sandbox and need no Claude permission"
    );
}

#[test]
fn a_claude_default_cannot_spawn_a_scripted_or_local_model_process() {
    let bench = Workbench::new("policy-executable-scope");
    let repository = bench.base.join("repo");
    let state = bench.base.join("state");
    let basis = kaisa_domain::ReducedModeBasis::DefaultPolicy(
        kaisa_domain::DefaultReducedPolicy::ClaudeReducedUntilT16,
    );
    let write_set = kaisa_domain::WriteSet::default();
    let launch = |_: &Path| kaisa_sys::ProcessSpec {
        program: PathBuf::from(env!("CARGO_BIN_EXE_kaisa")),
        args: vec!["--version".to_owned()],
        working_directory: repository.clone(),
        env: Vec::new(),
    };
    const LOCAL_MODEL_PORT: u16 = 12345;
    for port in [None, Some(LOCAL_MODEL_PORT)] {
        let mut request =
            a_reduced_daemon_request(&repository, &state, &launch, &basis, &write_set);
        request.local_model_port = port;
        assert!(
            kaisa_daemon::run_goal(&request).is_err_and(|error| error.not_admitted()),
            "the spawn boundary must bind Claude's default to the actual agent kind (port {port:?})"
        );
    }
}

/// A headless Claude result can report success after every useful tool was denied.
#[cfg(unix)]
fn a_denied_claude_result_script() -> String {
    let result = serde_json::json!({
        "type": "result", "subtype": "success", "is_error": false,
        "permission_denials": [{"tool_name": "mcp__kaisa__fs_read"}]
    });
    format!("cat >/dev/null; printf '%s\\n' '{result}'")
}

#[cfg(unix)]
#[test]
fn an_agent_denied_a_tool_explains_why_it_produced_no_successful_change() {
    let bench = Workbench::new("permission-denied");
    use std::os::unix::fs::PermissionsExt;

    let agent = bench.base.join("denied-agent");
    std::fs::write(
        &agent,
        format!("#!/bin/sh\n{}\n", a_denied_claude_result_script()),
    )
    .expect("the scripted agent is written");
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700))
        .expect("the scripted agent is executable");
    let run = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args([
            "--agent-program",
            &agent.to_string_lossy(),
            "--reviewer-arg",
            "unused-because-the-developer-made-no-change",
            GOAL,
        ])
        .current_dir(bench.base.join("repo"))
        .env("KAISA_STATE_DIR", bench.base.join("state"))
        .output()
        .expect("kaisa runs");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        !run.status.success()
            && output.contains("에이전트가 도구 권한을 거부당했다: mcp__kaisa__fs_read"),
        "the CLI must surface the denied tool even when Claude exits zero: {output}"
    );
}
