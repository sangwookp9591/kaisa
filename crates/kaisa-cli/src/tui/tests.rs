//! Behaviour tests for F10 using recorded fake-agent runs and Ratatui's memory backend.

use std::cell::Cell;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::model::{Snapshot, details, load, safe};
use super::{Action, App, Filter, Input, RestoreGuard, goal_command, plain, render};

#[path = "test_support.rs"]
mod test_support;
use test_support::{FAILED_GOAL, VERIFIED_GOAL, WAITING_GOAL, Workbench, directory_bytes};

const SCREEN_WIDTH: u16 = 180;
const SCREEN_HEIGHT: u16 = 70;
const FIRST_ROW: usize = 0;
const NEXT_ROW: usize = FIRST_ROW + 1;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn a_home(name: &str) -> (Workbench, App) {
    let bench = Workbench::recorded_home(name);
    let app = App::new(load(&bench.state()).expect("fake-agent ledger reads"));
    (bench, app)
}

fn a_screen(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("memory backend");
    terminal
        .draw(|frame| render(frame, app))
        .expect("frame renders");
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn a_recorded_home_renders_needs_you_first_and_the_attempt_tree_with_evidence() {
    let (_bench, mut app) = a_home("render");
    let frame = a_screen(&mut app, SCREEN_WIDTH, SCREEN_HEIGHT);
    assert!(
        app.snapshot.runs[FIRST_ROW].needs_person(),
        "a newer NeedsPerson gate must precede older delivered runs"
    );
    for expected in [
        "Needs you",
        WAITING_GOAL,
        ".claude/settings.json",
        "developer",
        "reviewer",
        "revision 1",
        "exited:0",
        "reclaimed",
        "tokens",
        "Details",
    ] {
        assert!(
            frame.contains(expected),
            "the full-screen home must expose {expected:?}: {frame}"
        );
    }
    let run_index = app
        .snapshot
        .runs
        .iter()
        .position(|run| run.goal == VERIFIED_GOAL)
        .expect("delivered run");
    let run = &app.snapshot.runs[run_index];
    let detail = details(run, None);
    let delivered_branch = format!(
        "git merge --ff-only kaisa/{}",
        run.candidate_attempt.as_ref().expect("candidate attempt")
    );
    for expected in [
        VERIFIED_GOAL,
        "Gate: Deliver",
        "Check:",
        "exit:",
        "Review",
        "Finding: Blocking",
        "Transcript tail:",
        &delivered_branch,
    ] {
        assert!(
            detail.contains(expected),
            "selected run details must contain {expected:?}: {detail}"
        );
    }
    let position = app
        .rows()
        .iter()
        .position(|selected| selected.run == run_index && selected.attempt.is_none())
        .expect("visible run");
    app.list.select(Some(position));
    let frame = a_screen(&mut app, SCREEN_WIDTH, SCREEN_HEIGHT);
    assert!(
        frame.contains("Gate: Deliver") && frame.contains("git merge --ff-only"),
        "the detail pane renders the delivery evidence, not just its data model: {frame}"
    );
}

#[test]
fn filters_search_and_navigation_keep_selection_on_visible_rows() {
    let (_bench, mut app) = a_home("keys");
    app.key(key(KeyCode::Down));
    assert_eq!(
        app.list.selected(),
        Some(NEXT_ROW),
        "down moves to the next visible row"
    );
    app.key(key(KeyCode::Char('k')));
    assert_eq!(
        app.list.selected(),
        Some(FIRST_ROW),
        "k moves back to the first row"
    );
    app.key(key(KeyCode::Char('n')));
    assert_eq!(
        app.filter,
        Filter::NeedsYou,
        "n selects only the recorded NeedsPerson lane"
    );
    assert!(
        app.rows()
            .iter()
            .all(|row| app.snapshot.runs[row.run].needs_person()),
        "needs-you filtering never invents a wait from an execution state"
    );
    app.key(key(KeyCode::Char('n')));
    assert_eq!(
        app.filter,
        Filter::All,
        "the same filter key toggles back to all runs"
    );
    app.key(key(KeyCode::Char('f')));
    assert!(
        app.rows()
            .iter()
            .all(|row| app.snapshot.runs[row.run].failed()),
        "failed filtering includes undelivered runs"
    );
    assert!(
        app.rows()
            .iter()
            .any(|row| app.snapshot.runs[row.run].goal == FAILED_GOAL),
        "the fake reviewer exhausted revision and appears under failed"
    );
    app.key(key(KeyCode::Char('f')));
    app.key(key(KeyCode::Char('/')));
    for character in "unavailable file".chars() {
        app.key(key(KeyCode::Char(character)));
    }
    app.key(key(KeyCode::Enter));
    assert!(
        app.rows()
            .iter()
            .all(|row| app.snapshot.runs[row.run].goal == FAILED_GOAL),
        "search narrows the run tree, including its attempts"
    );
    app.key(key(KeyCode::Char('/')));
    for character in "no such goal".chars() {
        app.key(key(KeyCode::Char(character)));
    }
    assert!(
        app.rows().is_empty() && app.list.selected().is_none(),
        "an empty search has no stale selection"
    );
    app.key(key(KeyCode::Backspace));
    app.key(key(KeyCode::Enter));
    assert_eq!(
        app.input,
        Input::Browse,
        "Enter finishes search without submitting a goal"
    );
}

#[test]
fn a_goal_prompt_starts_only_on_enter_and_ctrl_c_always_quits() {
    let mut app = App::new(Snapshot::default());
    assert_eq!(
        app.key(key(KeyCode::Char('g'))),
        Action::None,
        "opening a prompt must not start a process"
    );
    for character in VERIFIED_GOAL.chars() {
        app.key(key(KeyCode::Char(character)));
    }
    assert_eq!(
        app.key(key(KeyCode::Enter)),
        Action::Start(VERIFIED_GOAL.to_owned()),
        "submitting the prompt emits exactly the typed goal"
    );
    for mode in [
        Input::Browse,
        Input::Search,
        Input::Goal(VERIFIED_GOAL.to_owned()),
    ] {
        app.input = mode;
        let interrupt = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            app.key(interrupt),
            Action::Quit,
            "Ctrl-C must quit even while typing"
        );
    }
    assert_eq!(
        app.key(key(KeyCode::Esc)),
        Action::Quit,
        "Escape always quits"
    );
}

