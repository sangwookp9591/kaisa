//! `kaisa "<goal>"`: run one goal in the current repository (user-flows F04, M0 scope).

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_agents::{ClaudeLaunch, claude_brokered_argv};
use kaisa_daemon::{
    GoalOutcome, GoalRequest, VerifiedRequest, run_goal, run_verified_goal, whole_repository,
};
use kaisa_domain::{Role, WriteSet};
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
}

impl GoalCommand {
    /// Parses the arguments; `None` if there is no goal.
    pub(crate) fn parse(args: &[String]) -> Option<Self> {
        let mut agent = AgentChoice {
            program: None,
            args: Vec::new(),
            codex: false,
            unknown: None,
        };
        let mut goal: Vec<&str> = Vec::new();
        let mut reviewer_args: Vec<String> = Vec::new();
        let mut check: Option<Vec<String>> = None;
        let mut words = args.iter();
        while let Some(word) = words.next() {
            match word.as_str() {
                "--agent" => match words.next().map(String::as_str) {
                    Some("codex") => agent.codex = true,
                    Some("claude") => {}
                    other => agent.unknown = Some(other.unwrap_or_default().to_owned()),
                },
                "--agent-program" => agent.program = words.next().map(PathBuf::from),
                "--agent-arg" => agent.args.extend(words.next().cloned()),
                "--reviewer-arg" => reviewer_args.extend(words.next().cloned()),
                "--check-program" => check = words.next().map(|program| vec![program.clone()]),
                "--check-arg" => {
                    if let (Some(check), Some(arg)) = (check.as_mut(), words.next()) {
                        check.push(arg.clone());
                    }
                }
                other => goal.push(other),
            }
        }
        (!goal.is_empty()).then(|| Self {
            goal: goal.join(" "),
            agent,
            reviewer_args,
            check,
        })
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
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> io::Result<u8> {
        let kaisa: PathBuf = std::env::current_exe()?;
        let checks = crate::verified::trusted_checks(repository, self.check.as_deref());
        let policy = match crate::verified::default_policy().snapshot() {
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
        match run_verified_goal(&VerifiedRequest {
            repository,
            state_directory: state,
            goal: &self.goal,
            write_set,
            developer: &developer,
            reviewer: &reviewer,
            kaisa_executable: &kaisa,
            // Claude Code runs reduced until a confined run is verified per OS (T16.0).
            confine_agent: self.agent.program.is_some(),
            checks: &checks,
            policy: &policy,
            observed: self.agent.codex,
            first_pass,
        }) {
            Ok(outcome) => {
                for attempt in &outcome.attempts {
                    writeln!(out, "{}", attempt.summary())?;
                }
                let (line, status) = crate::report_delivery(&outcome.delivery);
                writeln!(out, "{line}")?;
                Ok(status)
            }
            Err(error) => {
                writeln!(err, "kaisa: {error}")?;
                Ok(crate::EXIT_FAILURE)
            }
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
        let Some(repository) = repository_root() else {
            writeln!(err, "kaisa: run this inside a git repository")?;
            return Ok(crate::EXIT_USAGE);
        };
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
            return self.run_verified(&repository, &state, &write_set, out, err);
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
        }) {
            Ok(outcome) => {
                writeln!(out, "{}", outcome.summary())?;
                Ok(exit_status(&outcome))
            }
            Err(error) => {
                writeln!(err, "kaisa: {error}")?;
                Ok(crate::EXIT_FAILURE)
            }
        }
    }
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
    use super::codex_args;

    #[test]
    fn the_codex_developer_launch_is_valid() {
        assert!(
            !codex_args().is_empty(),
            "the built-in Codex model and effort pass launch validation, so Codex gets its argv"
        );
    }
}
