//! The terminal home's accessible CLI contract over real fake-agent verification runs.

#[path = "../../../crates/kaisa-cli/src/tui/test_support.rs"]
mod test_support;

use test_support::{VERIFIED_GOAL, WAITING_GOAL, Workbench, directory_bytes};

const EXIT_SUCCESS: i32 = 0;
const EXIT_USAGE: i32 = 64;

#[test]
fn plain_and_non_tty_output_show_the_same_complete_frame_without_writing_state() {
    let bench = Workbench::recorded_home("cli-plain");
    let before = directory_bytes(&bench.state());
    let plain = bench
        .command()
        .args(["tui", "--plain"])
        .output()
        .expect("plain command");
    let non_tty = bench.command().arg("tui").output().expect("piped command");
    assert_eq!(
        plain.status.code(),
        Some(EXIT_SUCCESS),
        "plain mode is a successful one-frame read"
    );
    assert_eq!(
        non_tty.status.code(),
        Some(EXIT_SUCCESS),
        "non-TTY mode must not require raw mode"
    );
    assert_eq!(
        plain.stdout, non_tty.stdout,
        "piping tui must provide the same accessible information as --plain"
    );
    let text = String::from_utf8(plain.stdout).expect("UTF-8");
    let needs = text.find(WAITING_GOAL).expect("waiting goal shown");
    let delivered = text.find(VERIFIED_GOAL).expect("delivered goal shown");
    assert!(
        needs < delivered,
        "the needs-you lane must precede delivered goals"
    );
    for expected in [
        "Gate: NeedsPerson",
        "Reason:",
        "developer",
        "reviewer",
        "revision 1",
        "Execution:",
        "cleanup:",
        "tokens",
        "cost",
        "Check:",
        "exit:",
        "Finding:",
        "git merge --ff-only",
        "Transcript tail:",
    ] {
        assert!(
            text.contains(expected),
            "the accessible frame must include {expected:?}: {text}"
        );
    }
    assert!(
        text.contains("reduced") || text.contains("confined"),
        "the frame must report the containment actually recorded on this platform"
    );
    assert!(
        !text.contains('\u{1b}'),
        "plain output must contain no ANSI control sequences"
    );
    assert_eq!(
        directory_bytes(&bench.state()),
        before,
        "plain mode and implicit non-TTY mode must preserve every state file"
    );
}

#[test]
fn an_empty_home_prints_one_frame_and_never_initializes_the_ledger() {
    let bench = Workbench::new("cli-empty");
    let output = bench
        .command()
        .args(["tui", "--plain"])
        .output()
        .expect("empty home");
    assert!(
        output.status.success(),
        "an empty home is successful: {output:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("nothing has run yet"),
        "first-run output explains that no work exists"
    );
    assert!(
        !bench.state().exists(),
        "read-only first-run output must not create state"
    );
}

#[test]
fn unknown_tui_options_fail_with_usage_and_never_start_an_agent() {
    let bench = Workbench::new("cli-usage");
    let output = bench
        .command()
        .args(["tui", "--not-an-option"])
        .output()
        .expect("usage check");
    assert_eq!(
        output.status.code(),
        Some(EXIT_USAGE),
        "unknown options keep the existing CLI usage contract"
    );
    assert!(
        output.stdout.is_empty() && !bench.state().exists(),
        "a mistyped TUI option cannot initialize a run"
    );
}

#[test]
fn the_korean_catalog_translates_labels_and_preserves_recorded_goal_text() {
    let bench = Workbench::recorded_home("cli-korean");
    let output = bench
        .command()
        .env("KAISA_LANG", "ko")
        .args(["tui", "--plain"])
        .output()
        .expect("Korean plain frame");
    let text = String::from_utf8(output.stdout).expect("UTF-8 Korean");
    assert!(
        text.contains("사람을 기다려요") && text.contains("목표:") && text.contains(VERIFIED_GOAL),
        "ko/en labels come from the catalog while the ledger's goal stays literal: {text}"
    );
}
