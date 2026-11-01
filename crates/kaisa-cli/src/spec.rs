//! `kaisa spec`: draft, inspect and approve immutable specification versions.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_domain::spec::approval::{ApprovalTerminal, confirm};
use kaisa_domain::{ReducedModeBasis, Role, SpecApproval, SpecId, WriteSet};
use kaisa_ledger::spec::{SpecStore, StoredSpec};

const USAGE: &str = "usage: kaisa spec draft \"<requirements>\"|--file <path> [--id <spec_id>] [--agent-program <path> --agent-arg <arg> ...]\n       kaisa spec show <id> [--version <n>]\n       kaisa spec list\n       kaisa spec approve <id> [--provisional]";

struct Terminal;
fn command_text(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "{program} could not verify approval identity or ancestry"
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}
#[cfg(unix)]
fn attempt_ancestor() -> Result<bool, String> {
    let processes = command_text("/bin/ps", &["-axo", "pid=,ppid=,comm="])?;
    let mut rows = std::collections::BTreeMap::new();
    for line in processes.lines() {
        let mut fields = line.split_whitespace();
        let Some(pid) = fields.next().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let parent = fields
            .next()
            .and_then(|n| n.parse::<u32>().ok())
            .ok_or("invalid process ancestry")?;
        let program = line
            .trim_start()
            .split_once(char::is_whitespace)
            .and_then(|(_, rest)| rest.trim_start().split_once(char::is_whitespace))
            .map(|(_, program)| program.trim())
            .unwrap_or_default();
        rows.insert(pid, (parent, program));
    }
    kaisa_ancestor(&rows, std::process::id())
}
#[cfg(windows)]
fn attempt_ancestor() -> Result<bool, String> {
    let text = command_text(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name | ConvertTo-Json -Compress",
        ],
    )?;
    let processes: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let rows = processes
        .as_array()
        .ok_or("cannot verify process ancestry")?;
    let mut snapshot = std::collections::BTreeMap::new();
    for row in rows {
        let pid = row["ProcessId"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("invalid process id")?;
        let parent = row["ParentProcessId"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("invalid parent process id")?;
        let name = row["Name"].as_str().ok_or("invalid process name")?;
        snapshot.insert(pid, (parent, name));
    }
    kaisa_ancestor(&snapshot, std::process::id())
}
#[cfg(not(any(unix, windows)))]
fn attempt_ancestor() -> Result<bool, String> {
    Err("process ancestry verification is unsupported on this platform".into())
}
/// Every ancestor must be accounted for; absence is not evidence of a human shell.
fn kaisa_ancestor(
    rows: &std::collections::BTreeMap<u32, (u32, &str)>,
    pid: u32,
) -> Result<bool, String> {
    let mut current = pid;
    let mut visited = std::collections::BTreeSet::new();
    while current != 0 {
        if !visited.insert(current) {
            return Err("cycle in process ancestry".into());
        }
        let (parent, program) = rows.get(&current).ok_or("cannot verify process ancestry")?;
        let name = Path::new(program)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if current != pid && (name == "kaisa" || name.eq_ignore_ascii_case("kaisa.exe")) {
            return Ok(true);
        }
        current = *parent;
    }
    Ok(false)
}
impl ApprovalTerminal for Terminal {
    fn both_terminals(&self) -> bool {
        io::stdin().is_terminal() && io::stdout().is_terminal()
    }
    fn inside_attempt(&self) -> Result<bool, String> {
        if std::env::var_os("KAISA_ATTEMPT").is_some() {
            return Ok(true);
        }
        attempt_ancestor()
    }
    fn os_user(&self) -> Result<String, String> {
        #[cfg(unix)]
        let name = command_text("/usr/bin/id", &["-un"])?;
        #[cfg(windows)]
        let name = command_text("whoami.exe", &[])?;
        #[cfg(not(any(unix, windows)))]
        let name = return Err("OS user lookup unsupported".into());
        Ok(name.trim().into())
    }
    fn read_confirmation(&mut self, digest: &str) -> Result<String, String> {
        print!("Specification digest: {digest}\nType its first 8 hex characters to approve: ");
        io::stdout().flush().map_err(|e| e.to_string())?;
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .map_err(|e| e.to_string())?;
        Ok(answer)
    }
}
fn print_spec(spec: &StoredSpec, out: &mut dyn Write) -> io::Result<()> {
    let approval = spec
        .approval
        .as_ref()
        .map_or("provisional draft (NOT human-approved)", |a| {
            if a.kind == "human-approved" {
                "human-approved"
            } else {
                "provisional (NOT human-approved)"
            }
        });
    writeln!(
        out,
        "{} v{} digest={} {} frozen={}",
        spec.specification.id, spec.specification.version, spec.digest, approval, spec.frozen
    )?;
    writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(&spec.specification).map_err(io::Error::other)?
    )?;
    for ambiguity in &spec.specification.ambiguities {
        writeln!(out, "ambiguity {}: {}", ambiguity.id, ambiguity.question)?;
    }
    for finding in spec.specification.validate() {
        match finding {
            kaisa_domain::spec::SpecFinding::MissingAcceptanceCriteria { requirement_ids } => {
                writeln!(
                    out,
                    "missing acceptance criteria: {}",
                    requirement_ids.join(", ")
                )?;
            }
            other => writeln!(out, "validation finding: {other:?}")?,
        }
    }
    Ok(())
}
fn draft(
    args: &[String],
    state: &Path,
    store: &SpecStore,
    out: &mut dyn Write,
) -> Result<(), String> {
    let mut requirements = None;
    let mut id = None;
    let mut program = None;
    let mut agent_args = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--file" | "--id" | "--agent-program" | "--agent-arg" => {
                index += 1;
                let value = args.get(index).ok_or(USAGE)?;
                match arg.as_str() {
                    "--file" if requirements.is_none() => {
                        requirements =
                            Some(std::fs::read_to_string(value).map_err(|e| e.to_string())?)
                    }
                    "--id" if id.is_none() => id = Some(SpecId::try_new(value)?),
                    "--agent-program" => program = Some(PathBuf::from(value)),
                    "--agent-arg" => agent_args.push(value.clone()),
                    _ => return Err(USAGE.into()),
                }
            }
            value if !value.starts_with('-') && requirements.is_none() => {
                requirements = Some(value.into())
            }
            _ => return Err(USAGE.into()),
        }
        index += 1;
    }
    let requirements: String = requirements.ok_or(USAGE)?;
    if requirements.trim().is_empty() {
        return Err("requirements are empty".into());
    }
    let id = id.unwrap_or(
        SpecId::try_new(&format!(
            "spec_{:x}{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos()
        ))
        .map_err(|e| e.to_string())?,
    );
    let policy = crate::policy::load(state).map_err(|e| e.to_string())?;
    let repository = std::env::current_dir().map_err(|e| e.to_string())?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let fake = program.is_some();
    let launch = |config: &Path| match &program {
        Some(program) => {
            let mut args = vec!["--mcp-config".into(), config.to_string_lossy().into_owned()];
            args.extend(agent_args.clone());
            kaisa_sys::ProcessSpec {
                program: program.clone(),
                args,
                working_directory: repository.clone(),
                env: Vec::new(),
            }
        }
        None => crate::verified::claude_spec("sonnet", config),
    };
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("home directory unavailable")?;
    let confinement = crate::verified::claude_confinement(&home, &policy.allowed_egress_hosts);
    let egress: Vec<&str> = if fake {
        Vec::new()
    } else {
        confinement
            .egress_hosts
            .iter()
            .map(String::as_str)
            .collect()
    };
    let writable = if fake {
        &[][..]
    } else {
        confinement.writable.as_slice()
    };
    let basis = if policy.confine_agents {
        ReducedModeBasis::None
    } else {
        ReducedModeBasis::DefaultPolicy(kaisa_domain::DefaultReducedPolicy::ClaudeReducedUntilT16)
    };
    let write_set = WriteSet::default();
    let result = kaisa_daemon::spec_agent::draft_spec(
        kaisa_daemon::GoalRequest {
            repository: &repository,
            goal: &requirements,
            base: "HEAD",
            state_directory: state,
            write_set: &write_set,
            role: Role::Reviewer,
            launch: &launch,
            kaisa_executable: &executable,
            confine_agent: policy.confine_agents,
            egress_hosts: &egress,
            agent_writable: writable,
            local_model_port: None,
            observed: false,
            reduced_mode: &basis,
        },
        &requirements,
        id,
    )?;
    if !result.attempt.confined {
        writeln!(
            out,
            "spec agent containment: reduced ({:?})",
            result.attempt.missing
        )
        .map_err(|e| e.to_string())?;
    }
    let stored = store
        .draft(result.specification)
        .map_err(|e| e.to_string())?;
    print_spec(&stored, out).map_err(|e| e.to_string())
}
pub(crate) fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    let result = (|| -> Result<(), String> {
        let state = crate::doctor::state_directory().map_err(|e| e.to_string())?;
        kaisa_sys::ensure_private_directory(&state).map_err(|e| e.to_string())?;
        let store = SpecStore::new(&state);
        match args {
            [verb, rest @ ..] if verb == "draft" => draft(rest, &state, &store, out),
            [verb] if verb == "list" => {
                for spec in store.list().map_err(|e| e.to_string())? {
                    print_spec(&spec, out).map_err(|e| e.to_string())?;
                }
                Ok(())
            }
            [verb, id, rest @ ..] if verb == "show" => {
                let version = match rest {
                    [] => None,
                    [flag, version] if flag == "--version" => Some(
                        version
                            .parse()
                            .map_err(|_| "version must be a positive integer")?,
                    ),
                    _ => return Err(USAGE.into()),
                };
                let spec = store
                    .read(&SpecId::try_new(id)?, version)
                    .map_err(|e| e.to_string())?;
                print_spec(&spec, out).map_err(|e| e.to_string())
            }
            [verb, id, rest @ ..] if verb == "approve" => {
                let provisional = match rest {
                    [] => false,
                    [flag] if flag == "--provisional" => true,
                    _ => return Err(USAGE.into()),
                };
                let id = SpecId::try_new(id)?;
                let current = store.read(&id, None).map_err(|e| e.to_string())?;
                let policy = crate::policy::load(&state).map_err(|e| e.to_string())?;
                let approval = if provisional {
                    SpecApproval::Provisional {
                        policy: "allow_provisional_spec".into(),
                    }
                } else {
                    confirm(&mut Terminal, &current.digest)?
                };
                let frozen = store
                    .approve(&id, current.specification.version, &approval, &policy)
                    .map_err(|e| e.to_string())?;
                print_spec(&frozen, out).map_err(|e| e.to_string())
            }
            _ => Err(USAGE.into()),
        }
    })();
    match result {
        Ok(()) => Ok(crate::EXIT_SUCCESS),
        Err(problem) => {
            writeln!(err, "kaisa spec: {problem}")?;
            Ok(crate::EXIT_FAILURE)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicU64, Ordering};

    use kaisa_domain::{AcceptanceCriterion, CriterionKind, SpecId, Specification};
    use kaisa_ledger::spec::SpecStore;

    static NEXT: AtomicU64 = AtomicU64::new(0);
    const SPEC_ID: &str = "spec_sessions";
    const REQUIREMENT: &str = "Preserve the session.";
    fn executable() -> PathBuf {
        std::env::current_exe()
            .expect("test executable")
            .parent()
            .expect("deps directory")
            .parent()
            .expect("debug directory")
            .join(format!("kaisa{}", std::env::consts::EXE_SUFFIX))
    }
    fn session_spec() -> Specification {
        Specification {
            id: SpecId::try_new(SPEC_ID).expect("valid spec id"),
            version: 1,
            title: "Sessions".into(),
            requirements: kaisa_domain::spec::split_requirements(REQUIREMENT),
            criteria: vec![AcceptanceCriterion {
                id: "AC-1".into(),
                requirement_ids: vec!["R1".into()],
                given: "a session".into(),
                when: "refreshed".into(),
                then: "preserved".into(),
                kind: CriterionKind::Normal,
                correction: false,
            }],
            ambiguities: Vec::new(),
            invariants: Vec::new(),
            error_behaviour: Vec::new(),
        }
    }
    struct Scenario {
        root: PathBuf,
    }
    impl Scenario {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "kaisa-spec-cli-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).expect("isolated test directory");
            Self { root }
        }
        fn state(&self) -> PathBuf {
            self.root.join("state")
        }
        fn store(&self) -> SpecStore {
            SpecStore::new(&self.state())
        }
        fn seed(&self) -> kaisa_ledger::spec::StoredSpec {
            self.store()
                .draft(session_spec())
                .expect("reviewable draft seeded")
        }
        fn command(&self, args: &[&str]) -> Output {
            self.command_at(args, &self.root)
        }
        fn command_at(&self, args: &[&str], cwd: &Path) -> Output {
            Command::new(executable())
                .args(args)
                .env("KAISA_STATE_DIR", self.state())
                .env_remove("KAISA_ATTEMPT")
                .current_dir(cwd)
                .output()
                .expect("Kaisa test command")
        }
        fn read(&self) -> kaisa_ledger::spec::StoredSpec {
            self.store()
                .read(&SpecId::try_new(SPEC_ID).expect("spec id"), None)
                .expect("spec readable")
        }
    }
    impl Drop for Scenario {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn assert_success(output: &Output) {
        assert!(output.status.success(), "command failed: {output:?}");
    }
    #[test]
    fn a_kaisa_ancestor_is_refused_even_after_descendants_scrub_the_attempt_marker() {
        const ROOT: u32 = 1;
        const KAISA: u32 = 20;
        const AGENT: u32 = 21;
        const APPROVER: u32 = 22;
        let mut rows = std::collections::BTreeMap::from([
            (ROOT, (0, "init")),
            (KAISA, (ROOT, "/Applications/Kaisa Tools/kaisa")),
            (AGENT, (KAISA, "kaisa-fake-agent")),
            (APPROVER, (AGENT, "kaisa")),
        ]);
        assert_eq!(
            super::kaisa_ancestor(&rows, APPROVER),
            Ok(true),
            "a scrubbed environment cannot hide Kaisa in the ancestor chain"
        );
        rows.insert(KAISA, (ROOT, "user-shell"));
        assert_eq!(
            super::kaisa_ancestor(&rows, APPROVER),
            Ok(false),
            "the current approval process itself is excluded from ancestor checks"
        );
        rows.remove(&KAISA);
        assert!(
            super::kaisa_ancestor(&rows, APPROVER).is_err(),
            "incomplete ancestry is refused rather than assumed human"
        );
    }
    #[test]
    fn nonterminal_cli_approval_fails_without_creating_an_approval_receipt() {
        let scenario = Scenario::new();
        scenario.seed();
        let output = scenario.command(&["spec", "approve", SPEC_ID]);
        assert!(
            !output.status.success(),
            "piped automation cannot claim human approval"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("terminal stdin AND stdout"),
            "diagnostic states terminal requirement"
        );
        assert!(
            scenario.read().approval.is_none(),
            "no human receipt is recorded on terminal refusal"
        );
    }
    #[test]
    fn provisional_cli_approval_requires_policy_and_never_displays_or_stores_human_approval() {
        let scenario = Scenario::new();
        scenario.seed();
        assert!(
            !scenario
                .command(&["spec", "approve", SPEC_ID, "--provisional"])
                .status
                .success(),
            "provisional is opt-in"
        );
        assert_success(&scenario.command(&["policy", "set", "allow_provisional_spec", "true"]));
        let output = scenario.command(&["spec", "approve", SPEC_ID, "--provisional"]);
        assert_success(&output);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("NOT human-approved"),
            "person sees an explicit provisional label"
        );
        let approval = scenario.read().approval.expect("policy receipt");
        assert_eq!(
            (approval.kind.as_str(), approval.user),
            ("provisional", None),
            "stored receipt cannot impersonate a person"
        );
        for args in [vec!["spec", "show", SPEC_ID], vec!["spec", "list"]] {
            let output = scenario.command(&args);
            assert_success(&output);
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("NOT human-approved"),
                "inspection preserves the provisional label"
            );
        }
    }
    #[test]
    fn a_fake_spec_agent_drafts_from_source_and_reports_missing_acceptance_ids() {
        let scenario = Scenario::new();
        let repository = scenario.root.join("repo");
        std::fs::create_dir_all(&repository).expect("repository directory");
        let proposal = serde_json::json!({"title":"Sessions", "criteria":[], "ambiguities":[{"id":"A-1", "requirement_ids":["R1"], "question":"Which session lifetime?"}], "invariants":[], "error_behaviour":[]});
        std::fs::write(repository.join("proposal.json"), proposal.to_string())
            .expect("fake proposal fixture");
        std::fs::write(repository.join("requirements.txt"), REQUIREMENT)
            .expect("original requirements file");
        for args in [
            vec!["init", "--quiet"],
            vec!["add", "proposal.json", "requirements.txt"],
            vec![
                "-c",
                "user.name=Kaisa Test",
                "-c",
                "user.email=kaisa-test@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        ] {
            let output = Command::new("git")
                .args(args)
                .current_dir(&repository)
                .output()
                .expect("git fixture command");
            assert_success(&output);
        }
        let fake = executable()
            .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
        let output = scenario.command_at(
            &[
                "spec",
                "draft",
                "--file",
                "requirements.txt",
                "--id",
                SPEC_ID,
                "--agent-program",
                fake.to_str().expect("test path UTF8"),
                "--agent-arg",
                "--fake-spec",
                "--agent-arg",
                "proposal.json",
            ],
            &repository,
        );
        assert_success(&output);
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains("missing acceptance criteria") && text.contains("R1"),
            "source requirement missing a criterion is named: {text}"
        );
        assert!(
            text.contains("Which session lifetime?") && text.contains("NOT human-approved"),
            "ambiguity and provisional provenance are visible"
        );
        #[cfg(unix)]
        assert!(
            text.contains("containment: reduced"),
            "the documented Unix descendant limit is explicitly labelled"
        );
        let spec = scenario.read();
        assert_eq!(
            spec.specification.requirements,
            kaisa_domain::spec::split_requirements(REQUIREMENT),
            "the agent cannot own source splitting"
        );
        assert!(
            spec.approval.is_none() && !spec.frozen,
            "drafting cannot approve or freeze"
        );
        let attempts =
            std::fs::read_dir(scenario.state().join("attempts")).expect("attempt evidence");
        for attempt in attempts {
            let bridge: serde_json::Value = serde_json::from_slice(
                &std::fs::read(
                    attempt
                        .expect("attempt directory")
                        .path()
                        .join("bridge.json"),
                )
                .expect("bridge config"),
            )
            .expect("bridge JSON");
            assert_eq!(
                bridge["role"], "reviewer",
                "spec synthesis runs with the existing read-only role"
            );
            assert_eq!(
                bridge["write_set"],
                serde_json::json!([]),
                "the specification agent owns no writable paths"
            );
        }
    }
    #[cfg(unix)]
    fn pty_approve(scenario: &Scenario, answer: &str, marker: bool) -> Output {
        const SCRIPT: &str = r#"
import errno, os, pty, select, sys, time
exe, state, identity, answer, marker = sys.argv[1:]
pid, fd = pty.fork()
if pid == 0:
    os.environ['KAISA_STATE_DIR'] = state
    os.environ.pop('KAISA_ATTEMPT', None)
    os.environ['PATH'] = '/nonexistent-approval-tools'
    if marker == 'true': os.environ['KAISA_ATTEMPT'] = 'att_fake'
    os.execv(exe, [exe, 'spec', 'approve', identity])
output = b''
sent = False
deadline = time.monotonic() + 20
while time.monotonic() < deadline:
    ready, _, _ = select.select([fd], [], [], 0.1)
    if ready:
        try: chunk = os.read(fd, 65536)
        except OSError as e:
            if e.errno == errno.EIO: break
            raise
        if not chunk: break
        output += chunk
        if b'hex characters to approve:' in output and not sent:
            os.write(fd, answer.encode() + b'\n')
            sent = True
    exited, status = os.waitpid(pid, os.WNOHANG)
    if exited:
        sys.stdout.buffer.write(output)
        sys.exit(os.waitstatus_to_exitcode(status))
else:
    os.kill(pid, 9)
    raise RuntimeError('approval did not finish')
_, status = os.waitpid(pid, 0)
sys.stdout.buffer.write(output)
sys.exit(os.waitstatus_to_exitcode(status))
"#;
        Command::new("python3")
            .arg("-c")
            .arg(SCRIPT)
            .arg(executable())
            .arg(scenario.state())
            .arg(SPEC_ID)
            .arg(answer)
            .arg(marker.to_string())
            .output()
            .expect("Unix PTY test harness")
    }
    #[cfg(unix)]
    #[test]
    fn a_real_pty_accepts_only_a_persons_correct_digest_and_rejects_agent_markers() {
        let confirmed = Scenario::new();
        let draft = confirmed.seed();
        let output = pty_approve(
            &confirmed,
            &draft.digest[..kaisa_domain::spec::approval::CONFIRMATION_PREFIX_LEN],
            false,
        );
        assert_success(&output);
        let approval = confirmed.read().approval.expect("human receipt");
        assert_eq!(
            approval.kind, "human-approved",
            "actual terminal confirmation records human provenance"
        );
        assert!(
            approval.user.is_some(),
            "OS identity accompanies terminal approval"
        );
        let wrong = Scenario::new();
        wrong.seed();
        let output = pty_approve(&wrong, "incorrect", false);
        assert!(
            !output.status.success(),
            "TTY presence alone cannot approve a mismatched digest"
        );
        assert!(
            wrong.read().approval.is_none(),
            "wrong confirmation leaves no receipt"
        );
        let agent = Scenario::new();
        let draft = agent.seed();
        let output = pty_approve(
            &agent,
            &draft.digest[..kaisa_domain::spec::approval::CONFIRMATION_PREFIX_LEN],
            true,
        );
        assert!(
            !output.status.success(),
            "agent marker refuses even actual terminal access"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("forbidden inside a Kaisa attempt"),
            "marker rejection is explicit"
        );
        assert!(
            agent.read().approval.is_none(),
            "an agent-owned PTY never mints a human receipt"
        );
    }
}
