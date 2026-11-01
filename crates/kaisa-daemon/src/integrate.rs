//! Host-owned three-way integration of preserved commits. Git writes trees and commits only;
//! neither the user's checkout nor an agent's mutable worktree is used for merging.

use std::fmt;
use std::path::Path;
use std::process::{Command, Output};

use kaisa_domain::{RepoPath, WriteSet};
use kaisa_workspace::{Candidate, changed_paths, freeze_candidate};

/// A blocking integration failure; conflicting paths are retained for the ledger and UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationError {
    /// Git could not run or construct a merge.
    Git(String),
    /// Git found a textual or structural three-way conflict.
    Conflict {
        /// Unmerged paths reported by Git.
        files: Vec<String>,
    },
    /// A result changed files it did not own.
    OutsideWriteSet {
        /// Paths changed outside declared ownership.
        files: Vec<String>,
    },
}

impl IntegrationError {
    /// Files requiring a person or a separately verified resolver task.
    #[must_use]
    pub fn files(&self) -> &[String] {
        match self {
            Self::Conflict { files } | Self::OutsideWriteSet { files } => files,
            Self::Git(_) => &[],
        }
    }
}

impl fmt::Display for IntegrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git(message) => write!(f, "integration git error: {message}"),
            Self::Conflict { files } => write!(f, "blocking merge conflict: {}", files.join(", ")),
            Self::OutsideWriteSet { files } => write!(
                f,
                "blocking writes outside task ownership: {}",
                files.join(", ")
            ),
        }
    }
}
impl std::error::Error for IntegrationError {}