#[test]
fn details_help_and_unsupported_actions_never_emit_engine_mutations() {
    let mut app = App::new(Snapshot::default());
    app.key(key(KeyCode::Enter));
    assert!(app.details_focused, "Enter focuses the detail pane");
    app.key(key(KeyCode::Tab));
    assert!(!app.details_focused, "Tab returns focus to the goal tree");
    for character in ['t', 's', 'p', 'u', 'a'] {
        assert_eq!(
            app.key(key(KeyCode::Char(character))),
            Action::None,
            "unsupported action {character} must do nothing"
        );
    }
    app.key(key(KeyCode::Char('?')));
    let frame = a_screen(&mut app, SCREEN_WIDTH, SCREEN_HEIGHT);
    assert!(
        frame.contains("not available yet"),
        "help must label unavailable engine actions honestly"
    );
    assert_eq!(
        app.key(key(KeyCode::Char('g'))),
        Action::None,
        "help must not start an agent"
    );
}

#[test]
fn a_refresh_cycle_leaves_database_bytes_and_every_state_directory_entry_unchanged() {
    let bench = Workbench::recorded_home("read-only");
    #[cfg(unix)]
    let _permissions = ReadOnlyLedger::new(&bench.state());
    let before = directory_bytes(&bench.state());
    let mut app = App::new(load(&bench.state()).expect("a read-only ledger must refresh"));
    app.replace(load(&bench.state()).expect("a read-only ledger must refresh"));
    let _ = a_screen(&mut app, SCREEN_WIDTH, SCREEN_HEIGHT);
    let mut output = Vec::new();
    plain(&app.snapshot, &mut output).expect("plain frame");
    assert_eq!(
        directory_bytes(&bench.state()),
        before,
        "read-only refresh and rendering must leave all files and directory entries byte-for-byte unchanged"
    );
}

#[test]
fn a_missing_ledger_and_tiny_terminal_render_without_creating_state() {
    let bench = Workbench::new("empty");
    let mut app = App::new(load(&bench.state()).expect("missing is empty"));
    for (width, height) in [(SCREEN_WIDTH, SCREEN_HEIGHT), (80, 24), (1, 1), (0, 0)] {
        let _ = a_screen(&mut app, width, height);
    }
    let mut output = Vec::new();
    plain(&app.snapshot, &mut output).expect("plain frame");
    assert!(
        String::from_utf8(output)
            .expect("UTF-8")
            .contains("nothing has run yet"),
        "empty plain output explains the state"
    );
    assert!(
        !bench.state().exists(),
        "reading an empty home must not initialize state"
    );
}

