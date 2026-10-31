//! Preview and explicit project setup; agent integration is a separate extension.
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

use kaisa_context::{Confirmation, DetectedCommand, Language, ProjectProfile, Purpose};
use kaisa_domain::RepoPath;

/// Reads explicit checks from the working tree; invalid configuration never falls back to detection.
pub(crate) fn profile(root: &Path) -> io::Result<ProjectProfile> {
    let path = root.join("kaisa.toml");
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return kaisa_context::derive_profile(root);
        }
        Err(error) => return Err(error),
    };
    let value: toml::Value = text.parse().map_err(invalid)?;
    let table = value
        .get("profile")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| invalid("kaisa.toml needs [profile]"))?;
    let mut profile = ProjectProfile::default();
    for (key, purpose) in [
        ("test", Purpose::Test),
        ("build", Purpose::Build),
        ("lint", Purpose::Lint),
    ] {
        let Some(value) = table.get(key) else {
            continue;
        };
        let entries = value
            .as_array()
            .ok_or_else(|| invalid("checks must be arrays"))?;
        if entries.iter().all(toml::Value::is_str) {
            profile.commands.push(command(value, None, purpose)?);
        } else {
            for entry in entries {
                let argv = entry
                    .get("argv")
                    .ok_or_else(|| invalid("check needs argv"))?;
                let directory = entry
                    .get("directory")
                    .map(|value| {
                        value
                            .as_str()
                            .ok_or_else(|| invalid("directory must be a relative path"))
                            .and_then(|text| RepoPath::try_new(text).map_err(invalid))
                    })
                    .transpose()?;
                profile.commands.push(command(argv, directory, purpose)?);
            }
        }
    }
    if profile.commands.is_empty() {
        return kaisa_context::derive_profile(root);
    }
    Ok(profile)
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("kaisa.toml: {error}"))
}

