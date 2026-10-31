//! `kaisa "<goal>"`: run one goal in the current repository (user-flows F04, M0 scope).

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_agents::{ClaudeLaunch, claude_brokered_argv};
use kaisa_daemon::{
    GoalOutcome, GoalRequest, VerifiedRequest, run_goal, run_verified_goal, whole_repository,
};
use kaisa_domain::{
    ContainmentCapability, DefaultReducedPolicy, ReducedModeBasis, ReducedModeDecision, Role,
    WriteSet, codes,
};
use kaisa_sys::{ProcessExit, ProcessSpec};

/// Default model and effort until the router (M1) chooses them.
const DEFAULT_MODEL: &str = "sonnet";
const DEFAULT_EFFORT: &str = "medium";

/// The agent to start: Claude Code in brokered mode, or another program given on the command
/// line (used by tests with a scripted agent).
struct AgentChoice {
    program: Option<PathBuf>,
    args: Vec<String>,
    /// `--agent codex`: Codex develops, observed; Claude Code (or the scripted reviewer) reviews.
    codex: bool,
    /// An `--agent` value Kaisa does not know.
    unknown: Option<String>,
}

/// The Codex model a developer runs on, and its reasoning effort.
const CODEX_MODEL: &str = "gpt-6.1-sol";

/// Codex's arguments for an observed developer run.
fn codex_args() -> Vec<String> {
    kaisa_agents::CodexLaunch::try_new(CODEX_MODEL, kaisa_daemon::ROUTE_EFFORT)
        .map(|launch| kaisa_agents::codex_observed_argv(&launch))
        .unwrap_or_default()
}

/// How to run anyway when containment falls short: shown with every refusal.
const ACCEPT_HINT: &str = "to run anyway, accept what is missing until a deadline: \
--accept-reduced <write,tcp,udp,descendants> --accept-until <unix time in ms>";

/// A person's `--accept-reduced <codes> --accept-until <ms>`, as typed.
#[derive(Default)]
struct AcceptReduced {
    capabilities: Option<String>,
    until: Option<String>,
}

impl AcceptReduced {
    /// The scoped, expiring Decision the person gave, `None` if they gave none, or what is
    /// wrong with it (a usage error).
    fn decision(&self) -> Result<Option<ReducedModeDecision>, String> {
        let (capabilities, until) = match (&self.capabilities, &self.until) {
            (None, None) => return Ok(None),
            (Some(capabilities), Some(until)) => (capabilities, until),
            _ => {
                return Err(
                    "--accept-reduced and --accept-until go together: a Decision names what it \
                     accepts and when it ends"
                        .to_owned(),
                );
            }
        };
        let mut accepts_missing: Vec<ContainmentCapability> = Vec::new();
        for code in capabilities.split(',').map(str::trim) {
            match ContainmentCapability::from_code(code) {
                Some(capability) => accepts_missing.push(capability),
                None => {
                    return Err(format!(
                        "unknown capability {code:?}; use {}",
                        codes(&ContainmentCapability::ALL)
                    ));
                }
            }
        }
        let expires_at_ms: u64 = until
            .parse()
            .map_err(|_| format!("--accept-until {until:?} is not a Unix time in milliseconds"))?;
        Ok(Some(ReducedModeDecision {
            accepts_missing,
            expires_at_ms,
        }))
    }
}

/// Milliseconds since the Unix epoch, the ledger's clock.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Parsed `kaisa [--agent-program <path>] [--agent-arg <arg>]… [--reviewer-arg <arg>]…
/// [--check-program <path> [--check-arg <arg>]…] "<goal>"`.
pub(crate) struct GoalCommand {
    goal: String,
    agent: AgentChoice,
    /// Arguments that make the scripted agent act as the reviewer; with them, or with Claude
    /// Code, the goal runs verified (T31).
    reviewer_args: Vec<String>,
    /// The user's own trusted check, replacing the profile's test commands.
    check: Option<Vec<String>>,
    /// `--confine-claude`: a T16.0 trial that runs Claude Code confined, with its provider and
    /// session files allowed, instead of reduced.
    confine_claude: bool,
    /// `--accept-reduced … --accept-until …`: the person's Decision for a run that cannot be
    /// fully contained (D02).
    accept: AcceptReduced,
}

/// Why the words after `kaisa` are not a goal.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GoalParseError {
    /// Only options, no goal words.
    NoGoal,
    /// An option Kaisa does not have.
    UnknownFlag(String),
}