#[test]
fn an_error_or_unwind_runs_the_terminal_restore_guard_exactly_once() {
    let restores = Cell::new(0);
    let restore = || restores.set(restores.get() + 1);
    let one_restore = 1;
    let two_restores = one_restore + 1;
    fn a_failed_terminal_read(restore: impl FnOnce()) -> Result<(), &'static str> {
        let _guard = RestoreGuard(Some(restore));
        Err("simulated terminal read error")
    }
    let error = a_failed_terminal_read(restore);
    assert!(
        error.is_err(),
        "the simulated I/O error must return through the guard"
    );
    assert_eq!(
        restores.get(),
        one_restore,
        "an error return drops the guard"
    );
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = RestoreGuard(Some(restore));
        panic!("simulated render panic");
    }));
    assert!(
        panic.is_err(),
        "the simulated panic must unwind through the guard"
    );
    assert_eq!(
        restores.get(),
        two_restores,
        "panic unwinding must also restore exactly once"
    );
}

#[test]
fn new_goal_arguments_are_literal_and_reserved_words_cannot_become_commands() {
    let repository = Path::new("isolated-repository");
    let goal = "review";
    let command = goal_command(Path::new("kaisa"), repository, goal);
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        vec!["--", goal],
        "the option separator makes reserved verbs and option-looking goals literal"
    );
    assert_eq!(
        command.get_current_dir(),
        Some(repository),
        "a submitted goal runs in the current repository"
    );
}

#[test]
fn untrusted_transcripts_cannot_emit_terminal_control_sequences() {
    let injected = "notes\u{1b}]52;c;clipboard\u{7}\r\u{1b}[2J";
    let cleaned = safe(injected);
    assert!(
        !cleaned.contains('\u{1b}') && !cleaned.contains('\u{7}') && !cleaned.contains('\r'),
        "plain and interactive frames must strip terminal control characters"
    );
}

#[test]
fn catalog_translations_preserve_placeholders_and_user_goal_text() {
    use super::messages::{ALL, text_for};
    for &entry in ALL {
        let english = text_for(entry, "en");
        let korean = text_for(entry, "ko");
        assert_eq!(
            english.matches("{}").count(),
            korean.matches("{}").count(),
            "translations must preserve every dynamic field: {english}"
        );
    }
    let goal_with_placeholders = "Fix {} without interpreting the braces";
    let output = super::message(super::Message::SearchPrompt, &[&goal_with_placeholders]);
    assert!(
        output.contains(goal_with_placeholders),
        "catalog interpolation must keep goal text literal"
    );
}

#[test]
fn polling_that_reorders_runs_keeps_the_same_attempt_selected() {
    let (bench, mut app) = a_home("selection");
    let position = app
        .rows()
        .iter()
        .position(|selected| selected.attempt.is_some())
        .expect("an attempt row");
    app.list.select(Some(position));
    let selected = app.selected().expect("selected row");
    let id = app.snapshot.runs[selected.run].attempts[selected.attempt.expect("attempt")]
        .row
        .attempt
        .clone();
    let mut snapshot = std::mem::take(&mut app.snapshot);
    snapshot.runs.reverse();
    // Restore the old order for replace() to capture the selected identity before the new order.
    app.snapshot = load(&bench.state()).expect("original order");
    app.replace(snapshot);
    let selected = app.selected().expect("selection remains");
    assert_eq!(
        app.snapshot.runs[selected.run].attempts[selected.attempt.expect("attempt")]
            .row
            .attempt,
        id,
        "live refresh must not jump to another attempt when Needs you reorders the run list"
    );
}

#[cfg(unix)]
#[test]
fn a_submitted_background_process_survives_dropping_the_ui_child_handle() {
    use std::time::{Duration, Instant};
    let bench = Workbench::new("background");
    let script = bench.base.join("background.sh");
    let marker = bench.base.join("repo/finished");
    std::fs::write(&script, "sleep 0.2\nprintf 'finished' > finished\n").expect("isolated script");
    let (child, log) = super::start_goal(
        Path::new("/bin/sh"),
        &bench.base.join("repo"),
        &bench.state(),
        &script.to_string_lossy(),
    )
    .expect("background command starts");
    // Exactly the UI's quit path: dropping std::process::Child does not cancel the job.
    drop(child);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        marker.exists() && log.exists(),
        "a submitted goal must continue after the TUI quits, with its output redirected to a log"
    );
}

