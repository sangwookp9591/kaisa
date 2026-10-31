//! The Codex CLI adapter in observed mode.

use std::fmt;

use serde_json::Value;

use crate::transcript::{event_type, events};

/// How Kaisa supervises an agent's tool effects (architecture §5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustMode {
    /// Every tool effect passes through Kaisa's execution broker.
    Brokered,
    /// The agent uses its own tools; Kaisa records its activity under OS containment.
    Observed,
}

/// The Codex trust mode established by `docs/research/brokered-mode-spike.md`.
///
/// Its built-in tools cannot be turned off with the tested configuration, so Kaisa records
/// what Codex does instead of mediating it through the execution broker.
#[must_use]
pub fn codex_trust_mode() -> TrustMode {
    TrustMode::Observed
}

/// Why a Codex launch value cannot be passed safely to the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexLaunchError {
    /// The model is empty, starts with `-`, or contains whitespace or quotes.
    InvalidModel,
    /// The effort is empty, starts with `-`, or contains whitespace or quotes.
    InvalidEffort,
}

impl fmt::Display for CodexLaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidModel => f.write_str("the Codex model must be a nonempty value without a leading hyphen, whitespace or quotes"),
            Self::InvalidEffort => f.write_str("the Codex effort must be a nonempty value without a leading hyphen, whitespace or quotes"),
        }
    }
}

impl std::error::Error for CodexLaunchError {}

/// Validated router choices for one Codex attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexLaunch<'launch> {
    model: &'launch str,
    effort: &'launch str,
}

impl<'launch> CodexLaunch<'launch> {
    /// Validates the provider model id and reasoning effort before constructing a launch.
    ///
    /// # Errors
    ///
    /// Returns [`CodexLaunchError`] if either value is empty, starts with `-`, or contains
    /// whitespace or a single or double quote.
    pub fn try_new(model: &'launch str, effort: &'launch str) -> Result<Self, CodexLaunchError> {
        if !is_safe_launch_value(model) {
            return Err(CodexLaunchError::InvalidModel);
        }
        // The effort lands inside a TOML string, so it is one of Codex's own levels, never text.
        if !CODEX_EFFORTS.contains(&effort) {
            return Err(CodexLaunchError::InvalidEffort);
        }
        Ok(Self { model, effort })
    }
}

/// The arguments that run Codex with JSONL events, its workspace sandbox and a stdin prompt.
#[must_use]
pub fn codex_observed_argv(launch: &CodexLaunch<'_>) -> Vec<String> {
    vec![
        "exec".to_owned(),
        "--json".to_owned(),
        "--skip-git-repo-check".to_owned(),
        "-s".to_owned(),
        "workspace-write".to_owned(),
        "-m".to_owned(),
        launch.model.to_owned(),
        "-c".to_owned(),
        format!("model_reasoning_effort=\"{}\"", launch.effort),
        "-".to_owned(),
    ]
}

/// The reasoning levels Codex CLI 0.165 accepts for `model_reasoning_effort`.
pub const CODEX_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// What Codex finally answered in an `exec --json` `transcript`: the text of its last
/// `agent_message` item, if a `turn.completed` follows it. A failed turn or a stream error
/// after the message leaves no answer.
#[must_use]
pub fn codex_final_answer(transcript: &str) -> Option<String> {
    let mut answer: Option<String> = None;
    let mut completed: bool = false;
    let mut failed: bool = false;
    for event in events(transcript) {
        match event_type(&event) {
            Some("item.completed")
                if event.pointer("/item/type").and_then(Value::as_str) == Some("agent_message") =>
            {
                answer = event
                    .pointer("/item/text")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                completed = false;
            }
            Some("turn.completed") => completed = !failed,
            Some("thread.started" | "turn.started") => {
                answer = None;
                completed = false;
                failed = false;
            }
            Some("turn.failed" | "error") => {
                answer = None;
                completed = false;
                failed = true;
            }
            _ => {}
        }
    }
    answer.filter(|_| completed)
}

fn is_safe_launch_value(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, '\'' | '"'))
}