/// True for a single word shaped like an option (`--n`, `-x`), not for a quoted sentence or a
/// negative number.
fn looks_like_a_flag(word: &str) -> bool {
    let after_dash = word.strip_prefix("--").or_else(|| word.strip_prefix('-'));
    after_dash.is_some_and(|rest| {
        rest.chars().next().is_some_and(char::is_alphabetic) && !word.contains(char::is_whitespace)
    })
}

impl GoalCommand {
    /// Parses the arguments.
    ///
    /// # Errors
    ///
    /// [`GoalParseError::NoGoal`] if there is no goal, and [`GoalParseError::UnknownFlag`] if a
    /// word that looks like a flag is not one Kaisa knows: it must not silently become goal text
    /// that a paid agent then acts on. `--` ends the flags, so a goal can still begin with one.
    pub(crate) fn parse(args: &[String]) -> Result<Self, GoalParseError> {
        let mut agent = AgentChoice {
            program: None,
            args: Vec::new(),
            codex: false,
            unknown: None,
        };
        let mut goal: Vec<&str> = Vec::new();
        let mut reviewer_args: Vec<String> = Vec::new();
        let mut check: Option<Vec<String>> = None;
        let mut confine_claude = false;
        let mut accept = AcceptReduced::default();
        let mut words = args.iter();
        while let Some(word) = words.next() {
            match word.as_str() {
                "--" => {
                    goal.extend(words.by_ref().map(String::as_str));
                    break;
                }
                "--agent" => match words.next().map(String::as_str) {
                    Some("codex") => agent.codex = true,
                    Some("claude") => {}
                    other => agent.unknown = Some(other.unwrap_or_default().to_owned()),
                },
                "--agent-program" => agent.program = words.next().map(PathBuf::from),
                "--agent-arg" => agent.args.extend(words.next().cloned()),
                "--reviewer-arg" => reviewer_args.extend(words.next().cloned()),
                "--confine-claude" => confine_claude = true,
                "--accept-reduced" => {
                    accept.capabilities = Some(words.next().cloned().unwrap_or_default())
                }
                "--accept-until" => accept.until = Some(words.next().cloned().unwrap_or_default()),
                "--check-program" => check = words.next().map(|program| vec![program.clone()]),
                "--check-arg" => {
                    if let (Some(check), Some(arg)) = (check.as_mut(), words.next()) {
                        check.push(arg.clone());
                    }
                }
                other if looks_like_a_flag(other) => {
                    return Err(GoalParseError::UnknownFlag(other.to_owned()));
                }
                other => goal.push(other),
            }
        }
        if goal.is_empty() {
            return Err(GoalParseError::NoGoal);
        }
        Ok(Self {
            goal: goal.join(" "),
            agent,
            reviewer_args,
            check,
            confine_claude,
            accept,
        })
    }

    /// What admits this goal's attempts if their containment falls short: the person's
    /// Decision; otherwise, for Claude Code run reduced as documented, the default policy that
    /// says so (T16.0); otherwise nothing. Observed attempts are admitted on their own default
    /// policy by the daemon.
    fn reduced_mode(&self, decision: Option<ReducedModeDecision>) -> ReducedModeBasis {
        match decision {
            Some(decision) => ReducedModeBasis::Decision(decision),
            None if self.agent.program.is_none() && !self.confine_claude => {
                ReducedModeBasis::DefaultPolicy(DefaultReducedPolicy::ClaudeReducedUntilT16)
            }
            None => ReducedModeBasis::None,
        }
    }

    /// Codex as an observed developer: no bridge, its own workspace sandbox, the plain pack on
    /// standard input.
    fn codex_spec(&self) -> ProcessSpec {
        ProcessSpec {
            program: self
                .agent
                .program
                .clone()
                .unwrap_or_else(|| PathBuf::from("codex")),
            args: codex_args(),
            working_directory: std::env::temp_dir(),
            env: Vec::new(),
        }
    }

    fn launch_spec(&self, mcp_config: &Path) -> ProcessSpec {
        if self.agent.codex {
            return self.codex_spec();
        }
        match &self.agent.program {
            Some(program) => {
                let mut args: Vec<String> = vec![
                    "--mcp-config".to_owned(),
                    mcp_config.to_string_lossy().into_owned(),
                ];
                args.extend(self.agent.args.iter().cloned());
                ProcessSpec {
                    program: program.clone(),
                    args,
                    working_directory: std::env::temp_dir(),
                    env: Vec::new(),
                }
            }
            None => ProcessSpec {
                program: PathBuf::from("claude"),
                args: claude_brokered_argv(&ClaudeLaunch {
                    model: DEFAULT_MODEL,
                    effort: DEFAULT_EFFORT,
                    mcp_config,
                }),
                working_directory: std::env::temp_dir(),
                env: Vec::new(),
            },
        }
    }

