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
    Ok(profile)
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
}
