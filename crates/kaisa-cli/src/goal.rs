//! `kaisa "<goal>"`: run one goal in the current repository (user-flows F04, M0 scope).

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_agents::{ClaudeLaunch, claude_brokered_argv};
use kaisa_daemon::{GoalOutcome, GoalRequest, run_goal, whole_repository};
use kaisa_sys::{ProcessExit, ProcessSpec};

/// Default model and effort until the router (M1) chooses them.
const DEFAULT_MODEL: &str = "sonnet";
const DEFAULT_EFFORT: &str = "medium";

/// The agent to start: Claude Code in brokered mode, or another program given on the command
/// line (used by tests with a scripted agent).
struct AgentChoice {
    program: Option<PathBuf>,
    args: Vec<String>,
}

/// Parsed `kaisa [--agent-program <path>] [--agent-arg <arg>]… "<goal>"`.
pub(crate) struct GoalCommand {
    goal: String,
    agent: AgentChoice,
}

impl GoalCommand {
    /// Parses the arguments; `None` if there is no goal.
    pub(crate) fn parse(args: &[String]) -> Option<Self> {
        let mut agent = AgentChoice {
            program: None,
            args: Vec::new(),
        };
        let mut goal: Vec<&str> = Vec::new();
        let mut words = args.iter();
        while let Some(word) = words.next() {
            match word.as_str() {
                "--agent-program" => agent.program = words.next().map(PathBuf::from),
                "--agent-arg" => agent.args.extend(words.next().cloned()),
                other => goal.push(other),
            }
        }
        (!goal.is_empty()).then(|| Self {
            goal: goal.join(" "),
            agent,
        })
    }

    fn launch_spec(&self, mcp_config: &Path) -> ProcessSpec {
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

    /// Runs the goal in the repository containing the current directory.
    pub(crate) fn run(&self, out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
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
        writeln!(out, "▶ {}", self.goal)?;
        match run_goal(&GoalRequest {
            repository: &repository,
            goal: &self.goal,
            state_directory: &state,
            write_set: &write_set,
            launch: &launch,
            kaisa_executable: &kaisa,
            // Claude Code needs the network; until its egress goes through the broker it runs
            // reduced. A scripted agent needs none and is confined.
            confine_agent: self.agent.program.is_some(),
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