    /// True if the goal runs verified: with Claude Code, or a scripted agent given reviewer args.
    fn verified(&self) -> bool {
        self.agent.program.is_none() || !self.reviewer_args.is_empty()
    }

    /// The agent spec for `role`: a scripted program with its role's arguments, or Claude Code
    /// on the model the router picks.
    fn role_spec(&self, role: Role, write_set: &WriteSet, mcp_config: &Path) -> ProcessSpec {
        if self.agent.codex && role == Role::Developer {
            return self.codex_spec();
        }
        match &self.agent.program {
            Some(program) => {
                let mut args: Vec<String> = vec![
                    "--mcp-config".to_owned(),
                    mcp_config.to_string_lossy().into_owned(),
                ];
                let role_args = if role == Role::Reviewer {
                    &self.reviewer_args
                } else {
                    &self.agent.args
                };
                args.extend(role_args.iter().cloned());
                ProcessSpec {
                    program: program.clone(),
                    args,
                    working_directory: std::env::temp_dir(),
                    env: Vec::new(),
                }
            }
            None => crate::verified::claude_spec(
                &crate::verified::routed_model(role, write_set),
                mcp_config,
            ),
        }
    }

    /// Runs the goal verified and reports the gate's decision with its CI exit code.
    fn run_verified(
        &self,
        repository: &Path,
        state: &Path,
        write_set: &WriteSet,
        reduced_mode: &ReducedModeBasis,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> io::Result<u8> {
        let kaisa: PathBuf = std::env::current_exe()?;
        let checks = crate::verified::trusted_checks(repository, self.check.as_deref());
        // The personal policy (T34); a broken policy file stops the run rather than defaulting.
        let policy = match crate::policy::load(state).and_then(|policy| {
            policy
                .snapshot()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        }) {
            Ok(policy) => policy,
            Err(error) => {
                writeln!(err, "kaisa: the policy cannot govern a run: {error}")?;
                return Ok(crate::EXIT_FAILURE);
            }
        };
        let developer = |mcp_config: &Path| self.role_spec(Role::Developer, write_set, mcp_config);
        let reviewer = |mcp_config: &Path| self.role_spec(Role::Reviewer, write_set, mcp_config);
        // F37: light work (a first-pass review) goes to a registered T0 local model, if any.
        let local = crate::agent::load(state)
            .ok()
            .and_then(|personal| crate::verified::first_pass_route(&personal))
            .and_then(|route| {
                kaisa_agents::LocalEndpoint::parse(&route.endpoint)
                    .ok()
                    .map(|endpoint| (route, endpoint.port()))
            });
        let first_pass_spec = |mcp_config: &Path| ProcessSpec {
            program: kaisa.clone(),
            args: local.as_ref().map_or_else(Vec::new, |(route, _)| {
                vec![
                    "local-agent".to_owned(),
                    "--mcp-config".to_owned(),
                    mcp_config.to_string_lossy().into_owned(),
                    "--endpoint".to_owned(),
                    route.endpoint.clone(),
                    "--model".to_owned(),
                    route.model.clone(),
                ]
            }),
            working_directory: std::env::temp_dir(),
            env: Vec::new(),
        };
        if let Some((route, _)) = &local {
            writeln!(out, "first pass: model: local (light work) {}", route.model)?;
        }
        let first_pass = local.as_ref().map(|(_, port)| kaisa_daemon::FirstPass {
            launch: &first_pass_spec,
            model_port: Some(*port),
        });
        // T16.0 trial: Claude Code confined, reaching only its provider and its session files.
        let home: PathBuf = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .unwrap_or_default();
        let trial = self.confine_claude.then(|| {
            crate::verified::claude_confinement(&home, &policy.policy().allowed_egress_hosts)
        });
        let trial_hosts: Vec<&str> = trial
            .as_ref()
            .map(|trial| trial.egress_hosts.iter().map(String::as_str).collect())
            .unwrap_or_default();
        if let Some(trial) = &trial {
            writeln!(
                out,
                "trial (T16.0): Claude Code runs confined; it may reach {} and change {}",
                trial.egress_hosts.join(", "),
                trial
                    .writable
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )?;
        }
        match run_verified_goal(&VerifiedRequest {
            repository,
            state_directory: state,
            goal: &self.goal,
            write_set,
            developer: &developer,
            reviewer: &reviewer,
            kaisa_executable: &kaisa,
            // Claude Code runs reduced until a confined run is verified per OS (T16.0), unless
            // the person asked for the trial.
            confine_agent: self.agent.program.is_some() || self.confine_claude,
            checks: &checks,
            policy: &policy,
            observed: self.agent.codex,
            first_pass,
            egress_hosts: &trial_hosts,
            agent_writable: trial
                .as_ref()
                .map_or(&[][..], |trial| trial.writable.as_slice()),
            reduced_mode,
        }) {
            Ok(outcome) => {
                for attempt in &outcome.attempts {
                    writeln!(out, "{}", attempt.summary())?;
                }
                let (line, status) = crate::delivery::report_verified_outcome(&outcome);
                writeln!(out, "{line}")?;
                Ok(status)
            }
            Err(error) => report_error(&error, out, err),
        }
    }

    /// Runs the goal in the repository containing the current directory.
    pub(crate) fn run(&self, out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
        if let Some(unknown) = &self.agent.unknown {
            writeln!(
                err,
                "kaisa: unknown agent {unknown:?}; use --agent claude or --agent codex"
            )?;
            return Ok(crate::EXIT_USAGE);
        }
        let decision: Option<ReducedModeDecision> = match self.accept.decision() {
            Ok(decision) => decision,
            Err(problem) => {
                writeln!(err, "kaisa: {problem}")?;
                return Ok(crate::EXIT_USAGE);
            }
        };
        if let Some(expired) = decision
            .as_ref()
            .filter(|decision| decision.expires_at_ms <= now_ms())
        {
            writeln!(
                out,
                "waiting for you: your Decision accepting {} ended at {} ms; give a new one\n{ACCEPT_HINT}",
                codes(&expired.accepts_missing),
                expired.expires_at_ms
            )?;
            return Ok(crate::EXIT_WAITING_FOR_HUMAN);
        }
        let reduced_mode: ReducedModeBasis = self.reduced_mode(decision);
        let Some(repository) = repository_root() else {
            writeln!(err, "kaisa: run this inside a git repository")?;
            return Ok(crate::EXIT_USAGE);
        };
        if let Some(line) = parent_repository_notice(&repository) {
            writeln!(out, "{line}")?;
        }
        let state = match kaisa_sys::state_location(|name| std::env::var_os(name)) {
            Ok(location) => location.directory,
            Err(error) => {
                writeln!(err, "kaisa: {error}")?;
                return Ok(crate::EXIT_FAILURE);
            }
        };
        let write_set = whole_repository(&repository)?;
        let kaisa: PathBuf = std::env::current_exe()?;
        let launch = |mcp_config: &Path| self.launch_spec(mcp_config);
        match kaisa_daemon::recover_abandoned(&repository, &state) {
            Ok(recovered) => {
                for attempt in recovered {
                    writeln!(out, "{}", attempt.summary())?;
                }
            }
            Err(error) => writeln!(err, "kaisa: recovery of earlier attempts failed: {error}")?,
        }
        writeln!(out, "▶ {}", self.goal)?;
        if self.verified() {
            return self.run_verified(&repository, &state, &write_set, &reduced_mode, out, err);
        }
        match run_goal(&GoalRequest {
            repository: &repository,
            goal: &self.goal,
            base: "HEAD",
            state_directory: &state,
            write_set: &write_set,
            role: Role::Developer,
            launch: &launch,
            kaisa_executable: &kaisa,
            // Claude Code needs the network; until its confined run is verified on every OS
            // (T16.0) it runs reduced. A scripted agent needs none and is confined.
            confine_agent: self.agent.program.is_some(),
            egress_hosts: &[],
            agent_writable: &[],
            local_model_port: None,
            observed: self.agent.codex,
            reduced_mode: &reduced_mode,
        }) {
            Ok(outcome) => {
                writeln!(out, "{}", outcome.summary())?;
                Ok(exit_status(&outcome))
            }
            Err(error) => report_error(&error, out, err),
        }
    }
}

/// A run that could not be admitted waits for a person (exit 3) and says what is missing;
/// any other failure is a failure.
fn report_error(
    error: &kaisa_daemon::GoalError,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<u8> {
    if error.not_admitted() {
        writeln!(out, "waiting for you: {}\n{ACCEPT_HINT}", error.detail)?;
        return Ok(crate::EXIT_WAITING_FOR_HUMAN);
    }
    writeln!(err, "kaisa: {error}")?;
    Ok(crate::EXIT_FAILURE)
}

/// CI contract (user-flows F26): 0 when the agent finished and its work is preserved, 4 when
/// it failed or work had to be kept back.
fn exit_status(outcome: &GoalOutcome) -> u8 {
    if outcome.exit == Some(ProcessExit::Code(0)) && outcome.kept_back.is_empty() {
        crate::EXIT_SUCCESS
    } else {
        crate::EXIT_RUN_FAILED
    }
}

/// One line naming the repository when Kaisa was started below its root, so a run in a
/// subdirectory never silently works on the whole parent repository.
fn parent_repository_notice(repository: &Path) -> Option<String> {
    let current = std::env::current_dir().ok()?;
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    (!same(&current, repository))
        .then(|| format!("using the git repository at {}", repository.display()))
}

fn repository_root() -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{DefaultReducedPolicy, ReducedModeBasis};

    use super::{GoalParseError, codex_args};

    fn words(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn a_goal_of_several_unquoted_words_and_a_quoted_sentence_both_parse() {
        let unquoted = super::GoalCommand::parse(&words(&["fix", "the", "login", "bug"]));
        let quoted = super::GoalCommand::parse(&words(&["fix the login bug"]));

        assert_eq!(
            unquoted.expect("a goal").goal,
            "fix the login bug",
            "unquoted words are joined into the goal"
        );
        assert_eq!(
            quoted.expect("a goal").goal,
            "fix the login bug",
            "a quoted sentence is the goal as typed"
        );
    }

    #[test]
    fn a_quoted_goal_that_starts_with_dashes_is_text_and_a_double_dash_ends_the_options() {
        let quoted = super::GoalCommand::parse(&words(&["--fix typo in README"]));
        let after_separator = super::GoalCommand::parse(&words(&["--", "--verbose", "output"]));

        assert_eq!(
            quoted.expect("a goal").goal,
            "--fix typo in README",
            "a word with a space is a sentence, not an option"
        );
        assert_eq!(
            after_separator.expect("a goal").goal,
            "--verbose output",
            "everything after `--` is goal text"
        );
    }

    #[test]
    fn an_unknown_option_is_refused_by_name() {
        let parsed = super::GoalCommand::parse(&words(&["--n", "3", "fix"]));

        assert_eq!(
            parsed.err(),
            Some(GoalParseError::UnknownFlag("--n".to_owned())),
            "the refusal names the option the user typed"
        );
    }

    #[test]
    fn the_confine_claude_switch_is_parsed() {
        let words: Vec<String> = ["--confine-claude", "Add", "notes"]
            .iter()
            .map(|word| (*word).to_owned())
            .collect();

        let parsed = super::GoalCommand::parse(&words).expect("a goal is given");

        assert!(
            parsed.confine_claude && parsed.goal == "Add notes",
            "the trial switch is a flag, not part of the goal"
        );
    }

    fn a_goal_with(flags: &[&str]) -> super::GoalCommand {
        let mut words: Vec<String> = flags.iter().map(|word| (*word).to_owned()).collect();
        words.push("Add notes".to_owned());
        super::GoalCommand::parse(&words).expect("a goal is given")
    }

    #[test]
    fn claude_by_default_is_admitted_on_the_documented_default_policy() {
        let claude_by_default = a_goal_with(&[]);

        assert_eq!(
            claude_by_default.reduced_mode(None),
            ReducedModeBasis::DefaultPolicy(DefaultReducedPolicy::ClaudeReducedUntilT16),
            "Claude Code stays reduced until T16.0, and the ledger names the policy that says so"
        );
    }

    #[test]
    fn a_confined_claude_trial_has_no_default_policy_to_fall_back_on() {
        let trial = a_goal_with(&["--confine-claude"]);

        assert_eq!(
            trial.reduced_mode(None),
            ReducedModeBasis::None,
            "a run asked to be confined must not be admitted weaker without a Decision"
        );
    }

    #[test]
    fn a_scripted_agent_has_no_default_policy_to_fall_back_on() {
        let scripted = a_goal_with(&["--agent-program", "agent"]);

        assert_eq!(
            scripted.reduced_mode(None),
            ReducedModeBasis::None,
            "only the documented Claude Code default may admit a reduced run without a Decision"
        );
    }

    #[test]
    fn a_decision_needs_both_what_it_accepts_and_when_it_ends() {
        let without_deadline = a_goal_with(&["--accept-reduced", "udp"]);

        assert!(
            without_deadline.accept.decision().is_err(),
            "a Decision without an expiry would be a permanent weakening"
        );
    }

    #[test]
    fn the_codex_developer_launch_is_valid() {
        assert!(
            !codex_args().is_empty(),
            "the built-in Codex model and effort pass launch validation, so Codex gets its argv"
        );
    }
}
