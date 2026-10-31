//! Attempt ownership and failed-start recovery through the user's CLI.

use std::path::PathBuf;
use std::process::Command;
#[cfg(unix)]
use std::process::{Child, Output, Stdio};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

/// CI's exit status for a run that failed to deliver.
const RUN_FAILED: i32 = 4;
/// A process has ample time to reach a brokered write on a busy CI runner.
#[cfg(unix)]
const PROCESS_PATIENCE: Duration = Duration::from_secs(30);
/// Polling is only used to wait for observable filesystem/process state.
#[cfg(unix)]
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// The first agent's work, which must survive any external termination.
#[cfg(unix)]
const FIRST_FILE: &str = "src/first.md";
/// The content that the first agent asks its broker to write.
#[cfg(unix)]
const FIRST_CONTENT: &str = "notes from the first live attempt\n";

struct Workbench {
    root: PathBuf,
}

impl Workbench {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "kaisa-attempt-ownership-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let repository = root.join("repo");
        std::fs::create_dir_all(repository.join("src")).expect("writable fixture");
        std::fs::write(repository.join("src/lib.rs"), "pub fn login() {}\n")
            .expect("initial source written");
        for args in [
            vec!["init", "--quiet", "--initial-branch=main"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Kaisa Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "initial",
            ],
        ] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&repository)
                    .status()
                    .expect("git available")
                    .success(),
                "fixture git command succeeds"
            );
        }
        Self { root }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kaisa"));
        command
            .current_dir(self.root.join("repo"))
            .env("KAISA_STATE_DIR", self.root.join("state"));
        command
    }

    fn fake_agent(&self) -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_kaisa"))
            .with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX))
    }

    fn writing_goal(&self, path: &str) -> Command {
        let mut command = self.command();
        command.arg("--agent-program").arg(self.fake_agent()).args([
            "--agent-arg",
            "--fake-write",
            "--agent-arg",
            path,
        ]);
        command
    }

    fn watch(&self) -> String {
        let output = self.command().arg("watch").output().expect("watch runs");
        assert!(
            output.status.success(),
            "watch must read the attempt states"
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn worktrees(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.root.join("state/worktrees"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect()
    }
}

impl Drop for Workbench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A hanging fake agent with a live Kaisa owner; cleanup also runs on an assertion panic.
#[cfg(unix)]
struct RunningGoal {
    child: Option<Child>,
    identity: kaisa_sys::ProcessIdentity,
    worktree: PathBuf,
}

#[cfg(unix)]
impl RunningGoal {
    fn after_writing(bench: &Workbench) -> Self {
        let mut child = bench
            .writing_goal(FIRST_FILE)
            .args([
                "--agent-arg",
                "--fake-content",
                "--agent-arg",
                FIRST_CONTENT,
                "--agent-arg",
                "--hang-after-write",
                "Write the first notes.",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("first owner starts");
        let deadline = Instant::now() + PROCESS_PATIENCE;
        let worktree = loop {
            if let Some(worktree) = bench
                .worktrees()
                .into_iter()
                .find(|path| path.join(FIRST_FILE).exists())
            {
                break worktree;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().expect("failed owner reaped");
                panic!("first agent never wrote: {output:?}");
            }
            std::thread::sleep(POLL_INTERVAL);
        };
        let identity_path = bench
            .root
            .join("state/attempts")
            .join(worktree.file_name().expect("attempt worktree name"))
            .join("agent.identity");
        let line = std::fs::read_to_string(identity_path).expect("identity recorded before input");
        let identity =
            kaisa_sys::ProcessIdentity::from_line(&line).expect("Unix identity available");
        Self {
            child: Some(child),
            identity,
            worktree,
        }
    }

    fn finish_after_external_agent_termination(mut self) -> Output {
        self.identity
            .end_tree()
            .expect("external termination succeeds");
        let mut child = self.child.take().expect("owner still owned");
        let deadline = Instant::now() + PROCESS_PATIENCE;
        while child.try_wait().expect("owner inspected").is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("owner did not finish after its agent was ended");
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        child.wait_with_output().expect("owner output captured")
    }
}

#[cfg(unix)]
impl Drop for RunningGoal {
    fn drop(&mut self) {
        let _ = self.identity.end_tree();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(unix)]
fn first_worktree_hint(bench: &Workbench, attempt: &str) -> String {
    bench
        .root
        .join("state/worktrees")
        .join(attempt)
        .display()
        .to_string()
}

#[cfg(unix)]
fn assert_first_file_is_preserved(bench: &Workbench, attempt: &str) {
    let saved = Command::new("git")
        .args(["show", &format!("kaisa/{attempt}:{FIRST_FILE}")])
        .current_dir(bench.root.join("repo"))
        .output()
        .expect("preserved branch inspected");
    assert!(
        saved.status.success(),
        "the attempt branch must contain the agent's file: {saved:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&saved.stdout),
        FIRST_CONTENT,
        "preservation retains the exact bytes written before termination"
    );
}

#[cfg(unix)]
#[test]
fn a_second_goal_skips_the_live_owner_and_the_first_preserves_its_written_file() {
    let bench = Workbench::new("concurrent");
    let mut first = RunningGoal::after_writing(&bench);
    let attempt = first
        .worktree
        .file_name()
        .expect("attempt name")
        .to_string_lossy()
        .into_owned();
    let second = bench
        .writing_goal("src/second.md")
        .arg("Write the second notes.")
        .output()
        .expect("second goal runs");
    let survived = first.identity.is_running().expect("agent inspected")
        && first
            .child
            .as_mut()
            .expect("owner available")
            .try_wait()
            .expect("owner inspected")
            .is_none();
    let finished = first.finish_after_external_agent_termination();
    assert!(
        survived,
        "second recovery must leave the first live owner and agent untouched: {second:?}"
    );
    assert!(
        second.status.success(),
        "second goal completes independently: {second:?}"
    );
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("in use by another running Kaisa"),
        "a skipped live attempt must explain why it was left untouched"
    );
    assert_eq!(
        finished.status.code(),
        Some(RUN_FAILED),
        "externally ended agents fail the run"
    );
    assert!(
        String::from_utf8_lossy(&finished.stdout).contains("work preserved on"),
        "the first owner must report its preserved work: {finished:?}"
    );
    assert_first_file_is_preserved(&bench, &attempt);
    assert!(
        bench.worktrees().is_empty(),
        "each owner reclaims only its own worktree"
    );
}

#[cfg(unix)]
#[test]
fn a_legacy_attempt_without_an_owner_lock_is_kept_while_its_agent_is_live() {
    let bench = Workbench::new("legacy-owner");
    let first = RunningGoal::after_writing(&bench);
    let attempt = first
        .worktree
        .file_name()
        .expect("attempt name")
        .to_string_lossy()
        .into_owned();
    // Model a pre-upgrade invocation: it recorded no lock file for new Kaisa to probe.
    // Unlinking the fixture's held lock gives recovery exactly that on-disk evidence.
    std::fs::remove_file(
        bench
            .root
            .join("state/attempts")
            .join(&attempt)
            .join("owner.lock"),
    )
    .expect("legacy owner has no lock evidence");
    let recovered =
        kaisa_daemon::recover_abandoned(&bench.root.join("repo"), &bench.root.join("state"));
    let survived = first.identity.is_running().expect("legacy agent inspected");
    let finished = first.finish_after_external_agent_termination();
    assert!(
        survived,
        "an old live owner is indistinguishable from a dead old owner and must not lose its agent"
    );
    let recovered = recovered.expect("legacy attempt is reported conservatively");
    assert!(
        matches!(recovered.as_slice(), [kaisa_daemon::Recovered::Kept { reason, .. }]
        if reason.contains("no owner lock") && reason.contains("recorded by an older Kaisa")
            && reason.contains(&format!("kaisa/{attempt}"))
            && reason.contains("worktree remove")
            && reason.contains(&first_worktree_hint(&bench, &attempt))),
        "absence of a legacy owner lock cannot authorize recovery: {recovered:?}"
    );
    assert_eq!(
        finished.status.code(),
        Some(RUN_FAILED),
        "the original owner still finishes its failed run"
    );
    assert_first_file_is_preserved(&bench, &attempt);
}

#[cfg(unix)]
#[test]
fn an_externally_ended_agent_keeps_its_written_file_on_the_attempt_branch() {
    let bench = Workbench::new("external-end");
    let first = RunningGoal::after_writing(&bench);
    let attempt = first
        .worktree
        .file_name()
        .expect("attempt name")
        .to_string_lossy()
        .into_owned();
    let finished = first.finish_after_external_agent_termination();
    assert_eq!(
        finished.status.code(),
        Some(RUN_FAILED),
        "external termination is a failed run"
    );
    assert!(
        String::from_utf8_lossy(&finished.stdout).contains("work preserved on"),
        "termination must preserve writes: {finished:?}"
    );
    assert_first_file_is_preserved(&bench, &attempt);
}

#[cfg(unix)]
#[test]
fn a_dead_owners_attempt_is_preserved_by_only_one_of_two_recoverers() {
    let bench = Workbench::new("dead-owner");
    let mut first = RunningGoal::after_writing(&bench);
    let attempt = first
        .worktree
        .file_name()
        .expect("attempt name")
        .to_string_lossy()
        .into_owned();
    let mut owner = first.child.take().expect("live owner");
    owner.kill().expect("owner interrupted");
    owner.wait().expect("owner reaped and its lock released");
    let repository = bench.root.join("repo");
    let state = bench.root.join("state");
    let recovery_workers = ["this recovery", "the competing recovery"];
    let gate = std::sync::Barrier::new(recovery_workers.len());
    let recover = || {
        gate.wait();
        kaisa_daemon::recover_abandoned(&repository, &state).expect("abandoned recovery succeeds")
    };
    let recovered = std::thread::scope(|scope| {
        let other = scope.spawn(recover);
        let mut recovered = recover();
        recovered.extend(other.join().expect("second recovery returns"));
        recovered
    });
    let abandoned_attempts = [attempt.as_str()];
    assert_eq!(
        recovered
            .iter()
            .filter(|item| matches!(item, kaisa_daemon::Recovered::Preserved { .. }))
            .count(),
        abandoned_attempts.len(),
        "only one reclaimer may preserve and remove the dead owner's worktree: {recovered:?}"
    );
    assert!(
        recovered.iter().any(|item| matches!(
            item,
            kaisa_daemon::Recovered::Preserved {
                commit: Some(_),
                ..
            }
        )),
        "the dead owner's agent is proven stopped and its changes committed: {recovered:?}"
    );
    assert_first_file_is_preserved(&bench, &attempt);
    assert!(
        bench.worktrees().is_empty(),
        "a dead owner must not leave a held worktree"
    );
}

fn assert_missing_agent_is_reclaimed(observed: bool) {
    let bench = Workbench::new(if observed {
        "missing-observed"
    } else {
        "missing-unconfined"
    });
    let mut command = bench.command();
    if observed {
        command.args(["--agent", "codex"]);
    } else {
        let policy = bench
            .command()
            .args(["policy", "set", "confine_agents", "false"])
            .output()
            .expect("reduced policy configured");
        assert!(
            policy.status.success(),
            "policy must allow an explicitly accepted reduced run"
        );
        let decision_lifetime = Duration::from_secs(60);
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("wall clock available")
            .as_millis()
            + decision_lifetime.as_millis();
        command.args([
            "--accept-reduced",
            "write,tcp,udp,descendants",
            "--accept-until",
            &expires.to_string(),
            "--reviewer-arg",
            "--fake-review-require",
            "--reviewer-arg",
            "src/lib.rs",
        ]);
    }
    let failed = command
        .arg("--agent-program")
        .arg(bench.root.join("nonexistent-agent"))
        .arg("Try starting a missing agent.")
        .output()
        .expect("kaisa runs");
    assert!(
        String::from_utf8_lossy(&failed.stderr)
            .contains(&bench.root.join("nonexistent-agent").display().to_string()),
        "a launch failure must name the program the user needs to fix: {failed:?}"
    );
    let view = bench.watch();
    let leftovers = bench.worktrees();
    let next = bench
        .writing_goal("src/next.md")
        .arg("Write the next notes.")
        .output()
        .expect("next goal runs");
    assert_eq!(
        failed.status.code(),
        Some(RUN_FAILED),
        "a proven spawn failure uses the run-failed exit contract: {failed:?}"
    );
    assert!(
        view.contains("exited:failed-to-start") && view.contains("reclaimed"),
        "watch must distinguish a proven non-start from a live agent: {view}"
    );
    assert!(
        leftovers.is_empty(),
        "a failed spawn must leave no worktree: {leftovers:?}"
    );
    assert!(
        next.status.success() && !String::from_utf8_lossy(&next.stdout).contains("kept "),
        "the next goal must not inherit an unknown held attempt: {next:?}"
    );
}

#[test]
fn a_nonexistent_observed_agent_is_failed_to_start_and_reclaimed_before_the_next_goal() {
    assert_missing_agent_is_reclaimed(true);
}

#[test]
fn a_nonexistent_unconfined_agent_is_failed_to_start_and_reclaimed_before_the_next_goal() {
    assert_missing_agent_is_reclaimed(false);
}

#[cfg(unix)]
#[test]
fn a_legacy_worktree_removed_by_the_user_is_reclaimed_once_and_its_branch_is_untouched() {
    let bench = Workbench::new("legacy-removed");
    let mut first = RunningGoal::after_writing(&bench);
    let attempt = first
        .worktree
        .file_name()
        .expect("attempt name")
        .to_string_lossy()
        .into_owned();
    let mut owner = first.child.take().expect("live owner");
    owner.kill().expect("owner interrupted");
    owner.wait().expect("owner reaped");
    first.identity.end_tree().expect("agent ended by the user");
    std::fs::remove_file(
        bench
            .root
            .join("state/attempts")
            .join(&attempt)
            .join("owner.lock"),
    )
    .expect("legacy lock absent");
    let recovered =
        kaisa_daemon::recover_abandoned(&bench.root.join("repo"), &bench.root.join("state"))
            .expect("guidance returned");
    assert!(
        matches!(recovered.as_slice(), [kaisa_daemon::Recovered::Kept { .. }]),
        "legacy work must be kept until the user completes cleanup: {recovered:?}"
    );
    for args in [
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Kaisa Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "save abandoned work",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&first.worktree)
                .status()
                .expect("user git runs")
                .success(),
            "the user follows the preservation advice"
        );
    }
    let branch = format!("kaisa/{attempt}");
    let commit_before = Command::new("git")
        .args(["rev-parse", &branch])
        .current_dir(bench.root.join("repo"))
        .output()
        .expect("branch read")
        .stdout;
    assert!(
        Command::new("git")
            .args(["worktree", "remove"])
            .arg(&first.worktree)
            .current_dir(bench.root.join("repo"))
            .status()
            .expect("user removes worktree")
            .success(),
        "clean committed worktree is removable by the advised command"
    );
    let recovered =
        kaisa_daemon::recover_abandoned(&bench.root.join("repo"), &bench.root.join("state"))
            .expect("manual removal recognised");
    assert!(
        recovered
            .iter()
            .any(|item| item.summary().contains("removed by the user")),
        "completed manual cleanup must close the held attempt: {recovered:?}"
    );
    let again =
        kaisa_daemon::recover_abandoned(&bench.root.join("repo"), &bench.root.join("state"))
            .expect("next recovery succeeds");
    assert!(
        again.is_empty(),
        "manual cleanup advice must be terminal: {again:?}"
    );
    assert!(
        !bench.watch().contains(" held"),
        "the user must not see a permanently held attempt"
    );
    let commit_after = Command::new("git")
        .args(["rev-parse", &branch])
        .current_dir(bench.root.join("repo"))
        .output()
        .expect("branch read")
        .stdout;
    assert_eq!(
        commit_after, commit_before,
        "recognising user cleanup must not modify the preserved branch"
    );
    assert_first_file_is_preserved(&bench, &attempt);
}

#[cfg(unix)]
#[test]
fn a_dead_owners_lock_held_by_its_agent_is_recovered_instead_of_silently_skipped() {
    let bench = Workbench::new("hostile-holder");
    let mut first = RunningGoal::after_writing(&bench);
    let attempt = first
        .worktree
        .file_name()
        .expect("attempt name")
        .to_string_lossy()
        .into_owned();
    let directory = bench.root.join("state/attempts").join(&attempt);
    let mut owner = first.child.take().expect("live owner");
    owner.kill().expect("owner interrupted");
    owner.wait().expect("owner lock released");
    first
        .identity
        .end_tree()
        .expect("replace the agent with a hostile lock holder");
    let ready = bench.root.join("hostile-holding");
    let mut hostile = kaisa_sys::OwnedProcess::spawn(&kaisa_sys::ProcessSpec {
        program: bench.fake_agent(),
        args: vec![
            "--fake-hold-lock".to_owned(),
            directory.join("owner.lock").display().to_string(),
            "--lock-ready".to_owned(),
            ready.display().to_string(),
        ],
        working_directory: first.worktree.clone(),
        env: Vec::new(),
    })
    .expect("hostile fake agent starts as an owned group");
    first.identity = kaisa_sys::ProcessIdentity::of(hostile.id())
        .expect("holder inspected")
        .expect("holder lives");
    std::fs::write(directory.join("agent.identity"), first.identity.to_line())
        .expect("record the hostile agent's identity");
    let deadline = Instant::now() + PROCESS_PATIENCE;
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "the fake agent must acquire the released owner lock"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
    let recovered =
        kaisa_daemon::recover_abandoned(&bench.root.join("repo"), &bench.root.join("state"))
            .expect("dead owner recovered");
    assert!(
        recovered
            .iter()
            .any(|item| item.summary().contains("owner was dead")
                && item.summary().contains("was ended")),
        "a dead owner's hostile holder must be ended and explained: {recovered:?}"
    );
    assert!(
        bench.worktrees().is_empty(),
        "a read-only advisory lock must not keep an orphan's worktree held forever"
    );
    assert_first_file_is_preserved(&bench, &attempt);
    hostile.wait().expect("ended holder reaped");
}
