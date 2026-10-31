//! Fake-agent ledgers shared by render tests and the binary's plain-mode tests.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub(crate) const VERIFIED_GOAL: &str = "Write the session notes in two steps.";
pub(crate) const WAITING_GOAL: &str = "Review the repository permission change.";
pub(crate) const FAILED_GOAL: &str = "Write notes but require an unavailable file.";
pub(crate) const REQUIRED_REVISION: &str = "src/step-2.md";

pub(crate) struct Workbench {
    pub(crate) base: PathBuf,
}

impl Workbench {
    pub(crate) fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!("kaisa-tui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("repo/src")).expect("isolated repository directory");
        std::fs::create_dir_all(base.join("home")).expect("isolated HOME");
        std::fs::write(base.join("repo/src/lib.rs"), "pub fn login() {}\n")
            .expect("source written");
        let bench = Self { base };
        bench.git(&["init", "--quiet", "--initial-branch=main"]);
        bench.git(&["add", "."]);
        bench.git(&[
            "-c",
            "user.name=Kaisa Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "initial",
        ]);
        bench
    }

    pub(crate) fn state(&self) -> PathBuf {
        self.base.join("state")
    }

    pub(crate) fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(self.base.join("repo"))
            .output()
            .expect("git installed");
        assert!(output.status.success(), "fixture git {args:?}: {output:?}");
    }

    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(binary("kaisa"));
        command
            .current_dir(self.base.join("repo"))
            .env("KAISA_STATE_DIR", self.state())
            .env("HOME", self.base.join("home"))
            .env("USERPROFILE", self.base.join("home"));
        command
    }

    fn team(&self, goal: &str, reviewer_requires: &str) -> Output {
        let mut command = self.command();
        command.args([
            "--agent-program",
            &binary("kaisa-fake-agent").to_string_lossy(),
            "--agent-arg",
            "--fake-write-next",
            "--agent-arg",
            "src/step",
            "--reviewer-arg",
            "--fake-review-require",
            "--reviewer-arg",
            reviewer_requires,
        ]);
        if cfg!(windows) {
            command.args([
                "--check-program",
                "cmd",
                "--check-arg",
                "/c",
                "--check-arg",
                "exit 0",
            ]);
        } else {
            command.args([
                "--check-program",
                "sh",
                "--check-arg",
                "-c",
                "--check-arg",
                "exit 0",
            ]);
        }
        command.arg(goal).output().expect("fake-agent goal runs")
    }

    pub(crate) fn recorded_home(name: &str) -> Self {
        let bench = Self::new(name);
        let delivered = bench.team(VERIFIED_GOAL, REQUIRED_REVISION);
        assert!(
            delivered.status.success(),
            "scripted developer/reviewer revision must deliver: {delivered:?}"
        );
        let refused = bench.team(FAILED_GOAL, "src/missing.md");
        assert_eq!(
            refused.status.code(),
            Some(i32::from(kaisa_cli_exit_failed())),
            "bounded failed reviews must remain undelivered: {refused:?}"
        );
        // This genuine verification gate stops before spawn; it must never launch a real agent.
        std::fs::create_dir_all(bench.base.join("repo/.claude")).expect("settings directory");
        std::fs::write(
            bench.base.join("repo/.claude/settings.json"),
            r#"{"permissions":{"allow":["Bash(*)"]}}"#,
        )
        .expect("risky test settings");
        bench.git(&["add", ".claude/settings.json"]);
        bench.git(&[
            "-c",
            "user.name=Kaisa Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "permission change",
        ]);
        let waiting = bench.team(WAITING_GOAL, REQUIRED_REVISION);
        assert_eq!(
            waiting.status.code(),
            Some(i32::from(kaisa_cli_exit_waiting())),
            "risky settings must produce a real NeedsPerson gate: {waiting:?}"
        );
        bench
    }
}

// Keep this shared fixture usable inside kaisa-cli unit tests and bin/kaisa integration tests.
const fn kaisa_cli_exit_failed() -> u8 {
    4
}
const fn kaisa_cli_exit_waiting() -> u8 {
    3
}

pub(crate) fn binary(name: &str) -> PathBuf {
    let executable = std::env::current_exe().expect("test executable path");
    let debug = executable
        .parent()
        .expect("deps directory")
        .parent()
        .expect("target directory");
    let binary = debug.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        binary.is_file(),
        "build kaisa and kaisa-fake-agent before isolated TUI tests: {}",
        binary.display()
    );
    binary
}

impl Drop for Workbench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

pub(crate) fn directory_bytes(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn collect(root: &Path, path: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
        if !path.exists() {
            return;
        }
        for entry in std::fs::read_dir(path).expect("readable state") {
            let path = entry.expect("state entry").path();
            if path.is_dir() {
                files.push((
                    path.strip_prefix(root).expect("inside fixture").to_owned(),
                    Vec::new(),
                ));
                collect(root, &path, files);
            } else {
                files.push((
                    path.strip_prefix(root).expect("inside fixture").to_owned(),
                    std::fs::read(&path).expect("readable fixture file"),
                ));
            }
        }
    }
    let mut files = Vec::new();
    collect(path, path, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}
