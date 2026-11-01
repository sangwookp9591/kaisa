//! Command mistakes must be refused before any attempt can change state or Git metadata.

use std::path::Path;
use std::process::{Command, Output};

use support::{Workbench, fake_agent_beside, git};

mod support;

const EXIT_USAGE: i32 = 64;
const COMMANDS: &[&str] = &[
    "doctor",
    "init",
    "help",
    "version",
    "status",
    "watch",
    "tui",
    "local-agent",
    "agent",
    "spec",
    "build",
    "policy",
    "route",
    "stats",
    "mcp",
    "goal",
    "review",
    "ask",
    "attach",
    "steer",
    "pause",
    "resume",
    "cancel",
    "undo",
    "restore",
    "console",
    "memory",
    "team",
    "admin",
    "run",
    "keeper",
    "audit",
    "hook",
    "wrap",
    "uninstall",
    "retain",
    "handoff",
    "handback",
    "gc",
    "daemon",
];

fn invoke(bench: &Workbench, repository: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .args(args)
        .current_dir(repository)
        .env("KAISA_STATE_DIR", bench.state())
        .output()
        .expect("the CLI runs")
}

fn assert_refused_without_effects(args: &[&str], suggestion: Option<&str>) {
    let bench = Workbench::new(&format!("unknown-{}", args[0]));
    let repository = bench.repository("repo");
    // Preserve existing state bytes as well as proving no state is created on first use.
    for existing_state in [false, true] {
        if existing_state {
            std::fs::create_dir_all(bench.state()).expect("state directory");
            std::fs::write(bench.state().join("sentinel"), "unchanged").expect("sentinel");
        }
        let branches = git(&repository, &["show-ref"]);
        let worktrees = git(&repository, &["worktree", "list", "--porcelain"]);
        let output = invoke(&bench, &repository, args);
        assert_eq!(
            git(&repository, &["show-ref"]),
            branches,
            "refusal must not create attempt or result branches"
        );
        assert_eq!(
            git(&repository, &["worktree", "list", "--porcelain"]),
            worktrees,
            "refusal must not create worktrees"
        );
        if existing_state {
            let entries: Vec<_> = std::fs::read_dir(bench.state())
                .expect("existing state")
                .collect();
            let expected_entries = ["sentinel"];
            assert_eq!(
                entries.len(),
                expected_entries.len(),
                "refusal must not create a ledger, attempt or worktree in state"
            );
            assert_eq!(
                std::fs::read_to_string(bench.state().join("sentinel")).expect("sentinel"),
                "unchanged",
                "existing state bytes must remain unchanged"
            );
        } else {
            assert!(
                !bench.state().exists(),
                "refusal must not initialize any state: {output:?}"
            );
        }
        assert_eq!(
            output.status.code(),
            Some(EXIT_USAGE),
            "unknown commands must exit with usage error: {output:?}"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("unknown command")
                && error.contains("kaisa --")
                && error.contains("kaisa goal"),
            "refusal must explain the explicit goal forms: {error}"
        );
        if let Some(command) = suggestion {
            assert!(
                error.contains(&format!("did you mean `{command}`?")),
                "a close command must be suggested: {error}"
            );
        }
    }
}

#[test]
fn a_typo_of_each_command_is_refused_with_a_suggestion_and_no_state_or_git_changes() {
    for command in COMMANDS {
        let typo = format!("{command}x");
        assert_refused_without_effects(&[&typo], Some(command));
        assert_refused_without_effects(&[&typo, "draft", "requirements"], Some(command));
    }
    assert_refused_without_effects(&["stauts"], Some("status"));
    assert_refused_without_effects(&["spce", "draft", "requirements"], Some("spec"));
}

#[test]
fn an_unrecognized_one_word_goal_is_refused_without_state_or_git_changes() {
    assert_refused_without_effects(&["frobnicate"], None);
}

#[test]
fn explicit_goal_forms_and_multi_word_goals_still_run_the_fake_agent() {
    let fake = fake_agent_beside(Path::new(env!("CARGO_BIN_EXE_kaisa")));
    for (name, words) in [
        ("separator", vec!["--", "word"]),
        (
            "separator-typo",
            vec!["--", "spce", "draft", "requirements"],
        ),
        ("goal-alias", vec!["goal", "word"]),
        ("goal-command-name", vec!["goal", "status"]),
        ("quoted", vec!["fix the login bug"]),
        ("several", vec!["fix", "the", "login", "bug"]),
    ] {
        let bench = Workbench::new(name);
        let repository = bench.repository("repo");
        let mut args = if words[0] == "goal" {
            vec!["goal"]
        } else {
            vec![]
        };
        args.extend([
            "--agent-program",
            fake.to_str().expect("fake path"),
            "--agent-arg",
            "--fake-write",
            "--agent-arg",
            "src/notes.md",
        ]);
        args.extend(if words[0] == "goal" {
            &words[1..]
        } else {
            &words
        });
        let output = invoke(&bench, &repository, &args);
        let goal_words = if matches!(words[0], "goal" | "--") {
            &words[1..]
        } else {
            &words
        };
        let expected_goal = format!("▶ {}", goal_words.join(" "));
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&expected_goal),
            "explicit syntax must preserve the goal text without the command prefix: {output:?}"
        );
        assert!(
            output.status.success(),
            "explicit and multi-word goals must run the fake agent: {output:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("work preserved on kaisa/att_"),
            "the fake agent must actually complete an attempt: {output:?}"
        );
    }
}

#[test]
fn the_cli_version_reports_the_release_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_kaisa"))
        .arg("--version")
        .output()
        .expect("version runs");
    assert!(output.status.success(), "version must succeed");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "kaisa 0.1.0\n",
        "the workspace release version must reach the binary"
    );
}

#[test]
fn a_close_command_before_more_words_is_refused_without_effects() {
    assert_refused_without_effects(&["spce", "draft", "requirements"], Some("spec"));
}

#[test]
fn the_explicit_goal_command_without_text_returns_usage_without_effects() {
    let bench = Workbench::new("empty-explicit-goal");
    let repository = bench.repository("repo");
    let output = invoke(&bench, &repository, &["goal"]);
    assert_eq!(
        output.status.code(),
        Some(EXIT_USAGE),
        "explicit goals need goal text: {output:?}"
    );
    assert!(
        !bench.state().exists(),
        "missing explicit goal text must not start an attempt"
    );
}
