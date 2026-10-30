//! A revision's tracked files, addressed by Git blob IDs rather than worktree contents.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use kaisa_domain::RepoPath;

use crate::Language;

/// Metadata for one tracked blob. Git blob IDs are kept verbatim, supporting both Git hashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    /// Git's content address for this file, not a hash of its path or modification time.
    pub digest: String,
    /// The language inferred from its extension, when supported by the project profile.
    pub language: Option<Language>,
    /// The blob's size in bytes, supplied by Git without reading the blob's contents.
    pub size_bytes: u64,
}

/// A deterministic index of a repository revision. Unchanged entries are shared across refreshes.
#[derive(Debug, Clone, Default)]
pub struct RepositoryIndex {
    files: BTreeMap<RepoPath, Arc<IndexedFile>>,
}

impl RepositoryIndex {
    /// Indexes tracked blobs at `revision`; untracked and uncommitted worktree edits are ignored.
    /// Submodule commits are not files and are omitted.
    ///
    /// # Errors
    ///
    /// Returns an error if Git cannot read the tree or a path cannot be represented faithfully
    /// as a UTF-8 [`RepoPath`].
    pub fn build(repository: &Path, revision: &str) -> io::Result<Self> {
        let mut index = Self::default();
        index.refresh(repository, revision)?;
        Ok(index)
    }

    /// Updates added, changed and deleted files, returning their sorted, distinct paths.
    /// Only metadata for changed blob IDs is replaced. `ls-tree` supplies digests and sizes;
    /// neither unchanged blobs nor worktree files need to be read. A failed refresh leaves
    /// the previous index intact.
    ///
    /// # Errors
    ///
    /// Returns an error if the revision cannot be read or its tree contains an invalid path.
    pub fn refresh(&mut self, repository: &Path, revision: &str) -> io::Result<Vec<RepoPath>> {
        // Resolved first by `rev-parse --end-of-options`, so a revision such as `--output=x`
        // is only ever a name to git; `ls-tree` then sees a plain object id.
        let tree_id = git_output(
            repository,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{revision}^{{tree}}"),
            ],
        )?;
        let tree_id = String::from_utf8_lossy(&tree_id).trim().to_owned();
        let tree = git_output(repository, &["ls-tree", "-r", "-l", "-z", &tree_id])?;
        let mut next = BTreeMap::new();
        let mut changed = BTreeSet::new();
        for record in tree
            .split(|&byte| byte == 0)
            .filter(|record| !record.is_empty())
        {
            let (path, file) = parse_record(record)?;
            let Some(file) = file else {
                continue;
            };
            let entry = match self.files.get(&path) {
                Some(previous) if previous.digest == file.digest => Arc::clone(previous),
                _ => {
                    changed.insert(path.clone());
                    Arc::new(file)
                }
            };
            if next.insert(path, entry).is_some() {
                return Err(invalid_tree("tree paths collide after normalization"));
            }
        }
        changed.extend(
            self.files
                .keys()
                .filter(|path| !next.contains_key(*path))
                .cloned(),
        );
        self.files = next;
        Ok(changed.into_iter().collect())
    }

    /// The indexed file at `path`, if it is tracked at this revision.
    #[must_use]
    pub fn get(&self, path: &RepoPath) -> Option<&IndexedFile> {
        self.files.get(path).map(AsRef::as_ref)
    }

    /// All indexed files, in repository path order.
    pub fn files(&self) -> impl Iterator<Item = (&RepoPath, &IndexedFile)> {
        self.files.iter().map(|(path, file)| (path, file.as_ref()))
    }
}

fn parse_record(record: &[u8]) -> io::Result<(RepoPath, Option<IndexedFile>)> {
    let record = std::str::from_utf8(record).map_err(|error| invalid_tree(&error.to_string()))?;
    let (header, name) = record
        .split_once('\t')
        .ok_or_else(|| invalid_tree("missing tree path"))?;
    let mut fields = header.split_whitespace();
    let _mode = fields
        .next()
        .ok_or_else(|| invalid_tree("missing tree mode"))?;
    let kind = fields
        .next()
        .ok_or_else(|| invalid_tree("missing object type"))?;
    let digest = fields
        .next()
        .ok_or_else(|| invalid_tree("missing blob ID"))?;
    let size = fields
        .next()
        .ok_or_else(|| invalid_tree("missing blob size"))?;
    let path = RepoPath::try_new(name).map_err(|error| invalid_tree(&error.to_string()))?;
    if path.as_str() != name {
        return Err(invalid_tree(
            "tree path cannot be represented without normalization",
        ));
    }
    if kind != "blob" {
        return Ok((path, None));
    }
    let size_bytes = size
        .parse()
        .map_err(|error: std::num::ParseIntError| invalid_tree(&error.to_string()))?;
    let language = match Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("rs") => Some(Language::Rust),
        Some("js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts") => Some(Language::Node),
        Some("py" | "pyi") => Some(Language::Python),
        Some("go") => Some(Language::Go),
        _ => None,
    };
    Ok((
        path,
        Some(IndexedFile {
            digest: digest.to_owned(),
            language,
            size_bytes,
        }),
    ))
}

