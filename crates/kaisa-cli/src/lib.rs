//! Kaisa command-line front end.
//!
//! Design: `docs/user-flows.md` §7. A goal, `watch`, `tui`, `agent` and `policy` work; every other
//! command reports that it is not implemented yet instead of pretending to work.

use std::io;
use std::io::Write;

mod agent;
mod claude_code;
mod delivery;
mod doctor;
mod goal;
mod home;
mod init;
mod local_agent;
mod mcp_serve;
mod policy;
mod spec;
mod routing;
mod tui;
mod verified;
mod watch;

pub use claude_code::{
    PROJECT_MCP_PATH, PlannedAction, PlannedFile, SLASH_COMMAND_PATH, USER_SCOPE_REGISTRATION,
    claude_code_integration, claude_code_project_server,
};
pub use delivery::{EXIT_WAITING_FOR_HUMAN, report_delivery, report_verified_outcome};

/// Exit status for a command that completed successfully.
pub const EXIT_SUCCESS: u8 = 0;

/// Exit status for a command that has not been implemented yet.
pub const EXIT_NOT_IMPLEMENTED: u8 = 2;

/// Exit status for a command that ran and failed.
pub const EXIT_FAILURE: u8 = 1;

/// Exit status for a command used with the wrong arguments.
pub const EXIT_USAGE: u8 = 64;

/// Exit status of a goal whose agent failed or whose work was kept back (CI contract, F26).
pub const EXIT_RUN_FAILED: u8 = 4;