#[cfg(test)]
mod tests {
    use super::{CodexLaunch, CodexLaunchError, TrustMode, codex_observed_argv, codex_trust_mode};

    const CODEX_MODEL: &str = "gpt-6.1-sol";
    const REASONING_EFFORT: &str = "medium";
    const OPTION_AND_VALUE_LENGTH: usize = 2;

    fn a_codex_launch() -> CodexLaunch<'static> {
        CodexLaunch::try_new(CODEX_MODEL, REASONING_EFFORT)
            .expect("the router chose a plain model id and reasoning effort")
    }

    fn unsafe_launch_values() -> [&'static str; 9] {
        [
            "",
            "--dangerously-bypass-approvals-and-sandbox",
            "model --sandbox danger-full-access",
            "model\tflag",
            "model\nflag",
            "model\rflag",
            "model\u{00a0}flag",
            "model\"flag",
            "model'flag",
        ]
    }

    #[test]
    fn a_codex_attempt_is_labelled_observed() {
        assert_eq!(
            codex_trust_mode(),
            TrustMode::Observed,
            "the spike did not establish that Codex can disable its built-in tools"
        );
    }

    #[test]
    fn the_codex_launch_never_disables_its_sandbox() {
        let argv: Vec<String> = codex_observed_argv(&a_codex_launch());
        let sandbox: Option<&[String]> = argv
            .windows(OPTION_AND_VALUE_LENGTH)
            .find(|pair| pair.first().map(String::as_str) == Some("-s"));

        assert_eq!(
            sandbox.and_then(|pair| pair.last()).map(String::as_str),
            Some("workspace-write"),
            "observed Codex attempts retain the CLI's workspace sandbox"
        );
        assert!(
            argv.iter().all(|argument| !matches!(
                argument.as_str(),
                "danger-full-access"
                    | "--dangerously-bypass-approvals-and-sandbox"
                    | "--yolo"
                    | "--full-auto"
            )),
            "the launch must not request unrestricted access or bypass approval policy"
        );
    }

    #[test]
    fn a_model_name_cannot_smuggle_a_flag() {
        for unsafe_model in unsafe_launch_values() {
            assert_eq!(
                CodexLaunch::try_new(unsafe_model, REASONING_EFFORT),
                Err(CodexLaunchError::InvalidModel),
                "unsafe model {unsafe_model:?} must fail before an argv can be built"
            );
        }
    }

    #[test]
    fn a_reasoning_effort_cannot_smuggle_a_flag() {
        for unsafe_effort in unsafe_launch_values() {
            assert_eq!(
                CodexLaunch::try_new(CODEX_MODEL, unsafe_effort),
                Err(CodexLaunchError::InvalidEffort),
                "unsafe effort {unsafe_effort:?} must not escape its config value"
            );
        }
    }

    #[test]
    fn the_prompt_is_read_from_standard_input() {
        let argv: Vec<String> = codex_observed_argv(&a_codex_launch());

        assert_eq!(
            argv.last().map(String::as_str),
            Some("-"),
            "Codex reads the recorded context pack from stdin when PROMPT is a hyphen"
        );
    }

    #[test]
    fn a_valid_launch_sets_the_model_and_effort_without_extra_flags() {
        let argv: Vec<String> = codex_observed_argv(&a_codex_launch());

        assert_eq!(
            argv,
            [
                "exec",
                "--json",
                "--skip-git-repo-check",
                "-s",
                "workspace-write",
                "-m",
                CODEX_MODEL,
                "-c",
                "model_reasoning_effort=\"medium\"",
                "-",
            ],
            "the observed adapter emits exactly the audited sandboxed command"
        );
    }

    #[test]
    fn an_effort_outside_the_known_levels_is_refused() {
        let trailing_escape = r"medium\";

        let launch = super::CodexLaunch::try_new("gpt-6.1-sol", trailing_escape);

        assert_eq!(
            launch.err(),
            Some(super::CodexLaunchError::InvalidEffort),
            "an effort is one of Codex's levels, so it can never break the TOML string it sits in"
        );
    }
}