/// A unique host-owned empty hooks directory, never a path inside an agent's checkout.
struct EmptyIntegrationHooks(std::path::PathBuf);
impl EmptyIntegrationHooks {
    fn create() -> Result<Self, IntegrationError> {
        let path = std::env::temp_dir().join(format!(
            "kaisa-integration-hooks-{}",
            crate::ids::fresh_suffix()
        ));
        if path.to_str().is_none() {
            return Err(IntegrationError::Git(
                "integration hooks path is not UTF-8".into(),
            ));
        }
        let builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder.create(&path).map_err(|error| {
            IntegrationError::Git(format!("cannot disable integration hooks: {error}"))
        })?;
        Ok(Self(path))
    }
}
impl Drop for EmptyIntegrationHooks {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

fn git(repository: &Path, args: &[&str]) -> Result<Output, IntegrationError> {
    let hooks = EmptyIntegrationHooks::create()?;
    Command::new("git")
        .arg("-c")
        .arg(format!(
            "core.hooksPath={}",
            kaisa_workspace::git_cli_path(&hooks.0)
        ))
        .args(["-c", "core.fsmonitor=false", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(repository)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Kaisa")
        .env("GIT_AUTHOR_EMAIL", "kaisa@example.invalid")
        .env("GIT_COMMITTER_NAME", "Kaisa")
        .env("GIT_COMMITTER_EMAIL", "kaisa@example.invalid")
        .output()
        .map_err(|error| IntegrationError::Git(error.to_string()))
}

/// A repository's configured or built-in union drivers must not silently select content.
/// Unknown attribute driver names fall back to text; registered external drivers are disabled.
fn merge_tree(repository: &Path, current: &str, next: &str) -> Result<Output, IntegrationError> {
    let configured = git(
        repository,
        &[
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^merge\..*\.driver$",
        ],
    )?;
    if !configured.status.success() && configured.status.code() != Some(1) {
        return Err(IntegrationError::Git(
            String::from_utf8_lossy(&configured.stderr).into_owned(),
        ));
    }
    let mut args = vec![
        "-c".to_owned(),
        "merge.default=text".to_owned(),
        "-c".to_owned(),
        "merge.union.driver=false".to_owned(),
    ];
    for key in configured
        .stdout
        .split(|byte| *byte == 0)
        .filter(|key| !key.is_empty())
    {
        let key =
            std::str::from_utf8(key).map_err(|error| IntegrationError::Git(error.to_string()))?;
        if key.contains(['=', '\n', '\r']) {
            return Err(IntegrationError::Git(
                "unsupported merge driver configuration key".into(),
            ));
        }
        args.extend(["-c".to_owned(), format!("{key}=false")]);
    }
    args.extend(
        [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "-z",
            current,
            next,
        ]
        .into_iter()
        .map(str::to_owned),
    );
    git(
        repository,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )
}

fn successful(output: Output) -> Result<String, IntegrationError> {
    if !output.status.success() {
        return Err(IntegrationError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Rechecks ownership against the task's own frozen base, excluding inherited dependency work.
///
/// # Errors
/// Returns a blocking outcome for any path outside the declared set.
pub fn check_write_set(
    repository: &Path,
    base: &str,
    result: &Candidate,
    allowed: &WriteSet,
) -> Result<(), IntegrationError> {
    let files: Vec<_> = changed_paths(repository, base, result)
        .map_err(|error| IntegrationError::Git(error.to_string()))?
        .into_iter()
        .filter(|path| !RepoPath::try_new(path).is_ok_and(|path| allowed.permits(&path)))
        .collect();
    if files.is_empty() {
        Ok(())
    } else {
        Err(IntegrationError::OutsideWriteSet { files })
    }
}

/// Merges every dependency/result into `base`, in the supplied deterministic dependency order.
/// A preserved reference keeps the integration alive even after scratch checkouts are removed.
///
/// # Errors
/// Returns typed conflicts with files; no agent resolves a conflict implicitly.
pub fn merge_commits(
    repository: &Path,
    base: &str,
    commits: &[String],
    reference: &str,
) -> Result<Candidate, IntegrationError> {
    let mut current = freeze_candidate(repository, base)
        .map_err(|error| IntegrationError::Git(error.to_string()))?
        .commit;
    for next in commits {
        if next == &current {
            continue;
        }
        let output = merge_tree(repository, &current, next)?;
        let fields: Vec<_> = output.stdout.split(|byte| *byte == 0).collect();
        if !output.status.success() {
            if output.status.code() == Some(1) {
                let files = fields
                    .iter()
                    .skip(1)
                    .take_while(|field| !field.is_empty())
                    .map(|field| String::from_utf8_lossy(field).into_owned())
                    .collect();
                return Err(IntegrationError::Conflict { files });
            }
            return Err(IntegrationError::Git(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let tree = String::from_utf8_lossy(fields.first().copied().unwrap_or_default());
        current = successful(git(
            repository,
            &[
                "commit-tree",
                &tree,
                "-p",
                &current,
                "-p",
                next,
                "-m",
                "Kaisa task integration",
            ],
        )?)?;
    }
    successful(git(repository, &["update-ref", reference, &current])?)?;
    freeze_candidate(repository, &current).map_err(|error| IntegrationError::Git(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use kaisa_domain::{RepoPath, WriteSet};
    use kaisa_workspace::freeze_candidate;

    use super::{IntegrationError, check_write_set, merge_commits};
    static NEXT_REPOSITORY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    struct DivergedRepository {
        path: PathBuf,
        base: String,
        left: String,
        right: String,
    }
    fn git(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Kaisa Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .current_dir(path)
            .output()
            .expect("git installed");
        assert!(
            output.status.success(),
            "test git operation succeeds: {output:?}"
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
    impl DivergedRepository {
        fn new(conflicting: bool) -> Self {
            Self::with_driver(conflicting, None)
        }
        fn with_driver(conflicting: bool, driver: Option<&str>) -> Self {
            use std::sync::atomic::Ordering;
            let path = std::env::temp_dir().join(format!(
                "kaisa-merge-test-{}-{}",
                std::process::id(),
                NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("scratch repo");
            git(&path, &["init", "--quiet", "--initial-branch=main"]);
            std::fs::write(path.join("owned.txt"), "baseline\n").expect("base file");
            git(&path, &["add", "owned.txt"]);
            if let Some(driver) = driver {
                std::fs::write(
                    path.join(".gitattributes"),
                    format!("owned.txt merge={driver}\n"),
                )
                .expect("declared merge driver");
                git(&path, &["add", ".gitattributes"]);
                if driver == "keep" {
                    git(&path, &["config", "merge.keep.driver", "true"]);
                }
            }
            git(&path, &["commit", "--quiet", "-m", "base"]);
            let base = git(&path, &["rev-parse", "HEAD"]);
            std::fs::write(path.join("owned.txt"), "left\n").expect("left file");
            git(&path, &["commit", "-qam", "left"]);
            let left = git(&path, &["rev-parse", "HEAD"]);
            git(&path, &["checkout", "--quiet", "--detach", &base]);
            let right_path = if conflicting {
                "owned.txt"
            } else {
                "other.txt"
            };
            std::fs::write(path.join(right_path), "right\n").expect("right file");
            git(&path, &["add", right_path]);
            git(&path, &["commit", "--quiet", "-m", "right"]);
            let right = git(&path, &["rev-parse", "HEAD"]);
            Self {
                path,
                base,
                left,
                right,
            }
        }
    }
    impl Drop for DivergedRepository {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
    #[test]
    fn divergent_text_changes_return_a_blocking_conflict_with_the_file() {
        let repository = DivergedRepository::new(true);
        let result = merge_commits(
            &repository.path,
            &repository.base,
            &[repository.left.clone(), repository.right.clone()],
            "refs/kaisa/test/result",
        );
        assert_eq!(
            result,
            Err(IntegrationError::Conflict {
                files: vec!["owned.txt".into()]
            }),
            "Git must not silently select a dependency or auto-resolve conflicting content"
        );
        let reference = Command::new("git")
            .args(["show-ref", "--verify", "refs/kaisa/test/result"])
            .current_dir(&repository.path)
            .output()
            .expect("git");
        assert!(
            !reference.status.success(),
            "no deliverable integration ref is written for a conflict"
        );
    }
    #[test]
    fn a_three_way_dependency_base_contains_every_preserved_parent_result() {
        let repository = DivergedRepository::new(false);
        let result = merge_commits(
            &repository.path,
            &repository.base,
            &[repository.left.clone(), repository.right.clone()],
            "refs/kaisa/test/result",
        )
        .expect("nonconflicting parents merge");
        assert_eq!(
            git(
                &repository.path,
                &["show", &format!("{}:owned.txt", result.commit)]
            ),
            "left",
            "the first parent is retained"
        );
        assert_eq!(
            git(
                &repository.path,
                &["show", &format!("{}:other.txt", result.commit)]
            ),
            "right",
            "the other parent is also retained"
        );
    }
    #[test]
    fn a_task_result_outside_its_own_base_relative_write_set_is_blocked() {
        let repository = DivergedRepository::new(false);
        let result = freeze_candidate(&repository.path, &repository.right).expect("frozen result");
        let allowed = WriteSet::new(vec![
            RepoPath::try_new("owned.txt").expect("valid owned path"),
        ]);
        assert_eq!(
            check_write_set(&repository.path, &repository.base, &result, &allowed),
            Err(IntegrationError::OutsideWriteSet {
                files: vec!["other.txt".into()]
            }),
            "preservation does not grant authority to write outside a task's declared set"
        );
    }
    #[test]
    fn repository_merge_drivers_cannot_silently_resolve_an_integration_conflict() {
        for driver in ["union", "keep"] {
            let repository = DivergedRepository::with_driver(true, Some(driver));
            let result = merge_commits(
                &repository.path,
                &repository.base,
                &[repository.left.clone(), repository.right.clone()],
                "refs/kaisa/test/result",
            );
            assert_eq!(
                result,
                Err(IntegrationError::Conflict {
                    files: vec!["owned.txt".into()]
                }),
                "{driver} must not turn a textual conflict into silent content selection"
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn integration_never_executes_repository_hooks_on_the_host() {
        use std::os::unix::fs::PermissionsExt;
        let repository = DivergedRepository::new(false);
        let marker = repository.path.join("host-hook-executed");
        let hook = repository.path.join(".git/hooks/reference-transaction");
        let quoted = marker.to_string_lossy().replace('\'', "'\\''");
        std::fs::write(&hook, format!("#!/bin/sh\nprintf invoked > '{quoted}'\n"))
            .expect("repository hook");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700))
            .expect("executable fixture");
        merge_commits(
            &repository.path,
            &repository.base,
            &[repository.left.clone(), repository.right.clone()],
            "refs/kaisa/test/result",
        )
        .expect("nonconflicting integration");
        assert!(
            !marker.exists(),
            "integration must disable hooks just like host-owned preservation"
        );
    }
}
