//! Test vocabulary for workspaces: temporary git repositories built from story steps.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::git::run_git;

/// Distinguishes repositories created by tests running in parallel in one process.
static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);

/// Identity used for commits made by test fixtures (never the developer's own config).
const FIXTURE_IDENTITY: [&str; 4] = [
    "-c",
    "user.name=Kaisa Test",
    "-c",
    "user.email=kaisa-test@example.invalid",
];

/// A git repository in its own temporary directory, removed when dropped. Worktrees created
/// by tests live next to it inside the same directory.
pub(crate) struct TemporaryRepository {
    root: PathBuf,
    repository: PathBuf,
}

impl TemporaryRepository {
    /// The repository's working directory.
    pub(crate) fn path(&self) -> &Path {
        &self.repository
    }

    /// A path next to the repository where a worktree named `name` may be created.
    pub(crate) fn sibling(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Writes `contents` to `relative_path` and commits it with `message`.
    pub(crate) fn commit_file(&self, relative_path: &str, contents: &str, message: &str) {
        let file: PathBuf = self.path().join(relative_path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("the fixture directory is writable");
        }
        std::fs::write(&file, contents).expect("the fixture file is writable");
        git(self.path(), &["add", relative_path]);
        let mut args: Vec<&str> = FIXTURE_IDENTITY.to_vec();
        args.extend(["commit", "--quiet", "-m", message]);
        git(self.path(), &args);
    }
}

impl Drop for TemporaryRepository {
    fn drop(&mut self) {
        // Best effort: a leftover temp directory must not fail the test that used it.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(directory: &Path, args: &[&str]) {
    run_git(directory, args).expect("fixture git commands succeed");
}

/// A new repository whose `main` branch has a single commit adding a README.
pub(crate) fn a_repository_with_one_commit_on_main() -> TemporaryRepository {
    let root: PathBuf = std::env::temp_dir().join(format!(
        "kaisa-workspace-test-{}-{}",
        std::process::id(),
        NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed)
    ));
    let repository_path: PathBuf = root.join("repo");
    std::fs::create_dir_all(&repository_path).expect("the temp directory is writable");
    git(
        &repository_path,
        &["init", "--quiet", "--initial-branch=main"],
    );
    let repository = TemporaryRepository {
        root,
        repository: repository_path,
    };
    repository.commit_file("README.md", "# demo\n", "start the demo project");
    repository
}