#[test]
fn an_unlinked_fake_attempt_keeps_its_goal_usage_and_unknown_gate_when_its_transcript_is_missing() {
    let bench = Workbench::new("unlinked");
    const INPUT_TOKENS: u64 = 120;
    const OUTPUT_TOKENS: u64 = 40;
    const COST: f64 = 0.0012;
    let usage = format!("{INPUT_TOKENS}/{OUTPUT_TOKENS}/{COST}");
    let output = bench
        .command()
        .args([
            "--agent-program",
            &test_support::binary("kaisa-fake-agent").to_string_lossy(),
            "--agent-arg",
            "--fake-write",
            "--agent-arg",
            "src/notes.md",
            "--agent-arg",
            "--fake-usage",
            "--agent-arg",
            &usage,
            VERIFIED_GOAL,
        ])
        .output()
        .expect("unverified fake-agent run");
    assert!(
        output.status.success(),
        "the standalone fake agent preserves its work: {output:?}"
    );
    let snapshot = load(&bench.state()).expect("unlinked attempt reads");
    let run = snapshot.runs.first().expect("standalone run card");
    let attempt = run.attempts.first().expect("standalone attempt");
    assert!(
        run.goal == VERIFIED_GOAL && run.decision.is_empty(),
        "the context pack supplies the real goal without inventing a verification gate"
    );
    let reported = attempt.usage.as_ref().expect("usage reported");
    assert_eq!(
        (
            reported.input_tokens,
            reported.output_tokens,
            reported.cost_usd
        ),
        (INPUT_TOKENS, OUTPUT_TOKENS, Some(COST)),
        "tokens and cost must come directly from the recorded provider event"
    );
    let transcript = bench
        .state()
        .join("attempts")
        .join(attempt.row.attempt.as_str())
        .join("transcript.log");
    std::fs::remove_file(transcript).expect("simulate unreadable transcript");
    let snapshot = load(&bench.state()).expect("missing transcript is tolerated");
    let detail = details(&snapshot.runs[FIRST_ROW], None);
    assert!(
        detail.contains("transcript not readable yet") && !detail.contains("git merge --ff-only"),
        "missing transcripts cannot hide the attempt or turn an unverified result into a merge offer"
    );
}

#[test]
fn needs_you_is_ordered_by_the_recorded_wait_instead_of_the_start_of_the_goal() {
    let (bench, app) = a_home("wait-order");
    let already_waiting = app.snapshot.runs[FIRST_ROW].id.clone();
    let delivered = app
        .snapshot
        .runs
        .iter()
        .find(|run| run.goal == VERIFIED_GOAL)
        .expect("older delivered goal");
    let mut ledger =
        kaisa_ledger::EventStore::open(&bench.state().join("kaisa.db")).expect("fixture writer");
    const PAYLOAD_VERSION: u16 = 1;
    ledger.append(kaisa_domain::Event { kind: kaisa_domain::EventKind::GateDecided, subject: delivered.id.clone(), payload_version: PAYLOAD_VERSION, payload: serde_json::json!({"run_id": delivered.id, "decision": "NeedsPerson", "reason": "a later recorded permission boundary"}).to_string() }).expect("later wait recorded");
    drop(ledger);
    let snapshot = load(&bench.state()).expect("updated home");
    assert_eq!(
        snapshot.runs[FIRST_ROW].id, already_waiting,
        "the person who has been waiting longest stays first even when an older goal starts waiting later"
    );
}

#[cfg(unix)]
struct ReadOnlyLedger {
    directory: std::path::PathBuf,
    directory_permissions: std::fs::Permissions,
    database_permissions: std::fs::Permissions,
}

#[cfg(unix)]
impl ReadOnlyLedger {
    fn new(directory: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let database = directory.join("kaisa.db");
        let guard = Self {
            directory: directory.to_owned(),
            directory_permissions: std::fs::metadata(directory)
                .expect("state directory")
                .permissions(),
            database_permissions: std::fs::metadata(&database)
                .expect("database")
                .permissions(),
        };
        std::fs::set_permissions(database, std::fs::Permissions::from_mode(0o444))
            .expect("read-only database");
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o555))
            .expect("read-only state directory");
        guard
    }
}

#[cfg(unix)]
impl Drop for ReadOnlyLedger {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.directory, self.directory_permissions.clone());
        let _ = std::fs::set_permissions(
            self.directory.join("kaisa.db"),
            self.database_permissions.clone(),
        );
    }
}