fn command(
    value: &toml::Value,
    directory: Option<RepoPath>,
    purpose: Purpose,
) -> io::Result<DetectedCommand> {
    let argv = value
        .as_array()
        .ok_or_else(|| invalid("argv must be an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("argv must contain strings"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    if argv.is_empty() || argv[0].is_empty() {
        return Err(invalid("command must name a program"));
    }
    Ok(DetectedCommand {
        language: Language::Rust,
        purpose,
        argv,
        directory,
        confirmation: Confirmation::Detected,
    })
}

fn preview(profile: &ProjectProfile) -> io::Result<String> {
    let mut checks = toml::map::Map::new();
    for (key, purpose) in [
        ("test", Purpose::Test),
        ("build", Purpose::Build),
        ("lint", Purpose::Lint),
    ] {
        let entries = profile
            .commands
            .iter()
            .filter(|command| command.purpose == purpose)
            .map(|command| {
                let mut entry = toml::map::Map::new();
                entry.insert(
                    "argv".to_owned(),
                    toml::Value::Array(
                        command
                            .argv
                            .iter()
                            .cloned()
                            .map(toml::Value::String)
                            .collect(),
                    ),
                );
                if let Some(directory) = &command.directory {
                    entry.insert(
                        "directory".to_owned(),
                        toml::Value::String(directory.as_str().to_owned()),
                    );
                }
                toml::Value::Table(entry)
            })
            .collect::<Vec<_>>();
        if !entries.is_empty() {
            checks.insert(key.to_owned(), toml::Value::Array(entries));
        }
    }
    let mut root = toml::map::Map::new();
    root.insert("profile".to_owned(), toml::Value::Table(checks));
    toml::to_string_pretty(&root).map_err(invalid)
}

pub(crate) fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    if args.iter().any(|arg| arg != "--yes") {
        writeln!(err, "usage: kaisa init [--yes]")?;
        return Ok(crate::EXIT_USAGE);
    }
    let root = match crate::doctor::repository_root(&std::env::current_dir()?) {
        Some(root) => root,
        None => {
            writeln!(
                err,
                "kaisa init: no git repository; run git init and commit your project first"
            )?;
            return Ok(crate::EXIT_FAILURE);
        }
    };
    let profile = match profile(&root) {
        Ok(profile) => profile,
        Err(error) => {
            writeln!(err, "{error}; fix the configuration before init")?;
            return Ok(crate::EXIT_FAILURE);
        }
    };
    crate::doctor::show_checks(&profile, out)?;
    crate::doctor::show_manifest_warning(&root, out)?;
    crate::doctor::show_risks(&root, out)?;
    let destination = root.join("kaisa.toml");
    let mut files = integration_files(&root, out)?;
    if fs::symlink_metadata(&destination).is_ok() {
        writeln!(
            out,
            "Keeping existing {} (no changes).",
            destination.display()
        )?;
    } else {
        files.insert(
            0,
            PlannedFile {
                path: "kaisa.toml".into(),
                contents: preview(&profile)?,
                merge_into_existing: false,
            },
        );
    }
    let mut changed = Vec::new();
    for mut file in files {
        file.contents = planned_contents(&root, &file)?;
        file.merge_into_existing = false;
        if fs::read_to_string(root.join(&file.path)).is_ok_and(|existing| existing == file.contents)
        {
            continue;
        }
        writeln!(
            out,
            "Preview: {} {}\n{}",
            if root.join(&file.path).exists() {
                "update (backup first)"
            } else {
                "add"
            },
            file.path.display(),
            file.contents
        )?;
        changed.push(file);
    }
    if changed.is_empty() {
        writeln!(out, "No changes needed.")?;
        return Ok(crate::EXIT_SUCCESS);
    }
    if !args.iter().any(|arg| arg == "--yes") {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            writeln!(out, "Preview only. Run kaisa init --yes to apply.")?;
            return Ok(crate::EXIT_SUCCESS);
        }
        write!(out, "[Enter] Apply  [Q] Cancel: ")?;
        out.flush()?;
        let mut answer = String::new();
        if io::stdin().read_line(&mut answer)? == 0 || !answer.trim().is_empty() {
            writeln!(out, "Cancelled; no files changed.")?;
            return Ok(crate::EXIT_SUCCESS);
        }
    }
    for file in &changed {
        apply_file(&root, file)?;
    }
    writeln!(
        out,
        "Applied project setup. Review and commit kaisa.toml; goals use its test command."
    )?;
    Ok(crate::EXIT_SUCCESS)
}

/// Files supplied by UX-C; contents are the complete desired file after any merge.
/// Existing files are backed up when replacement is explicitly requested.
pub(crate) struct PlannedFile {
    pub(crate) path: std::path::PathBuf,
    pub(crate) contents: String,
    pub(crate) merge_into_existing: bool,
}

/// Claude Code integration: the `/kaisa` slash command. A file that is already right is skipped;
/// a different file at the same path is left alone and named. The MCP server is registered at user
/// scope by the person (printed here), not written into the repository.
fn integration_files(repo: &Path, out: &mut dyn Write) -> io::Result<Vec<PlannedFile>> {
    let mut files = Vec::new();
    for file in crate::claude_code::claude_code_integration(repo) {
        match file.action {
            crate::claude_code::PlannedAction::Create
            | crate::claude_code::PlannedAction::Merge => {
                files.push(PlannedFile {
                    path: file.path,
                    contents: file.contents,
                    merge_into_existing: false,
                });
            }
            crate::claude_code::PlannedAction::Unchanged => {}
            crate::claude_code::PlannedAction::Conflict(reason) => {
                writeln!(out, "Not changing {}: {reason}", file.path.display())?;
            }
        }
    }
    writeln!(
        out,
        "To use /kaisa in Claude Code, register Kaisa once for your user: {}",
        crate::claude_code::USER_SCOPE_REGISTRATION
    )?;
    Ok(files)
}

