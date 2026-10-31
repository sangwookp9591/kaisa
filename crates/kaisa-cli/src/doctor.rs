//! Read-only setup diagnosis. Version probes never start coding sessions.
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use kaisa_context::{ProjectProfile, Purpose};

/// Keep Home responsive even when an installed CLI hangs or prompts.
const VERSION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);
const VERSION_OUTPUT_CAP: usize = 4096;

/// Explicit checks are authoritative, but a broken optional manifest still deserves a warning.
pub(crate) fn show_manifest_warning(root: &Path, out: &mut dyn Write) -> io::Result<()> {
    if root.join("kaisa.toml").is_file() {
        match std::fs::read_to_string(root.join("package.json")) {
            Ok(text) => {
                if let Err(error) = serde_json::from_str::<serde_json::Value>(&text) {
                    writeln!(
                        out,
                        "! package.json warning: {error}; using kaisa.toml checks"
                    )?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => writeln!(
                out,
                "! package.json warning: {error}; using kaisa.toml checks"
            )?,
        }
    }
    Ok(())
}

pub(crate) fn git(root: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "submodule.recurse=false",
    ]);
    if args.first() == Some(&"status") {
        // Status can invoke configured clean/process filters while comparing tracked files.
        // Diagnostics must never execute those repository- or user-defined programs.
        if let Some(keys) = git(
            root,
            &[
                "config",
                "--null",
                "--name-only",
                "--get-regexp",
                r"^filter\..*\.(clean|process|required)$",
            ],
        ) {
            let keys = keys
                .split('\0')
                .filter(|key| !key.is_empty())
                .collect::<Vec<_>>();
            if keys
                .iter()
                .any(|key| key.contains('=') || key.chars().any(char::is_control))
            {
                return None;
            }
            for key in keys {
                command.args([
                    "-c",
                    &format!(
                        "{key}={}",
                        if key.ends_with(".required") {
                            "false"
                        } else {
                            ""
                        }
                    ),
                ]);
            }
        }
    }
    let output = command
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub(crate) fn repository_root(directory: &Path) -> Option<PathBuf> {
    git(directory, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// A line's status is stable even when stdout is not a terminal.
fn line(out: &mut dyn Write, passed: bool, text: &str, fix: &str) -> io::Result<()> {
    if passed {
        writeln!(out, "✓ {text}")
    } else {
        writeln!(out, "✗ {text} → {fix}")
    }
}

pub(crate) fn show_checks(profile: &ProjectProfile, out: &mut dyn Write) -> io::Result<bool> {
    writeln!(
        out,
        "Profile: {:?} (detected; commands have not been run)",
        profile.languages
    )?;
    for command in &profile.commands {
        writeln!(
            out,
            "  {:?}: {}{}",
            command.purpose,
            command.argv.join(" "),
            command
                .directory
                .as_ref()
                .map_or_else(String::new, |directory| format!(
                    " (in {})",
                    directory.as_str()
                ))
        )?;
    }
    let test = profile
        .commands
        .iter()
        .any(|command| command.purpose == Purpose::Test);
    line(
        out,
        test,
        if test {
            "Trusted test command found"
        } else {
            "no test command found → goals here end not delivered"
        },
        "define [profile] test in kaisa.toml or add project tests",
    )?;
    Ok(test)
}

pub(crate) fn show_risks(root: &Path, out: &mut dyn Write) -> io::Result<bool> {
    let findings = match kaisa_workspace::scan_settings(root) {
        Ok(findings) => findings,
        Err(error) => {
            line(
                out,
                false,
                &format!("Settings scan: {error}"),
                "make repository settings readable and retry kaisa doctor",
            )?;
            return Ok(false);
        }
    };
    for finding in &findings {
        line(
            out,
            false,
            &format!(
                "Risky settings: {} {:?}: {}",
                finding.path,
                finding.rule,
                visible(&finding.excerpt)
            ),
            "remove the authority expansion or fix the unreadable setting; agents cannot run with risky settings",
        )?;
    }
    if findings.is_empty() {
        writeln!(out, "✓ Settings: no risky settings found")?;
    }
    Ok(findings.is_empty())
}

fn visible(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect()
}

pub(crate) fn state_directory() -> io::Result<PathBuf> {
    kaisa_sys::state_location(|name| std::env::var_os(name)).map(|location| location.directory)
}

/// Checks the nearest existing directory without creating the state directory or a probe file.
fn state_writable(state: &Path) -> bool {
    let mut directory = state;
    while !directory.exists() {
        let Some(parent) = directory.parent() else {
            return false;
        };
        directory = parent;
    }
    directory.is_dir() && kaisa_sys::directory_writable(directory).unwrap_or(false)
}

pub(crate) fn diagnose(out: &mut dyn Write) -> io::Result<bool> {
    writeln!(out, "✓ kaisa {}", crate::VERSION)?;
    let mut ready = true;
    let mut claude_found = false;
    for (agent, install, login) in [
        (
            "claude",
            "install Claude Code from https://docs.anthropic.com/en/docs/claude-code, then run claude auth login",
            "claude auth login",
        ),
        (
            "codex",
            "install Codex from https://github.com/openai/codex, then run codex login",
            "codex login",
        ),
    ] {
        let version = kaisa_sys::run_captured(
            &kaisa_sys::ProcessSpec {
                program: PathBuf::from(agent),
                args: vec!["--version".to_owned()],
                working_directory: std::env::current_dir()?,
                env: Vec::new(),
            },
            &kaisa_sys::Confinement::Unconfined,
            &[],
            VERSION_DEADLINE,
            VERSION_OUTPUT_CAP,
        )
        .ok();
        let answered = version
            .as_ref()
            .is_some_and(|run| run.exit == Some(kaisa_sys::ProcessExit::Code(0)));
        if agent == "claude" {
            claude_found = answered;
        }
        match version {
            Some(output) if answered => writeln!(
                out,
                "✓ Agent: {agent} {}",
                visible(String::from_utf8_lossy(&output.output).trim())
            )?,
            Some(output) if output.exit.is_none() => writeln!(
                out,
                "{} Agent: {agent} did not answer --version within {} seconds → check the installation, then retry",
                if agent == "claude" { "✗" } else { "!" },
                VERSION_DEADLINE.as_secs(),
            )?,
            _ => writeln!(
                out,
                "{} Agent: {agent} not available → {install}",
                if agent == "claude" { "✗" } else { "!" }
            )?,
        }
        writeln!(
            out,
            "! {agent} login not checked (no credential files or keychain read; no network) → {login}"
        )?;
    }
    ready &= claude_found;
    let directory = std::env::current_dir()?;
    if let Some(root) = repository_root(&directory) {
        let branch = git(&root, &["symbolic-ref", "--short", "HEAD"])
            .unwrap_or_else(|| "detached HEAD".to_owned());
        let status = git(
            &root,
            &[
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--ignore-submodules=all",
            ],
        );
        let clean = status.as_ref().is_some_and(|status| status.is_empty());
        ready &= status.is_some();
        writeln!(
            out,
            "{} Repo: {} (branch {branch}, {})",
            if clean { "✓" } else { "!" },
            root.display(),
            if clean {
                "clean"
            } else if status.is_some() {
                "dirty; preserve or commit your changes before a goal"
            } else {
                "status not checked → inspect Git filter configuration and rerun kaisa doctor"
            }
        )?;
        writeln!(
            out,
            "! Git status skips external filters, fsmonitor and submodules; filtered files may appear dirty."
        )?;
        let committed = git(&root, &["rev-parse", "--verify", "HEAD"]).is_some();
        line(
            out,
            committed,
            "Repository has a commit",
            "git add your files, then git commit",
        )?;
        ready &= committed;
        match crate::init::profile(&root) {
            Ok(profile) => {
                ready &= show_checks(&profile, out)?;
                show_manifest_warning(&root, out)?;
            }
            Err(error) => {
                line(
                    out,
                    false,
                    &format!("Profile: {error}"),
                    "fix kaisa.toml or the project manifest",
                )?;
                ready = false;
            }
        }
        ready &= show_risks(&root, out)?;
    } else {
        line(
            out,
            false,
            "Current directory is not a git repository",
            "cd to your project, or git init and commit your files",
        )?;
        ready = false;
    }
    let missing = kaisa_daemon::expected_sandbox_missing(true);
    let admission = kaisa_domain::admit(&missing, None, 0);
    writeln!(
        out,
        "{} Protection preflight: {} (sandbox creation is checked again at run time)",
        if missing.is_empty() { "✓" } else { "!" },
        admission.code()
    )?;
    writeln!(
        out,
        "  Missing: {}; platform limits: {}",
        kaisa_domain::codes(&missing),
        kaisa_domain::codes(kaisa_domain::platform_limits())
    )?;
    writeln!(
        out,
        "! Claude Code default: reduced; Codex: observed, checked afterwards. Use --confine-claude for the confinement trial."
    )?;
    if matches!(admission, kaisa_domain::Admission::Refused { .. }) {
        writeln!(
            out,
            "✗ Sandbox admission needs a decision → upgrade the OS/kernel or review the missing protection before running"
        )?;
        ready = false;
    }
    match state_directory() {
        Ok(state) => {
            let writable = state_writable(&state);
            line(
                out,
                writable,
                &format!(
                    "State: {} ({})",
                    state.display(),
                    if writable {
                        "writable by permission check; no probe file created"
                    } else {
                        "not writable"
                    }
                ),
                "set KAISA_STATE_DIR to a writable directory",
            )?;
            ready &= writable;
        }
        Err(error) => {
            line(
                out,
                false,
                &format!("State: {error}"),
                "set KAISA_STATE_DIR to an absolute writable directory",
            )?;
            ready = false;
        }
    }
    Ok(ready)
}

pub(crate) fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    if !args.is_empty() {
        writeln!(err, "usage: kaisa doctor")?;
        return Ok(crate::EXIT_USAGE);
    }
    Ok(if diagnose(out)? {
        crate::EXIT_SUCCESS
    } else {
        crate::EXIT_FAILURE
    })
}
