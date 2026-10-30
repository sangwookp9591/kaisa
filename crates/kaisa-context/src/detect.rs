//! Deriving a project profile from the files in a repository (architecture §5.9).
//!
//! Detection only reads manifest files. It never runs anything; confirming a command by running
//! it is a separate step.

use std::io;
use std::path::Path;

use crate::profile::{Confirmation, DetectedCommand, Language, ProjectProfile, Purpose};

/// Derives the profile of the repository rooted at `root`.
///
/// # Errors
///
/// Returns the I/O error if a manifest exists but cannot be read.
pub fn derive_profile(root: &Path) -> io::Result<ProjectProfile> {
    let mut profile = ProjectProfile::default();
    if root.join("Cargo.toml").is_file() {
        add_rust(&mut profile);
    }
    let package_json = root.join("package.json");
    if package_json.is_file() {
        add_node(&mut profile, root, &std::fs::read_to_string(&package_json)?)?;
    }
    Ok(profile)
}

/// What `npm init` writes as the test script: a placeholder, not a test suite.
const NPM_PLACEHOLDER_TEST: &str = "no test specified";

fn add_node(profile: &mut ProjectProfile, root: &Path, manifest: &str) -> io::Result<()> {
    let parsed: serde_json::Value = serde_json::from_str(manifest)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    profile.languages.push(Language::Node);
    let manager: &str = node_package_manager(root);
    let scripts = parsed.get("scripts").and_then(serde_json::Value::as_object);
    for (purpose, script) in [
        (Purpose::Build, "build"),
        (Purpose::Test, "test"),
        (Purpose::Lint, "lint"),
    ] {
        let Some(body) = scripts
            .and_then(|scripts| scripts.get(script))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        if purpose == Purpose::Test && body.contains(NPM_PLACEHOLDER_TEST) {
            continue;
        }
        profile
            .commands
            .push(detected(Language::Node, purpose, &[manager, "run", script]));
    }
    Ok(())
}

/// The package manager a Node project uses, judged by its lockfile; npm when there is none.
fn node_package_manager(root: &Path) -> &'static str {
    [
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lock", "bun"),
        ("bun.lockb", "bun"),
    ]
    .into_iter()
    .find(|(lockfile, _)| root.join(lockfile).is_file())
    .map_or("npm", |(_, manager)| manager)
}

fn add_rust(profile: &mut ProjectProfile) {
    profile.languages.push(Language::Rust);
    // `--workspace` so a root package with members still covers every member.
    for (purpose, argv) in [
        (Purpose::Build, &["cargo", "build", "--workspace"][..]),
        (Purpose::Test, &["cargo", "test", "--workspace"][..]),
        (
            Purpose::Lint,
            &["cargo", "clippy", "--workspace", "--all-targets"][..],
        ),
    ] {
        profile
            .commands
            .push(detected(Language::Rust, purpose, argv));
    }
}

fn detected(language: Language, purpose: Purpose, argv: &[&str]) -> DetectedCommand {
    DetectedCommand {
        language,
        purpose,
        argv: argv.iter().map(|&part| part.to_owned()).collect(),
        directory: None,
        confirmation: Confirmation::Detected,
    }
}

#[cfg(test)]
mod tests {
    use super::derive_profile;
    use crate::profile::{Language, ProjectProfile, Purpose};
    use crate::test_support::ScratchProject;

    /// A pnpm project with test and lint scripts but no build step.
    const PNPM_PACKAGE_JSON: &str =
        r#"{ "name": "web", "scripts": { "test": "vitest run", "lint": "eslint ." } }"#;
    /// A package with only a dev server script.
    const PACKAGE_WITHOUT_TESTS: &str = r#"{ "name": "site", "scripts": { "dev": "vite" } }"#;
    /// What `npm init` leaves behind as the test script.
    const NPM_INIT_PACKAGE_JSON: &str = r#"{ "name": "fresh", "scripts": { "test": "echo \"Error: no test specified\" && exit 1" } }"#;

    /// The manifest of a small Cargo workspace.
    const CARGO_WORKSPACE_MANIFEST: &str = "[workspace]\nmembers = [\"crates/*\"]\n";

    fn argv_for(profile: &ProjectProfile, purpose: Purpose) -> Option<Vec<&str>> {
        profile
            .commands
            .iter()
            .find(|command| command.purpose == purpose)
            .map(|command| command.argv.iter().map(String::as_str).collect())
    }

    #[test]
    fn a_cargo_workspace_is_detected_as_rust_with_cargo_commands() {
        let project: ScratchProject =
            ScratchProject::with_files(&[("Cargo.toml", CARGO_WORKSPACE_MANIFEST)]);

        let profile: ProjectProfile =
            derive_profile(project.root()).expect("the manifest is readable");

        assert_eq!(
            profile.languages,
            vec![Language::Rust],
            "a Cargo manifest at the root makes a Rust project"
        );
        assert_eq!(
            argv_for(&profile, Purpose::Test),
            Some(vec!["cargo", "test", "--workspace"]),
            "tests must cover every workspace member, not only the root package"
        );
    }

    #[test]
    fn a_pnpm_project_uses_its_test_and_lint_scripts() {
        let project: ScratchProject = ScratchProject::with_files(&[
            ("package.json", PNPM_PACKAGE_JSON),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
        ]);

        let profile: ProjectProfile =
            derive_profile(project.root()).expect("the manifest is readable");

        assert_eq!(
            (
                argv_for(&profile, Purpose::Test),
                argv_for(&profile, Purpose::Lint)
            ),
            (
                Some(vec!["pnpm", "run", "test"]),
                Some(vec!["pnpm", "run", "lint"])
            ),
            "the project's own scripts run through the package manager its lockfile names"
        );
    }

    #[test]
    fn a_project_without_a_test_script_has_no_test_command() {
        let project: ScratchProject =
            ScratchProject::with_files(&[("package.json", PACKAGE_WITHOUT_TESTS)]);

        let profile: ProjectProfile =
            derive_profile(project.root()).expect("the manifest is readable");

        assert_eq!(
            argv_for(&profile, Purpose::Test),
            None,
            "Kaisa must not invent a test command, or results would look verified when they are not"
        );
    }

    #[test]
    fn the_npm_init_placeholder_is_not_a_test_command() {
        let project: ScratchProject =
            ScratchProject::with_files(&[("package.json", NPM_INIT_PACKAGE_JSON)]);

        let profile: ProjectProfile =
            derive_profile(project.root()).expect("the manifest is readable");

        assert_eq!(
            argv_for(&profile, Purpose::Test),
            None,
            "a script that always fails with 'no test specified' is not a test suite"
        );
    }
}
