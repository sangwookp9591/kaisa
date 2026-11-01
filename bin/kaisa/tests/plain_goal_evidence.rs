//! Plain goals require positive evidence from recognised test harnesses.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use kaisa_context::{Confirmation, DetectedCommand, Language, Purpose};
use kaisa_daemon::{Evidence, run_trusted_checks};
use kaisa_workspace::freeze_candidate;

const LIBRARY: &str =
    "pub fn accepts() -> bool { true }\n#[test] fn acceptance() { assert!(accepts()); }\n";
const EARLY_EXIT: &str = "pub fn accepts() -> bool { std::process::exit(0) }\n#[test] fn acceptance() { assert!(accepts()); }\n";
const ZERO_TESTS: &str = "pub fn accepts() -> bool { true }\n";

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str, source: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "kaisa-plain-evidence-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let project = Self { root };
        project.write(
            "Cargo.toml",
            "[package]\nname = \"plain-evidence\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        project.write("src/lib.rs", source);
        project.git(&["init", "--quiet", "--initial-branch=main"]);
        project.commit();
        project
    }

    fn repository(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn write(&self, path: &str, source: &str) {
        let target = self.repository().join(path);
        std::fs::create_dir_all(target.parent().expect("source parent")).expect("source directory");
        std::fs::write(target, source).expect("fixture source");
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(self.repository())
            .output()
            .expect("Git installed");
        assert!(output.status.success(), "Git {args:?}: {output:?}");
    }

    fn commit(&self) {
        self.git(&["add", "."]);
        self.git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ]);
    }

    fn checks(&self, argv: &[&str]) -> Vec<Evidence> {
        let candidate = freeze_candidate(&self.repository(), "HEAD").expect("frozen candidate");
        run_trusted_checks(
            &self.repository(),
            &self.root.join("state"),
            &self.root.join("state/attempts/developer"),
            "main",
            &candidate,
            &[DetectedCommand {
                language: Language::Rust,
                purpose: Purpose::Test,
                argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
                directory: None,
                confirmation: Confirmation::Confirmed,
            }],
        )
        .expect("trusted check executes")
    }

    fn receipts(&self) -> Vec<serde_json::Value> {
        kaisa_ledger::EventStore::open_read_only(&self.root.join("state/kaisa.db"))
            .expect("recorded ledger")
            .read_from(kaisa_domain::Sequence::FIRST)
            .expect("verification events")
            .into_iter()
            .filter(|event| event.recorded.event.kind == kaisa_domain::EventKind::CheckEvidence)
            .map(|event| serde_json::from_str(&event.recorded.event.payload).expect("receipt JSON"))
            .collect()
    }

    fn goal(&self, path: &str, source: &str, check: &[&str]) -> Output {
        let binary = Path::new(env!("CARGO_BIN_EXE_kaisa"));
        let fake =
            binary.with_file_name(format!("kaisa-fake-agent{}", std::env::consts::EXE_SUFFIX));
        let mut command = Command::new(binary);
        command.args([
            "--agent-program",
            fake.to_str().expect("fake path"),
            "--agent-arg",
            "--fake-write",
            "--agent-arg",
            path,
            "--agent-arg",
            "--fake-content",
            "--agent-arg",
            source,
            "--reviewer-arg",
            "--fake-review-require",
            "--reviewer-arg",
            path,
            "--check-program",
            check[0],
        ]);
        for arg in &check[1..] {
            command.args(["--check-arg", arg]);
        }
        command
            .arg("fix acceptance")
            .current_dir(self.repository())
            .env("KAISA_STATE_DIR", self.root.join("state"))
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .expect("plain goal runs")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_cargo_check_that_exits_zero_before_finishing_its_tests_fails() {
    let project = Project::new("early-exit", EARLY_EXIT);
    let evidence = project.checks(&["cargo", "test", "--offline"]);
    assert!(
        !evidence.iter().all(Evidence::passed),
        "a successful process exit must not substitute for completed tests: {evidence:?}"
    );
}

#[test]
fn a_cargo_check_that_executes_zero_tests_fails() {
    let project = Project::new("zero-tests", ZERO_TESTS);
    let evidence = project.checks(&["cargo", "test", "--offline"]);
    assert!(
        !evidence.iter().all(Evidence::passed),
        "an empty harness supplies no positive test evidence: {evidence:?}"
    );
    assert!(
        evidence
            .iter()
            .all(|piece| piece.validation_label() == "no positive test evidence"),
        "the failure reason must distinguish absent harness completion: {evidence:?}"
    );
}

#[test]
fn a_plain_goal_that_adds_a_library_exit_zero_is_not_delivered() {
    let project = Project::new("goal-exit", LIBRARY);
    let output = project.goal("src/lib.rs", EARLY_EXIT, &["cargo", "test", "--offline"]);
    assert!(
        !output.status.success(),
        "early termination must stop delivery even with an approving fake reviewer: {output:?}"
    );
}

#[test]
fn a_legitimate_plain_goal_with_completed_tests_is_delivered() {
    let project = Project::new("goal-legitimate", LIBRARY);
    let output = project.goal(
        "src/lib.rs",
        &format!("{LIBRARY}// implemented\n"),
        &["cargo", "test", "--offline"],
    );
    assert!(
        output.status.success(),
        "ordinary completed tests must still allow delivery: {output:?}"
    );
}

#[test]
fn a_plain_goal_termination_scan_is_advisory_even_with_exit_code_only_checks() {
    let project = Project::new("goal-guard", LIBRARY);
    let output = project.goal("src/lib.rs", EARLY_EXIT, &["git", "diff", "--exit-code"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success()
            && stdout.contains("advisory: added process termination at src/lib.rs:1"),
        "plain goals expose termination without a person gate: {output:?}"
    );
    assert!(project.receipts().iter().all(|r| {
        r["weakened_tests"].as_array().expect("findings").is_empty()
            && r["advisories"]
                .as_array()
                .expect("advice")
                .iter()
                .any(|a| a == "advisory: added process termination at src/lib.rs:1")
    }));
    let ledger = kaisa_ledger::EventStore::open_read_only(&project.root.join("state/kaisa.db"))
        .expect("ledger");
    assert!(
        ledger
            .attempts_from_log()
            .expect("attempts")
            .into_iter()
            .filter_map(|a| a.context_pack)
            .filter_map(|digest| ledger.context_pack(&digest).expect("pack"))
            .any(|pack| String::from_utf8_lossy(&pack)
                .contains("advisory: added process termination at src/lib.rs:1")),
        "reviewer receives the advisory in its evidence"
    );
}

#[test]
fn an_unrecognised_plain_goal_check_keeps_exit_code_semantics_and_labels_its_receipt() {
    let project = Project::new("goal-exit-only", LIBRARY);
    let output = project.goal(
        "src/notes.md",
        "implemented",
        &["git", "diff", "--exit-code"],
    );
    assert!(
        output.status.success(),
        "non-test checks keep their successful exit semantics: {output:?}"
    );
    let receipts = project.receipts();
    assert!(
        !receipts.is_empty()
            && receipts
                .iter()
                .all(|receipt| receipt["validation"] == "exit-code only"),
        "the durable receipt must state the narrower evidence boundary: {receipts:?}"
    );
}

#[test]
fn a_plain_goal_with_no_executed_tests_records_the_missing_positive_evidence() {
    let project = Project::new("goal-zero-tests", ZERO_TESTS);
    let output = project.goal(
        "src/notes.md",
        "implemented",
        &["cargo", "test", "--offline"],
    );
    assert!(
        !output.status.success(),
        "zero-test checks must exhaust revisions instead of delivering: {output:?}"
    );
    let receipts = project.receipts();
    assert!(
        !receipts.is_empty()
            && receipts
                .iter()
                .all(|receipt| receipt["validation"] == "no positive test evidence"),
        "check receipts must state why the exit-zero checks failed: {receipts:?}"
    );
    let ledger = kaisa_ledger::EventStore::open_read_only(&project.root.join("state/kaisa.db"))
        .expect("ledger");
    let developer_packs = ledger
        .attempts_from_log()
        .expect("attempts")
        .into_iter()
        .filter_map(|attempt| attempt.context_pack)
        .filter_map(|digest| ledger.context_pack(&digest).expect("recorded pack"))
        .map(|pack| String::from_utf8_lossy(&pack).into_owned())
        .collect::<Vec<_>>();
    assert!(
        developer_packs
            .iter()
            .any(|pack| pack.contains("no positive test evidence from `")
                && pack.contains("no executed tests")),
        "bounded revisions must explain the deterministic harness failure: {developer_packs:?}"
    );
}

#[test]
fn changed_cargo_and_toolchain_configuration_are_named_as_check_definitions() {
    for (name, path) in [
        ("cargo-config", ".cargo/config.local"),
        ("toolchain", "rust-toolchain.toml"),
    ] {
        let project = Project::new(name, LIBRARY);
        project.git(&["checkout", "--quiet", "-b", "candidate"]);
        project.write(path, "# changed test environment\n");
        project.commit();
        let evidence = project.checks(&["git", "diff", "--exit-code"]);
        assert!(
            evidence.iter().all(|piece| piece
                .definitions_changed
                .iter()
                .any(|changed| changed == path)),
            "the check-definition guard must protect {path}: {evidence:?}"
        );
    }
}

#[test]
fn qa_influencing_configuration_changes_are_blocking_findings_for_plain_checks() {
    for (name, path, source) in [
        ("build-script", "build.rs", "fn main() {}"),
        ("node-options", ".npmrc", "node-options=--import=./skip.mjs"),
        (
            "test-script",
            "package.json",
            r#"{"scripts":{"test":"node --test"},"nodeOptions":"--import=./skip.mjs"}"#,
        ),
    ] {
        let project = Project::new(name, LIBRARY);
        project.git(&["checkout", "--quiet", "-b", "candidate"]);
        project.write(path, source);
        project.commit();
        let evidence = project.checks(&["git", "diff", "--exit-code"]);
        assert!(
            evidence.iter().all(|piece| piece
                .weakened_tests
                .iter()
                .any(|finding| finding.contains("QA-influencing configuration")
                    && finding.contains(path))),
            "plain checks must reuse the private-QA configuration guard for {path}: {evidence:?}"
        );
    }
}

#[cfg(unix)]
fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(unix)]
#[test]
fn node_and_npm_test_checks_require_completed_named_tests() {
    if !node_available() {
        return;
    }
    const NODE_PASS: &str = "import test from 'node:test'; import assert from 'node:assert/strict'; test('ships an executable src/cli.js', () => { assert.equal(true, true); });\n";
    const NODE_EXIT: &str = "process.exit(0);\n";
    for (name, source, should_pass) in [
        ("node-pass", NODE_PASS, true),
        ("node-exit", NODE_EXIT, false),
    ] {
        for reporter in ["tap", "spec"] {
            let project = Project::new(&format!("{name}-{reporter}"), LIBRARY);
            project.write("test.mjs", source);
            project.write(
                "package.json",
                format!(
                    r#"{{"scripts":{{"test":"node --test --test-reporter={reporter} test.mjs"}}}}"#
                )
                .as_str(),
            );
            project.commit();
            let evidence = project.checks(&[
                "node",
                "--test",
                &format!("--test-reporter={reporter}"),
                "test.mjs",
            ]);
            assert_eq!(
                evidence.iter().all(Evidence::passed),
                should_pass,
                "a Node file wrapper cannot substitute for real completed tests: {evidence:?}"
            );
            if Command::new("npm")
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
            {
                let evidence = project.checks(&["npm", "test"]);
                assert_eq!(
                    evidence.iter().all(Evidence::passed),
                    should_pass,
                    "npm node:test scripts use the same positive evidence requirement: {evidence:?}"
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn a_recognised_nextest_command_requires_positive_consistent_completion() {
    use std::os::unix::fs::PermissionsExt;
    const COMPLETED: &str =
        "PASS [0.001s] fixture acceptance\nSummary [0.001s] 1 test run: 1 passed, 0 skipped";
    const EMPTY: &str = "Summary [0.001s] 0 tests run: 0 passed, 0 skipped";
    const INCONSISTENT: &str = "Starting 1 test across 1 binary\nPASS [0.001s] fixture acceptance\nSummary [0.001s] 2 tests run: 2 passed, 0 skipped";
    for (name, summary, passes) in [
        ("nextest-pass", COMPLETED, true),
        ("nextest-empty", EMPTY, false),
        ("nextest-inconsistent", INCONSISTENT, false),
    ] {
        let project = Project::new(name, LIBRARY);
        let tools = project.root.join("tools");
        std::fs::create_dir_all(&tools).expect("trusted command directory");
        let runner = tools.join("cargo");
        // macOS /bin/sh here-documents need a temporary file outside the sandbox's
        // writable paths. Emit the canned harness output without that incidental I/O.
        std::fs::write(
            &runner,
            format!("#!/bin/sh\nprintf '%s\\n' '{summary}'\n"),
        )
        .expect("scripted nextest output");
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755))
            .expect("executable runner");
        let evidence = project.checks(&[runner.to_str().expect("runner path"), "nextest", "run"]);
        assert!(
            evidence
                .iter()
                .all(|piece| piece.exit == Some(kaisa_sys::ProcessExit::Code(0))),
            "the nextest fixture must finish successfully before evaluating its summary: {evidence:?}"
        );
        assert_eq!(
            evidence.iter().all(Evidence::passed),
            passes,
            "nextest exit zero must accompany a matching final summary and start count: {evidence:?}"
        );
        assert!(
            evidence.iter().all(|piece| piece.test_evidence.is_some()),
            "nextest must be recognised instead of falling back to exit-code only"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_plain_goal_that_adds_node_process_exit_in_a_library_is_not_delivered() {
    if !node_available() {
        return;
    }
    let project = Project::new("node-library-exit", LIBRARY);
    project.write(
        "src/library.mjs",
        "export function accepts() { return true; }\n",
    );
    project.write("test.mjs", "import test from 'node:test'; import assert from 'node:assert/strict'; import { accepts } from './src/library.mjs'; test('acceptance', () => { assert.equal(accepts(), true); });\n");
    project.commit();
    let output = project.goal(
        "src/library.mjs",
        "export function accepts() { process.exit(0); }\n",
        &["node", "--test", "--test-reporter=tap", "test.mjs"],
    );
    assert!(
        !output.status.success(),
        "Node library termination must block delivery despite an approving reviewer: {output:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("no positive test evidence from `"),
        "positive evidence, not the advisory scan, must explain the stop: {output:?}"
    );
}

#[test]
fn review_fixa_real_cargo_stdout_variants_deliver() {
    let reexec = r#"pub fn accepts() -> bool { true }
#[cfg(test)] mod tests {
    #[test] fn child_probe() { assert!(super::accepts()); }
    #[test] fn parent() {
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "tests::child_probe"]).status().expect("child");
        assert!(status.success());
    }
}
"#;
    for (name, source, args) in [
        (
            "self-reexec",
            reexec.to_owned(),
            vec!["cargo", "test", "--offline"],
        ),
        (
            "nocapture",
            LIBRARY.replace(
                "assert!(accepts());",
                "println!(\"running migrations\"); assert!(accepts());",
            ),
            vec!["cargo", "test", "--offline", "--", "--nocapture"],
        ),
        (
            "color",
            LIBRARY.to_owned(),
            vec!["cargo", "test", "--offline", "--", "--color", "always"],
        ),
    ] {
        let project = Project::new(name, &source);
        let output = project.goal("src/lib.rs", &format!("{source}// comment\n"), &args);
        assert!(output.status.success(), "passing {name} goal: {output:?}");
        assert!(
            project
                .receipts()
                .iter()
                .all(|r| r["validation"] == "positive test evidence")
        );
    }
    let project = Project::new("custom-harness", LIBRARY);
    project.write("Cargo.toml", "[package]\nname = \"plain-evidence\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[[test]]\nname = \"golden\"\nharness = false\n");
    project.write(
        "tests/golden.rs",
        "fn main() { println!(\"running golden files\"); assert!(plain_evidence::accepts()); }\n",
    );
    project.commit();
    let output = project.goal(
        "src/lib.rs",
        &format!("{LIBRARY}// comment\n"),
        &["cargo", "test", "--offline"],
    );
    assert!(
        output.status.success(),
        "passing custom harness plus libtest: {output:?}"
    );
}

#[test]
fn review_fixa_no_run_is_compile_only_and_missing_evidence_is_actionable() {
    let project = Project::new("compile-only", ZERO_TESTS);
    let evidence = project.checks(&["cargo", "test", "--offline", "--no-run"]);
    assert!(
        evidence
            .iter()
            .all(|e| e.passed() && e.validation_label() == "exit-code only")
    );
    let output = project.goal("src/notes.md", "docs", &["cargo", "test", "--offline"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("no positive test evidence from `")
            && stdout.contains("the command exited 0 but reported no executed tests")
            && stdout.contains("run at least one test"),
        "actionable failure: {output:?}"
    );
    assert!(!stdout.contains("Some(Code(0))"));
}

#[test]
fn review_fixa_plain_cli_exit_aliases_are_advisory_with_passing_tests() {
    for (name, source) in [
        ("direct", "fn main() { std::process::exit(2); }"),
        (
            "module",
            "use std::process; fn main() { process::exit(2); }",
        ),
        (
            "function",
            "use std::process::exit as quit; fn main() { quit(2); }",
        ),
    ] {
        let project = Project::new(name, LIBRARY);
        let output = project.goal("src/main.rs", source, &["cargo", "test", "--offline"]);
        assert!(output.status.success(), "legitimate CLI exit: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("advisory: added process termination at src/main.rs:1")
        );
    }
}