/// Command names reserved for later milestones; they answer "not implemented" instead of being
/// mistaken for a goal.
///
/// Every verb that README, docs/user-flows.md or docs/architecture.md names as a command and that
/// does not work yet is listed, so none of them can start a paid run as goal text. The docs name
/// them as literal `kaisa <verb>`, in the command tables of user-flows §7 and architecture §4
/// (`handoff`, `handback`, `wrap`, `console`, `memory` appear only there), and in process-model
/// rows.
const PLANNED_COMMANDS: [&str; 25] = [
    "review",
    "ask",
    "attach",
    "steer",
    "pause",
    "resume",
    "cancel",
    "undo",
    "restore",
    "status",
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

/// The version of this build, taken from the workspace manifest (single source of truth).
const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
Kaisa — run coding agents safely

Usage:
  kaisa \"<goal>\"           a developer agent works on the goal, your checks and a reviewer judge
                           the result, and only a verified candidate is delivered
  kaisa watch              attempts, their state, tokens and cost, from the ledger
  kaisa tui [--plain]      terminal home: needs you, goals, attempts and evidence
  kaisa review             approve results       (planned)
  kaisa mcp serve          let Claude Code start and follow goals (claude mcp add kaisa -- kaisa mcp serve)

More commands:
  kaisa                    Home: setup and recent work
  kaisa doctor             read-only setup diagnosis
  kaisa init [--yes]        preview project checks; confirm to save kaisa.toml
  kaisa agent add|list     register a personal agent or a local model server
  kaisa spec <command>     draft, show, list or approve versioned requirements
  kaisa policy show|set    view or change your personal policy
  kaisa route explain <task|last>  recorded route and candidate constraints
  kaisa stats routing      measured routing cells; unknown costs stay labelled

Goal options:
  --agent codex            Codex develops (observed); Claude Code reviews
  --model <id>             pin a model; automatic routing leaves pins unchanged
  --check-program <path> [--check-arg <arg>]...
                           your own trusted check instead of the project's tests
  --confine-claude         try a confined Claude Code run (T16.0 trial)

Exit codes: 0 success/verified, 1 setup failure, 3 waiting for you, 4 not delivered, 64 usage error.

Options:
  -h, --help               show this help
  -V, --version            show the version
";

/// Runs the CLI with `args` (excluding the program name) and returns the exit status.
///
/// Output goes to `out` and diagnostics to `err`, so callers and tests choose the sinks.
///
/// # Errors
///
/// Returns an error only if writing to `out` or `err` fails.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    match args.first().map(String::as_str) {
        None => home::run(out, err),
        Some("doctor") => doctor::run(&args[1..], out, err),
        Some("init") => init::run(&args[1..], out, err),
        Some("-h" | "--help" | "help") => {
            out.write_all(HELP.as_bytes())?;
            Ok(EXIT_SUCCESS)
        }
        Some("-V" | "--version" | "version") => {
            writeln!(out, "kaisa {VERSION}")?;
            Ok(EXIT_SUCCESS)
        }
        Some("watch") => watch::run(out, err),
        Some("tui") => tui::run(&args[1..], out, err),
        Some("local-agent") => local_agent::run(&args[1..], out, err),
        Some("agent") => agent::run(&args[1..], out, err),
        Some("spec") => spec::run(&args[1..], out, err),
        Some("policy") => policy::run(&args[1..], out, err),
        Some("route") => routing::run(&args[1..], false, out, err),
        Some("stats") => routing::run(&args[1..], true, out, err),
        Some("mcp") => mcp_serve::run(&args[1..], out, err),
        Some(command) if PLANNED_COMMANDS.contains(&command) => {
            writeln!(
                err,
                "kaisa: `{command}` is not implemented yet. See docs/user-flows.md for the plan."
            )?;
            if args.len() > 1 {
                writeln!(
                    err,
                    "kaisa: if you meant a goal that starts with `{command}`, quote the whole \
                     goal: kaisa \"{}\"",
                    args.join(" ")
                )?;
            }
            Ok(EXIT_NOT_IMPLEMENTED)
        }
        Some(_) => match goal::GoalCommand::parse(args) {
            Ok(command) => command.run(out, err),
            Err(goal::GoalParseError::UnknownFlag(flag)) => {
                writeln!(
                    err,
                    "kaisa: unknown option `{flag}`. Options are listed in `kaisa --help`; to \
                     use it in a goal, put `--` before the goal."
                )?;
                Ok(EXIT_USAGE)
            }
            Err(goal::GoalParseError::NoGoal) => {
                writeln!(err, "usage: kaisa \"<goal>\"")?;
                Ok(EXIT_USAGE)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{EXIT_NOT_IMPLEMENTED, EXIT_SUCCESS, EXIT_USAGE, VERSION, run};

    /// What a user sees after running one `kaisa` command line.
    struct Transcript {
        status: u8,
        stdout: String,
        stderr: String,
    }

    /// Runs the CLI as if the user typed `kaisa <words…>`.
    fn user_types(words: &[&str]) -> Transcript {
        let args: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
        let mut stdout: Vec<u8> = Vec::new();
        let mut stderr: Vec<u8> = Vec::new();
        let status: u8 =
            run(&args, &mut stdout, &mut stderr).expect("writing to memory cannot fail");
        Transcript {
            status,
            stdout: String::from_utf8(stdout).expect("CLI output is UTF-8"),
            stderr: String::from_utf8(stderr).expect("CLI diagnostics are UTF-8"),
        }
    }

    #[test]
    fn asking_for_the_version_prints_the_workspace_version_and_succeeds() {
        let transcript: Transcript = user_types(&["--version"]);

        assert_eq!(
            transcript.status, EXIT_SUCCESS,
            "showing the version is not an error"
        );
        assert_eq!(
            transcript.stdout,
            format!("kaisa {VERSION}\n"),
            "the version comes from the workspace manifest, the single source of truth"
        );
    }

    #[test]
    fn explicit_help_shows_the_three_daily_verbs() {
        let transcript: Transcript = user_types(&["--help"]);

        assert_eq!(
            transcript.status, EXIT_SUCCESS,
            "explicit help must not fail"
        );
        for verb in ["kaisa \"<goal>\"", "kaisa watch", "kaisa review"] {
            assert!(
                transcript.stdout.contains(verb),
                "help must teach the three-verb surface (user-flows U1), missing `{verb}`"
            );
        }
    }

    #[test]
    fn the_help_lists_the_commands_that_work_without_calling_them_planned() {
        let transcript: Transcript = user_types(&["--help"]);

        for working in ["kaisa watch", "kaisa tui", "kaisa agent", "kaisa policy"] {
            let line: &str = transcript
                .stdout
                .lines()
                .find(|line| line.contains(working))
                .unwrap_or_default();
            assert!(
                !line.is_empty() && !line.contains("(planned)"),
                "`{working}` works, so help lists it and does not call it planned: {line:?}"
            );
        }
    }

    #[test]
    fn an_unimplemented_command_fails_honestly_instead_of_pretending_to_work() {
        let transcript: Transcript = user_types(&["review"]);

        assert_eq!(
            transcript.status, EXIT_NOT_IMPLEMENTED,
            "scripts must be able to tell that nothing happened"
        );
        assert!(
            transcript.stdout.is_empty(),
            "no fake output may appear on stdout"
        );
        assert!(
            transcript.stderr.contains("not implemented yet"),
            "the user must be told why nothing happened"
        );
    }

    /// Every verb the docs name as a command: the planned ones answer "not implemented", the
    /// others work today. Collected from literal `kaisa <verb>` text and from the command tables
    /// in docs/user-flows.md §7 and docs/architecture.md §4.
    const DOCUMENTED_UNIMPLEMENTED_VERBS: [&str; 25] = [
        "review",
        "ask",
        "attach",
        "steer",
        "pause",
        "resume",
        "cancel",
        "undo",
        "restore",
        "status",
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

    #[test]
    fn the_refusal_table_covers_exactly_the_reserved_verbs() {
        let mut reserved: Vec<&str> = super::PLANNED_COMMANDS.to_vec();
        let mut documented: Vec<&str> = DOCUMENTED_UNIMPLEMENTED_VERBS.to_vec();
        reserved.sort_unstable();
        documented.sort_unstable();

        assert_eq!(
            reserved, documented,
            "a verb added to one list without the other would escape the refusal test"
        );
    }

    #[test]
    fn every_documented_but_unimplemented_verb_answers_not_implemented_instead_of_starting_a_run() {
        for verb in DOCUMENTED_UNIMPLEMENTED_VERBS {
            let transcript: Transcript = user_types(&[verb, "--dry-run"]);

            assert_eq!(
                transcript.status, EXIT_NOT_IMPLEMENTED,
                "`kaisa {verb}` must not become goal text for a paid agent"
            );
            assert!(
                transcript.stderr.contains("not implemented yet"),
                "`kaisa {verb}` tells the user nothing ran: {:?}",
                transcript.stderr
            );
        }
    }

    #[test]
    fn a_goal_starting_with_a_reserved_verb_is_told_to_quote_the_goal() {
        let transcript: Transcript = user_types(&["review", "the", "parser"]);

        assert_eq!(
            transcript.status, EXIT_NOT_IMPLEMENTED,
            "a reserved first word is refused, never run as a goal"
        );
        assert!(
            transcript.stderr.contains("kaisa \"review the parser\""),
            "the hint shows the quoted form: {:?}",
            transcript.stderr
        );
    }

    #[test]
    fn an_unknown_option_is_a_usage_error_naming_it_and_not_goal_text() {
        for flag in [
            "--agents",
            "--n",
            "--budget",
            "--template",
            "--headless",
            "--json",
        ] {
            let transcript: Transcript = user_types(&[flag, "fix", "the", "login", "bug"]);

            assert_eq!(
                transcript.status, EXIT_USAGE,
                "`{flag}` is not an option Kaisa has"
            );
            assert!(
                transcript.stderr.contains(flag),
                "the error names `{flag}`: {:?}",
                transcript.stderr
            );
        }
    }

    #[test]
    fn an_unknown_option_among_the_goal_words_is_a_usage_error_too() {
        let transcript: Transcript = user_types(&["fix", "the", "--verbose", "bug"]);

        assert_eq!(
            transcript.status, EXIT_USAGE,
            "an option-like word inside a goal is a usage error, not goal text"
        );
        assert!(
            transcript.stderr.contains("--verbose"),
            "the error names the option: {:?}",
            transcript.stderr
        );
    }
}