fn invalid_tree(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) fn git_output(repository: &Path, arguments: &[&str]) -> io::Result<Vec<u8>> {
    let output = Command::new("git")
        .current_dir(repository)
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output.stdout)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use kaisa_domain::RepoPath;

    use super::{RepositoryIndex, git_output};
    use crate::Language;
    use crate::test_support::ScratchProject;

    pub(crate) const SESSION_PATH: &str = "src/auth/session.rs";
    const ORIGINAL_SESSION: &str = "pub fn session() {}\n";
    const UPDATED_SESSION: &str = "pub fn session() { renew(); }\n";

    /// A committed project, cleaned up by ScratchProject on drop.
    pub(crate) struct GitProject {
        project: ScratchProject,
    }

    impl GitProject {
        pub(crate) fn with_files(files: &[(&str, &str)]) -> Self {
            let project = Self {
                project: ScratchProject::with_files(files),
            };
            project.git(&["init", "--quiet"]);
            project.commit();
            project
        }

        pub(crate) fn root(&self) -> &Path {
            self.project.root()
        }

        pub(crate) fn git(&self, arguments: &[&str]) -> Vec<u8> {
            git_output(self.root(), arguments)
                .expect("the scratch repository accepts the Git command")
        }

        pub(crate) fn commit(&self) {
            self.git(&["add", "--all"]);
            self.git(&[
                "-c",
                "user.name=Kaisa Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--quiet",
                "-m",
                "Record project files",
            ]);
        }
    }

    pub(crate) fn session_path() -> RepoPath {
        RepoPath::try_new(SESSION_PATH).expect("the session source is a repository path")
    }

    fn a_repository_with_a_session() -> GitProject {
        GitProject::with_files(&[(SESSION_PATH, ORIGINAL_SESSION)])
    }

    #[test]
    fn an_unchanged_file_is_not_reindexed() {
        let repository = a_repository_with_a_session();
        let mut index =
            RepositoryIndex::build(repository.root(), "HEAD").expect("the revision exists");
        let previous = Arc::clone(
            index
                .files
                .get(&session_path())
                .expect("the session is tracked"),
        );
        std::fs::write(repository.root().join(SESSION_PATH), UPDATED_SESSION)
            .expect("the worktree is writable");

        let changed = index
            .refresh(repository.root(), "HEAD")
            .expect("the revision remains readable");

        assert!(
            changed.is_empty(),
            "uncommitted worktree edits do not change a revision's index"
        );
        assert!(
            Arc::ptr_eq(
                &previous,
                index
                    .files
                    .get(&session_path())
                    .expect("the session remains tracked")
            ),
            "an unchanged blob retains the very same indexed entry"
        );
        assert_eq!(
            previous.language,
            Some(Language::Rust),
            "the source extension identifies Rust"
        );
    }

    #[test]
    fn a_changed_file_is_reindexed_with_its_new_digest() {
        let repository = a_repository_with_a_session();
        let mut index =
            RepositoryIndex::build(repository.root(), "HEAD").expect("the revision exists");
        let previous = index
            .get(&session_path())
            .expect("the session is indexed")
            .digest
            .clone();
        std::fs::write(repository.root().join(SESSION_PATH), UPDATED_SESSION)
            .expect("the session can be edited");
        repository.commit();

        let changed = index
            .refresh(repository.root(), "HEAD")
            .expect("the new revision exists");
        let expected = repository.git(&["rev-parse", "HEAD:src/auth/session.rs"]);
        let expected = std::str::from_utf8(&expected)
            .expect("Git IDs are ASCII")
            .trim();
        let current = index
            .get(&session_path())
            .expect("the edited session is indexed");

        assert_eq!(
            changed,
            vec![session_path()],
            "only the edited source needs indexing"
        );
        assert_eq!(
            current.digest, expected,
            "the digest is the new Git blob's content address"
        );
        assert_ne!(
            current.digest, previous,
            "different committed bytes have a different blob ID"
        );
    }

    #[test]
    fn a_deleted_file_leaves_the_index() {
        let repository = a_repository_with_a_session();
        let mut index =
            RepositoryIndex::build(repository.root(), "HEAD").expect("the revision exists");
        std::fs::remove_file(repository.root().join(SESSION_PATH))
            .expect("the session can be deleted");
        repository.git(&["add", "--all"]);
        repository.git(&[
            "-c",
            "user.name=Kaisa Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--quiet",
            "-m",
            "Delete session",
        ]);

        let changed = index
            .refresh(repository.root(), "HEAD")
            .expect("the deletion revision exists");

        assert_eq!(
            changed,
            vec![session_path()],
            "deleted paths are reported for downstream invalidation"
        );
        assert!(
            index.get(&session_path()).is_none(),
            "a deleted source must not survive in the index"
        );
    }

    #[test]
    fn a_revision_that_looks_like_an_option_is_refused_as_a_name() {
        let project = GitProject::with_files(&[(SESSION_PATH, "pub fn session() {}\n")]);

        let indexed = super::RepositoryIndex::build(project.root(), "--output=leak.txt");

        assert!(
            indexed.is_err() && !project.root().join("leak.txt").exists(),
            "an option-like revision names nothing, and git never acts on it as an option"
        );
    }
}
