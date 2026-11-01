//! `kaisa spec`: draft, inspect and approve immutable specification versions.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use kaisa_domain::spec::approval::confirm_on_process_terminal;
use kaisa_domain::{ReducedModeBasis, Role, SpecApproval, SpecId, WriteSet};
use kaisa_ledger::spec::{SpecStore, StoredSpec};

const USAGE: &str = "usage: kaisa spec draft \"<requirements>\"|--file <path> [--id <spec_id>] [--agent-program <path> --agent-arg <arg> ...]\n       kaisa spec show <id> [--version <n>]\n       kaisa spec list\n       kaisa spec report <run>\n       kaisa spec approve <id> [--version <n>] [--provisional]";

fn print_spec(spec: &StoredSpec, out: &mut dyn Write) -> io::Result<()> {
    let approval = spec
        .approval
        .as_ref()
        .map_or("provisional draft (NOT human-approved)", |a| {
            if a.kind == "human-approved" {
                if a.receipt["attempts_unconfined_since_draft"] == true {
                    "human-approved (reduced assurance)"
                } else {
                    "human-approved"
                }
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
            [verb, run] if verb == "report" => {
                let report = kaisa_daemon::spec_flow::spec_report(&state, run)
                    .map_err(|error| error.to_string())?;
                write!(out, "{report}").map_err(|error| error.to_string())
            }
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
                let mut provisional = false;
                let mut version = None;
                let mut index = 0;
                while index < rest.len() {
                    match rest[index].as_str() {
                        "--provisional" if !provisional => provisional = true,
                        "--version" if version.is_none() => {
                            index += 1;
                            version = Some(
                                rest.get(index)
                                    .ok_or(USAGE)?
                                    .parse::<u32>()
                                    .map_err(|_| "version must be a positive integer")?,
                            );
                        }
                        _ => return Err(USAGE.into()),
                    }
                    index += 1;
                }
                let id = SpecId::try_new(id)?;
                let current = store
                    .for_approval(&id, version)
                    .map_err(|e| e.to_string())?;
                print_spec(&current, out).map_err(|e| e.to_string())?;
                writeln!(
                    out,
                    "Review: {} v{}; {} requirements; {} criteria; {} open ambiguities",
                    current.specification.title,
                    current.specification.version,
                    current.specification.requirements.len(),
                    current.specification.criteria.len(),
                    current.specification.ambiguities.len()
                )
                .map_err(|e| e.to_string())?;
                for criterion in &current.specification.criteria {
                    writeln!(out, "{} then: {}", criterion.id, criterion.then)
                        .map_err(|e| e.to_string())?;
                }
                if !current.specification.validate().is_empty() {
                    return Err("specification has unresolved validation findings".into());
                }
                out.flush().map_err(|e| e.to_string())?;
                let policy = crate::policy::load(&state).map_err(|e| e.to_string())?;
                let approval = if provisional {
                    SpecApproval::Provisional {
                        policy: "allow_provisional_spec".into(),
                    }
                } else {
                    store
                        .check_human_approval_available()
                        .map_err(|e| e.to_string())?;
                    writeln!(out, "Approval assurance: the binding control is the confinement sandbox (state-directory write isolation and PTY denial); marker and ancestry checks are best effort. Unconfined attempts can impersonate terminal users; receipts record reduced assurance.")
                        .map_err(|e| e.to_string())?;
                    out.flush().map_err(|e| e.to_string())?;
                    confirm_on_process_terminal(&current.digest)?
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
    fn nonterminal_cli_approval_fails_without_creating_an_approval_receipt() {
        let scenario = Scenario::new();
        scenario.seed();
        let output = scenario.command(&["spec", "approve", SPEC_ID]);
        assert!(
            !output.status.success(),
            "piped automation cannot claim human approval"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(if cfg!(unix) {
                "terminal stdin AND stdout"
            } else {
                "unsupported on Windows"
            }),
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
        pty_approve_mode(scenario, answer, marker, false)
    }
    #[cfg(unix)]
    fn pty_approve_mode(scenario: &Scenario, answer: &str, marker: bool, detached: bool) -> Output {
        const SCRIPT: &str = r#"
import errno, os, pty, select, sys, time
exe, state, identity, answer, marker, detached = sys.argv[1:]
if detached == 'true':
    if os.fork(): os._exit(0)
    os.setsid()
    if os.fork(): os._exit(0)
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
            .arg(detached.to_string())
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
        let shown = String::from_utf8_lossy(&output.stdout);
        let review = shown
            .find("Review: Sessions v1; 1 requirements; 1 criteria; 0 open ambiguities")
            .expect("title, version, counts and ambiguities shown");
        let criterion = shown
            .find("AC-1 then: preserved")
            .expect("criterion then-clause shown");
        let prompt = shown
            .find("Type its first 8 hex characters")
            .expect("confirmation prompt");
        assert!(
            review < prompt && criterion < prompt,
            "reviewable content precedes digest confirmation"
        );
        let approval = confirmed.read().approval.expect("human receipt");
        assert_eq!(
            approval.kind, "human-approved",
            "actual terminal confirmation records human provenance"
        );
        assert_eq!(
            approval.receipt["attempts_unconfined_since_draft"], false,
            "no unconfined activity retains full assurance"
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
    #[test]
    fn multiple_unapproved_versions_require_an_explicit_selection() {
        let scenario = Scenario::new();
        scenario.seed();
        scenario.seed();
        let output = scenario.command(&["spec", "approve", SPEC_ID]);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("--version"),
            "ambiguous selection must fail before the terminal prompt: {output:?}"
        );
    }

    #[cfg(unix)]
    fn record_attempt(scenario: &Scenario, live: bool) {
        use kaisa_domain::{Event, EventKind};
        let mut ledger = kaisa_ledger::EventStore::open(&scenario.state().join("kaisa.db"))
            .expect("ledger opens");
        let directory = scenario.state().join("attempts/att_approvaltest");
        std::fs::create_dir_all(&directory).expect("owner directory");
        std::fs::write(
            directory.join("owner.identity"),
            kaisa_sys::ProcessIdentity::of(std::process::id())
                .expect("identity lookup")
                .expect("test process alive")
                .to_line(),
        )
        .expect("live owner recorded");
        for (kind, payload) in [
            (
                EventKind::AttemptAdmitted,
                serde_json::json!({"task":"task_approvaltest",
                "containment":{"admission":"reduced", "missing":["filesystem"]}}),
            ),
            (
                EventKind::AttemptStateChanged,
                serde_json::json!({"execution":"starting"}),
            ),
        ] {
            ledger
                .append(Event {
                    kind,
                    subject: "att_approvaltest".into(),
                    payload_version: 1,
                    payload: payload.to_string(),
                })
                .expect("attempt event");
        }
        if !live {
            ledger
                .append(Event {
                    kind: EventKind::AttemptStateChanged,
                    subject: "att_approvaltest".into(),
                    payload_version: 1,
                    payload: serde_json::json!({"execution":"exited:0"}).to_string(),
                })
                .expect("exit event");
            ledger
                .append(Event {
                    kind: EventKind::AttemptStateChanged,
                    subject: "att_approvaltest".into(),
                    payload_version: 1,
                    payload: serde_json::json!({"cleanup":"reclaimed"}).to_string(),
                })
                .expect("cleanup event");
        }
    }
    #[cfg(unix)]
    #[test]
    fn live_attempts_block_even_a_double_forked_approver_without_an_attempt_marker() {
        let scenario = Scenario::new();
        let draft = scenario.seed();
        record_attempt(&scenario, true);
        let output = pty_approve_mode(&scenario, &draft.digest[..8], false, true);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("live Kaisa attempt"),
            "state-directory owner evidence survives double fork and setsid: {output:?}"
        );
        assert!(
            scenario.read().approval.is_none(),
            "no detached approval receipt"
        );
    }
    #[cfg(unix)]
    #[test]
    fn an_unconfined_attempt_after_drafting_reduces_later_human_approval_assurance() {
        let scenario = Scenario::new();
        let draft = scenario.seed();
        record_attempt(&scenario, false);
        let output = pty_approve(&scenario, &draft.digest[..8], false);
        assert_success(&output);
        let approval = scenario.read().approval.expect("human receipt");
        assert_eq!(
            approval.receipt["attempts_unconfined_since_draft"], true,
            "unconfined history is bound into the receipt"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("human-approved (reduced assurance)"),
            "the human label exposes reduced assurance"
        );
    }
}