fn planned_contents(root: &Path, file: &PlannedFile) -> io::Result<String> {
    if file.path.is_absolute()
        || file
            .path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(invalid("integration path must be repository-relative"));
    }
    let mut path = root.to_owned();
    for part in file.path.components() {
        path.push(part);
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(invalid("integration paths must not follow symlinks"));
        }
    }
    let existing = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(file.contents.clone()),
        Err(error) => return Err(error),
    };
    if !file.merge_into_existing || existing == file.contents {
        return Ok(file.contents.clone());
    }
    if file
        .path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        let mut value: serde_json::Value = serde_json::from_str(&existing).map_err(invalid)?;
        let patch: serde_json::Value = serde_json::from_str(&file.contents).map_err(invalid)?;
        merge_json(&mut value, patch);
        return serde_json::to_string_pretty(&value)
            .map(|text| text + "\n")
            .map_err(invalid);
    }
    Err(invalid(
        "integration merge requires JSON; supplier must provide already merged contents for other files",
    ))
}

fn merge_json(value: &mut serde_json::Value, patch: serde_json::Value) {
    if let (Some(destination), Some(source)) = (value.as_object_mut(), patch.as_object()) {
        for (key, source) in source {
            merge_json(
                destination.entry(key).or_insert(serde_json::Value::Null),
                source.clone(),
            );
        }
    } else {
        *value = patch;
    }
}

fn apply_file(root: &Path, file: &PlannedFile) -> io::Result<()> {
    let contents = planned_contents(root, file)?;
    let destination = root.join(&file.path);
    if let Ok(existing) = fs::read_to_string(&destination) {
        if existing == contents {
            return Ok(());
        }
        // Exclusive creation keeps earlier backups, including backups from interrupted init.
        for suffix in 0.. {
            let backup = destination.with_file_name(format!(
                "{}.kaisa-backup-{suffix}",
                destination
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ));
            match fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(backup)
            {
                Ok(mut backup) => {
                    backup.write_all(existing.as_bytes())?;
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        fs::write(destination, contents)
    } else {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(destination)?
            .write_all(contents.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::{PlannedFile, apply_file, planned_contents};

    #[test]
    fn planned_integration_files_merge_preserve_settings_and_back_up_before_replacing() {
        let root =
            std::env::temp_dir().join(format!("kaisa-init-integration-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".claude")).expect("fixture root");
        let existing = "{\"permissions\":{\"deny\":[\"Read(secret)\"]},\"theme\":\"dark\"}\n";
        let destination = root.join(".claude/settings.json");
        std::fs::write(&destination, existing).expect("existing setting");
        let planned = PlannedFile {
            path: ".claude/settings.json".into(),
            contents: "{\"permissions\":{\"allow\":[\"mcp__kaisa\"]}}".into(),
            merge_into_existing: true,
        };
        let preview = planned_contents(&root, &planned).expect("preview merge");
        assert_eq!(
            std::fs::read_to_string(&destination).expect("existing file"),
            existing,
            "preview must leave existing settings unchanged"
        );
        apply_file(&root, &planned).expect("apply confirmed plan");
        let value: serde_json::Value = serde_json::from_str(&preview).expect("merged JSON");
        assert_eq!(
            value["theme"], "dark",
            "unrelated settings must survive integration"
        );
        assert_eq!(
            value["permissions"]["deny"],
            serde_json::json!(["Read(secret)"]),
            "existing restrictions must survive integration"
        );
        assert_eq!(
            std::fs::read_to_string(destination.with_file_name("settings.json.kaisa-backup-0"))
                .expect("backup"),
            existing,
            "backup must retain original bytes"
        );
        apply_file(&root, &planned).expect("repeat confirmed plan");
        assert!(
            !destination
                .with_file_name("settings.json.kaisa-backup-1")
                .exists(),
            "idempotent integration must not create redundant backups"
        );
        std::fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn an_integration_path_outside_the_repository_is_refused_before_writing() {
        let planned = PlannedFile {
            path: "../outside".into(),
            contents: "untrusted".into(),
            merge_into_existing: false,
        };
        assert!(
            planned_contents(std::path::Path::new("."), &planned).is_err(),
            "integration paths cannot escape the project root"
        );
    }
}
