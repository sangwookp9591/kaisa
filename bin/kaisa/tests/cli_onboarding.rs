//! User-facing setup flows with isolated homes, state and version-only stand-ins.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use kaisa_domain::{Event, EventKind};
use kaisa_ledger::EventStore;

const SUCCESS: i32 = 0;
const FAILURE: i32 = 1;
const WAITING: i32 = 3;
const NOT_DELIVERED: i32 = 4;
const NOTES: &str = "src/notes.md";

struct Project {
    base: PathBuf,
}
impl Project {
    fn rust(name: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("kaisa-onboarding-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("repo/src")).expect("fixture directory");
        std::fs::create_dir_all(base.join("home")).expect("isolated home");
        std::fs::create_dir_all(base.join("tools")).expect("version stand-ins");
        for agent in ["claude", "codex"] {
            std::fs::copy(
                env!("CARGO_BIN_EXE_kaisa"),
                base.join("tools")
                    .join(format!("{agent}{}", std::env::consts::EXE_SUFFIX)),
            )
            .expect("version-only CLI copy");
        }
        let project = Self { base };
        project.write(
            "Cargo.toml",
            "[package]\nname='fixture'\nversion='0.1.0'\nedition='2024'\n",
        );
        project.write("src/lib.rs", "pub fn ready() {}\n");
        project.git(&["init", "--quiet", "--initial-branch=main"]);
        // Git's automatic background maintenance after a commit can create and remove
        // `.git/objects/maintenance.lock` at any moment, which would race the read-only snapshots.
        project.git(&["config", "maintenance.auto", "false"]);
        project.git(&["config", "gc.auto", "0"]);
        project.commit();
        project
    }
    fn repo(&self) -> PathBuf {
        self.base.join("repo")
    }
    fn write(&self, path: &str, text: &str) {
        let destination = self.repo().join(path);
        std::fs::create_dir_all(destination.parent().expect("file parent"))
            .expect("fixture parent");
        std::fs::write(destination, text).expect("fixture file");
    }
    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .env("HOME", self.base.join("home"))
            .env("USERPROFILE", self.base.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.base.join("home/gitconfig"))
            .args(args)
            .current_dir(self.repo())
            .output()
            .expect("git");
        assert!(output.status.success(), "fixture git {args:?}: {output:?}");
    }
    fn commit(&self) {
        self.git(&["add", "."]);
        self.git(&[
            "-c",
            "user.name=Kaisa Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ]);
    }
    fn run_in(&self, directory: &Path, args: &[&str]) -> Output {
        let mut paths = vec![self.base.join("tools")];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        Command::new(env!("CARGO_BIN_EXE_kaisa"))
            .args(args)
            .current_dir(directory)
            .env("HOME", self.base.join("home"))
            .env("USERPROFILE", self.base.join("home"))
            .env("KAISA_STATE_DIR", self.base.join("state"))
            .env("PATH", std::env::join_paths(paths).expect("paths"))
            .stdin(std::process::Stdio::null())
            .output()
            .expect("kaisa")
    }
    fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.repo(), args)
    }
    fn verified(&self, target: &str) -> Output {
        self.verified_with_content(target, "Project notes.\n")
    }
    fn verified_with_content(&self, target: &str, content: &str) -> Output {
        let fake = PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
            .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
        self.run(&[
            "--agent-program",
            &fake.to_string_lossy(),
            "--agent-arg",
            "--fake-write",
            "--agent-arg",
            target,
            "--agent-arg",
            "--fake-content",
            "--agent-arg",
            content,
            "--reviewer-arg",
            "--fake-review-require",
            "--reviewer-arg",
            target,
            "Write project notes.",
        ])
    }
    fn configured_check(&self, program: &str, args: &[&str]) {
        let argv = std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>();
        self.write(
            "kaisa.toml",
            &format!(
                "[profile]\ntest = {}\n",
                serde_json::to_string(&argv).expect("argv")
            ),
        );
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}
fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}
#[derive(PartialEq, Eq)]
struct SnapshotFile {
    bytes: Vec<u8>,
    modified: std::time::SystemTime,
}
impl std::fmt::Debug for SnapshotFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SnapshotFile")
            .field("length", &self.bytes.len())
            .field("modified", &self.modified)
            .finish()
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, SnapshotFile> {
    fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, SnapshotFile>) {
        if !directory.exists() {
            return;
        }
        for entry in std::fs::read_dir(directory).expect("snapshot directory") {
            let path = entry.expect("snapshot entry").path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).expect("relative").to_owned(),
                    SnapshotFile {
                        bytes: std::fs::read(&path).expect("snapshot file"),
                        modified: std::fs::metadata(&path)
                            .expect("snapshot metadata")
                            .modified()
                            .expect("modification time"),
                    },
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

#[test]
fn doctor_in_a_clean_rust_repository_reports_checks_and_leaves_repo_home_and_state_untouched() {
    let project = Project::rust("doctor-clean");
    let before = snapshot(&project.base);
    let diagnosis = project.run(&["doctor"]);
    let report = text(&diagnosis);
    let platform_can_admit = !matches!(
        kaisa_domain::admit(&kaisa_daemon::expected_sandbox_missing(true), None, 0),
        kaisa_domain::Admission::Refused { .. }
    );
    assert_eq!(
        diagnosis.status.code(),
        Some(if platform_can_admit { SUCCESS } else { FAILURE }),
        "required checks determine exit status: {report}"
    );
    for expected in [
        "✓ kaisa",
        "Agent: claude",
        "login not checked",
        "branch main, clean",
        "cargo test --workspace",
        "Protection preflight",
        "State:",
    ] {
        assert!(
            report.contains(expected),
            "doctor must explain {expected}: {report}"
        );
    }
    assert_eq!(
        snapshot(&project.base),
        before,
        "read-only diagnosis must not create state, refresh the git index, or write HOME"
    );
    assert!(
        !project.base.join("state").exists(),
        "doctor must not create an absent state directory"
    );
}

#[test]
fn doctor_outside_a_repository_fails_with_a_concrete_next_step() {
    let project = Project::rust("doctor-outside");
    let output = project.run_in(&project.base.join("home"), &["doctor"]);
    assert_eq!(
        output.status.code(),
        Some(FAILURE),
        "a goal requires a repository"
    );
    assert!(
        text(&output).contains("git init and commit"),
        "diagnosis must offer a concrete fix"
    );
}

#[test]
fn doctor_without_tests_or_with_risky_settings_fails_and_names_the_problem() {
    let project = Project::rust("doctor-problems");
    std::fs::remove_file(project.repo().join("Cargo.toml")).expect("remove manifest");
    let no_tests = project.run(&["doctor"]);
    assert_eq!(
        no_tests.status.code(),
        Some(FAILURE),
        "no trusted tests prevents delivery"
    );
    assert!(
        text(&no_tests).contains("no test command found → goals here end not delivered"),
        "missing tests need honest wording"
    );
    project.write(
        ".claude/settings.json",
        r#"{"enableAllProjectMcpServers":true}"#,
    );
    let risky = project.run(&["doctor"]);
    assert_eq!(
        risky.status.code(),
        Some(FAILURE),
        "risky settings must fail diagnosis"
    );
    assert!(
        text(&risky).contains("ClaudeAutoApprovesMcpServers"),
        "every risk finding must be reported"
    );
}

#[test]
fn home_without_a_ledger_welcomes_the_user_without_creating_state() {
    let project = Project::rust("home-first");
    let before = snapshot(&project.base);
    let output = project.run(&[]);
    assert_eq!(
        output.status.code(),
        Some(SUCCESS),
        "Home offers setup even when optional checks warn"
    );
    for expected in [
        "Welcome to Kaisa.",
        "Claude Code develops and reviews",
        "checks run in a sandbox",
        "Try it now:",
        "kaisa watch",
        "kaisa review",
    ] {
        assert!(
            text(&output).contains(expected),
            "Home must teach {expected}"
        );
    }
    assert_eq!(
        snapshot(&project.base),
        before,
        "Home must not initialize any state"
    );
}

#[test]
fn home_with_delivered_and_waiting_goals_reads_the_latest_gate_without_writing() {
    let project = Project::rust("home-later");
    let state = project.base.join("state");
    std::fs::create_dir(&state).expect("ledger directory");
    let mut ledger = EventStore::open(&state.join("kaisa.db")).expect("fixture ledger");
    for (run, decision) in [("run_delivered", "Deliver"), ("run_waiting", "NeedsPerson")] {
        for (kind, payload) in [
            (
                EventKind::RunStarted,
                serde_json::json!({"goal":"Write notes"}),
            ),
            (
                EventKind::GateDecided,
                serde_json::json!({"run_id":run,"decision":decision,"reason":"check definition changed"}),
            ),
        ] {
            ledger
                .append(Event {
                    kind,
                    subject: run.into(),
                    payload_version: 1,
                    payload: payload.to_string(),
                })
                .expect("record fixture");
        }
    }
    drop(ledger);
    let before = snapshot(&project.base);
    let output = project.run(&[]);
    assert!(
        text(&output).contains("run_delivered: Write notes — Deliver"),
        "recent delivered goal must be visible"
    );
    assert!(
        text(&output).contains("Waiting for you: run_waiting"),
        "NeedsPerson is waiting independently of recent list truncation"
    );
    assert_eq!(
        snapshot(&project.base),
        before,
        "reading existing ledger must not write sidecars or projections"
    );
}

#[test]
fn init_without_a_terminal_only_previews_and_yes_creates_an_idempotent_profile() {
    let project = Project::rust("init");
    let before = snapshot(&project.base);
    let preview = project.run(&["init"]);
    assert_eq!(
        preview.status.code(),
        Some(SUCCESS),
        "preview is a successful read-only command"
    );
    assert!(
        text(&preview).contains("Preview only"),
        "non-interactive users need an explicit apply command"
    );
    assert_eq!(
        snapshot(&project.base),
        before,
        "preview must change nothing"
    );
    let applied = project.run(&["init", "--yes"]);
    assert_eq!(
        applied.status.code(),
        Some(SUCCESS),
        "explicit yes applies setup: {applied:?}"
    );
    let after = snapshot(&project.base);
    assert!(
        project.repo().join("kaisa.toml").exists(),
        "init writes project checks"
    );
    let repeated = project.run(&["init", "--yes"]);
    assert_eq!(
        repeated.status.code(),
        Some(SUCCESS),
        "repeat setup succeeds"
    );
    assert_eq!(
        snapshot(&project.base),
        after,
        "repeat setup must preserve existing bytes"
    );
}

/// Where `kaisa init` puts the `/kaisa` slash command for Claude Code.
const SLASH_COMMAND: &str = ".claude/commands/kaisa.md";

#[test]
fn init_adds_the_kaisa_slash_command_and_prints_the_user_scope_registration() {
    let project = Project::rust("init-claude");
    let applied = project.run(&["init", "--yes"]);
    assert_eq!(
        applied.status.code(),
        Some(SUCCESS),
        "explicit yes applies setup: {applied:?}"
    );
    let command = std::fs::read_to_string(project.repo().join(SLASH_COMMAND))
        .expect("init writes the /kaisa command");
    assert!(
        command.contains("kaisa_start_goal") && command.contains("Never merge"),
        "the command starts goals through Kaisa and forbids merging: {command}"
    );
    assert!(
        text(&applied).contains("claude mcp add kaisa -- kaisa mcp serve"),
        "the MCP server is registered by the person at user scope, so init prints how"
    );
    assert!(
        !project.repo().join(".mcp.json").exists(),
        "init does not write a project MCP server"
    );
}

#[test]
fn init_leaves_a_different_existing_slash_command_alone_and_says_so() {
    let project = Project::rust("init-claude-conflict");
    let own = "my own command\n";
    std::fs::create_dir_all(project.repo().join(".claude/commands")).expect("commands directory");
    std::fs::write(project.repo().join(SLASH_COMMAND), own).expect("existing command");
    let applied = project.run(&["init", "--yes"]);
    assert_eq!(
        std::fs::read_to_string(project.repo().join(SLASH_COMMAND)).expect("command still there"),
        own,
        "a person's own /kaisa command is never overwritten"
    );
    assert!(
        text(&applied).contains("Not changing"),
        "the skipped file is named: {}",
        text(&applied)
    );
}

#[test]
fn a_goal_uses_the_configured_test_without_a_check_program_override() {
    let project = Project::rust("configured-check");
    if cfg!(windows) {
        project.configured_check(
            "cmd",
            &["/c", r"if exist src\notes.md (exit 0) else (exit 1)"],
        );
    } else {
        project.configured_check("sh", &["-c", "test -f src/notes.md"]);
    }
    project.commit();
    let output = project.verified(NOTES);
    assert_eq!(
        output.status.code(),
        Some(SUCCESS),
        "configured test verifies fake-agent work: {} {:?}",
        text(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text(&output)
            .lines()
            .any(|line| line.starts_with("verified")),
        "success must be a verified result (on Windows the check is labelled reduced): {}",
        text(&output)
    );
    if cfg!(windows) {
        project.configured_check("cmd", &["/c", "exit 1"]);
    } else {
        project.configured_check("sh", &["-c", "exit 1"]);
    }
    project.commit();
    let failing = project.verified(NOTES);
    assert_eq!(
        failing.status.code(),
        Some(NOT_DELIVERED),
        "a configured failing test cannot fall back to detected cargo tests: {}",
        text(&failing)
    );
}

#[test]
fn a_candidate_editing_kaisa_toml_stops_for_a_person() {
    let project = Project::rust("changed-check");
    if cfg!(windows) {
        project.configured_check("cmd", &["/c", "exit 0"]);
    } else {
        project.configured_check("sh", &["-c", "exit 0"]);
    }
    project.commit();
    let delivered = project.verified(NOTES);
    assert_eq!(
        delivered.status.code(),
        Some(SUCCESS),
        "fake-agent delivered goal seeds Home: {}",
        text(&delivered)
    );
    let output = project.verified("kaisa.toml");
    assert_eq!(
        output.status.code(),
        Some(WAITING),
        "candidate-owned checks require a person: {} {:?}",
        text(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text(&output).contains("changed what its checks run: kaisa.toml"),
        "the person must know why delivery stopped: {}",
        text(&output)
    );
    let before = snapshot(&project.base);
    let home = project.run(&[]);
    assert!(
        text(&home).contains("Deliver")
            && text(&home).contains("Waiting for you:")
            && text(&home).contains("NeedsPerson"),
        "Home reads delivered and waiting goals from real fake-agent runs: {}",
        text(&home)
    );
    assert_eq!(
        snapshot(&project.base),
        before,
        "Home must not change a ledger produced by agents"
    );
}

#[cfg(unix)]
#[test]
fn doctor_does_not_execute_git_clean_filters_or_fsmonitor_hooks() {
    for filter in ["diagnostic-test", "side=effect"] {
        let project = Project::rust(&format!("doctor-git-programs-{filter}"));
        project.write(".gitattributes", &format!("src/lib.rs filter={filter}\n"));
        project.git(&[
            "config",
            &format!("filter.{filter}.clean"),
            "touch filter-ran; cat",
        ]);
        project.git(&["config", "core.fsmonitor", "touch fsmonitor-ran"]);
        // Same length as the original source makes Git hash the content and invoke a clean
        // filter if the read-only guard is absent, instead of stopping at a size mismatch.
        project.write("src/lib.rs", "pub fn later() {}\n");
        let before = snapshot(&project.base);
        let output = project.run(&["doctor"]);
        assert!(
            text(&output).contains("branch main, dirty")
                || text(&output).contains("status not checked"),
            "disabled Git programs must still permit a conservative dirty-tree report"
        );
        assert_eq!(
            snapshot(&project.base),
            before,
            "read-only diagnosis must disable Git programs that can write or access the network"
        );
    }
}

#[test]
fn an_empty_profile_uses_tests_detected_after_init() {
    let project = Project::rust("empty-profile");
    let manifest =
        std::fs::read_to_string(project.repo().join("Cargo.toml")).expect("detected manifest");
    std::fs::remove_file(project.repo().join("Cargo.toml")).expect("start without checks");
    project.run(&["init", "--yes"]);
    project.write("Cargo.toml", &manifest);
    let output = project.run(&["doctor"]);
    assert!(
        text(&output).contains("cargo test --workspace"),
        "empty configuration must not pin the absence of checks: {}",
        text(&output)
    );
}

#[test]
fn valid_explicit_checks_survive_a_malformed_package_manifest_with_a_warning() {
    let project = Project::rust("broken-package");
    project.configured_check("trusted-check", &["--test"]);
    project.write("package.json", "{broken");
    let output = project.run(&["doctor"]);
    let report = text(&output);
    assert!(
        report.contains("trusted-check --test"),
        "explicit checks must win: {report}"
    );
    assert!(
        report.contains("! package.json") && report.contains("warning"),
        "the broken manifest must remain visible as a warning: {report}"
    );
}

#[cfg(unix)]
#[test]
fn a_candidate_rewriting_a_script_named_by_explicit_checks_stops_for_a_person() {
    assert_script_edit_waits_for_a_person("script-explicit", true);
}

#[cfg(unix)]
#[test]
fn a_candidate_rewriting_a_script_named_by_package_tests_stops_for_a_person() {
    assert_script_edit_waits_for_a_person("script-package", false);
}

#[cfg(unix)]
fn assert_script_edit_waits_for_a_person(name: &str, explicit: bool) {
    let project = Project::rust(name);
    let script = "scripts/check.sh";
    project.write(script, "exit 1\n");
    if explicit {
        project.configured_check("sh", &[script]);
    } else {
        project.write(
            "package.json",
            r#"{"scripts":{"test":"sh scripts/check.sh"}}"#,
        );
        // A local package-manager stand-in runs the trusted script without a Node install.
        write_executable(
            &project.base.join("tools/npm"),
            "#!/bin/sh\nexec sh scripts/check.sh\n",
        );
    }
    project.commit();
    let output = project.verified_with_content(script, "exit 0\n");
    assert_eq!(
        output.status.code(),
        Some(WAITING),
        "rewriting a referenced check requires a person: {} {:?}",
        text(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text(&output).contains(script),
        "the changed script must be named: {}",
        text(&output)
    );
}

#[cfg(unix)]
fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).expect("executable fixture");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("executable permissions");
}

#[cfg(unix)]
#[test]
fn hung_version_probes_return_a_did_not_answer_diagnosis_within_a_deadline() {
    let project = Project::rust("hung-version");
    write_executable(
        &project.base.join("tools/claude"),
        "#!/bin/sh\nwhile :; do sleep 1; done\n",
    );
    const PATIENCE: std::time::Duration = std::time::Duration::from_secs(8);
    for args in [vec!["doctor"], vec![]] {
        let started = std::time::Instant::now();
        let mut paths = vec![project.base.join("tools")];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let spec = kaisa_sys::ProcessSpec {
            program: PathBuf::from(env!("CARGO_BIN_EXE_kaisa")),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            working_directory: project.repo(),
            env: vec![
                (
                    "PATH".into(),
                    std::env::join_paths(paths)
                        .expect("test path")
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    "HOME".into(),
                    project.base.join("home").to_string_lossy().into_owned(),
                ),
                (
                    "KAISA_STATE_DIR".into(),
                    project.base.join("state").to_string_lossy().into_owned(),
                ),
            ],
        };
        const OUTPUT_CAP: usize = 64 * 1024;
        let output = kaisa_sys::run_captured(
            &spec,
            &kaisa_sys::Confinement::Unconfined,
            &[],
            PATIENCE,
            OUTPUT_CAP,
        )
        .expect("bounded diagnosis");
        let report = String::from_utf8_lossy(&output.output);
        assert!(
            output.exit.is_some() && started.elapsed() < PATIENCE,
            "doctor and first-run Home must bound version probes: {report}"
        );
        assert!(
            report.contains("claude did not answer"),
            "a timeout must be distinguished from a missing agent: {report}"
        );
    }
}

#[cfg(unix)]
#[test]
fn terminal_init_waits_for_confirmation_and_only_enter_applies_the_preview() {
    use std::io::{Read, Write};

    use kaisa_sys::{OwnedProcess, ProcessSpec, PseudoTerminal, TerminalSize};
    const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);
    const ROWS: u16 = 24;
    const COLUMNS: u16 = 100;
    for (name, answer, applies) in [
        ("enter", "\n", true),
        ("quit", "q\n", false),
        ("garbage", "wrong\n", false),
        ("eof", "\x04", false),
    ] {
        let project = Project::rust(&format!("tty-{name}"));
        let spec = ProcessSpec {
            program: PathBuf::from(env!("CARGO_BIN_EXE_kaisa")),
            args: vec!["init".into()],
            working_directory: project.repo(),
            env: vec![(
                "KAISA_STATE_DIR".into(),
                project.base.join("state").to_string_lossy().into_owned(),
            )],
        };
        let (mut process, terminal) = OwnedProcess::spawn_in_terminal(
            &spec,
            TerminalSize::new(ROWS, COLUMNS).expect("terminal size"),
        )
        .expect("terminal init");
        let mut reader = terminal.reader().expect("terminal output");
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 1024];
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 || send.send(buffer[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        let deadline = std::time::Instant::now() + PATIENCE;
        let mut output = Vec::new();
        while !String::from_utf8_lossy(&output).contains("Cancel:") {
            output.extend(
                receive
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .expect("init must prompt before writing"),
            );
        }
        assert!(
            !project.repo().join("kaisa.toml").exists(),
            "TTY preview must wait before writing"
        );
        terminal
            .writer()
            .expect("terminal input")
            .write_all(answer.as_bytes())
            .expect("answer confirmation");
        let exit = process
            .wait_timeout(PATIENCE)
            .expect("wait for confirmation");
        process.kill_tree().expect("clean terminal process tree");
        assert_eq!(
            exit,
            Some(kaisa_sys::ProcessExit::Code(SUCCESS)),
            "confirmation or cancellation should succeed"
        );
        assert_eq!(
            project.repo().join("kaisa.toml").exists(),
            applies,
            "only Enter authorizes the write for {name}"
        );
    }
}
