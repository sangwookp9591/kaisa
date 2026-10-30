//! A deterministic, language-agnostic first pass at the files a change can affect.

use std::collections::BTreeSet;
use std::path::Path;

use kaisa_domain::RepoPath;

use crate::RepositoryIndex;
use crate::index::git_output;

/// Largest blob scanned for module references: one mebibyte. Larger blobs are not read.
pub const IMPACT_TEXT_LIMIT_BYTES: u64 = 1024 * 1024;

/// Returns changed paths, their directory neighbors and files naming a changed file's stem
/// as a whole word. Word characters are Unicode letters/numbers and underscore; matching is
/// case-sensitive. The result is sorted and contains no duplicates, including for deleted paths.
///
/// A private copy of `index` is refreshed to `revision`, so symbolic revisions and indexes from
/// an earlier revision cannot cause worktree edits or stale blobs to enter the result. Only
/// UTF-8 blobs of at most [`IMPACT_TEXT_LIMIT_BYTES`] are scanned for references; directory
/// neighbors are included regardless of their contents. Git read failures skip that blob;
/// if the revision itself cannot be read, only the supplied changed paths are returned.
#[must_use]
pub fn impact_set(
    index: &RepositoryIndex,
    repository: &Path,
    revision: &str,
    changed: &[RepoPath],
) -> Vec<RepoPath> {
    let mut impacted: BTreeSet<RepoPath> = changed.iter().cloned().collect();
    if changed.is_empty() {
        return Vec::new();
    }
    let mut snapshot = index.clone();
    if snapshot.refresh(repository, revision).is_err() {
        return impacted.into_iter().collect();
    }
    let directories: BTreeSet<&str> = changed.iter().map(directory).collect();
    let modules: BTreeSet<&str> = changed
        .iter()
        .filter_map(|path| {
            Path::new(path.as_str())
                .file_stem()
                .and_then(|stem| stem.to_str())
        })
        .filter(|stem| !stem.is_empty())
        .collect();
    for (path, file) in snapshot.files() {
        if directories.contains(directory(path)) {
            impacted.insert(path.clone());
            continue;
        }
        if file.size_bytes > IMPACT_TEXT_LIMIT_BYTES {
            continue;
        }
        let Ok(bytes) = git_output(repository, &["cat-file", "blob", &file.digest]) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if modules.iter().any(|module| contains_word(text, module)) {
            impacted.insert(path.clone());
        }
    }
    impacted.into_iter().collect()
}

fn directory(path: &RepoPath) -> &str {
    path.as_str()
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent)
}

fn contains_word(text: &str, module: &str) -> bool {
    text.match_indices(module).any(|(start, matched)| {
        let end = start + matched.len();
        !text[..start]
            .chars()
            .next_back()
            .is_some_and(is_word_character)
            && !text[end..].chars().next().is_some_and(is_word_character)
    })
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

#[cfg(test)]
mod tests {
    use kaisa_domain::RepoPath;

    use super::{IMPACT_TEXT_LIMIT_BYTES, impact_set};
    use crate::RepositoryIndex;
    use crate::index::tests::{GitProject, SESSION_PATH, session_path};

    const SESSION_SOURCE: &str = "pub fn session() {}\n";
    const AUTH_NEIGHBOR: &str = "src/auth/token.rs";
    const SESSION_IMPORTER: &str = "src/api/login.py";
    const LONGER_NAME_SOURCE: &str = "src/tools/session_tools.rs";
    const UNRELATED_SOURCE: &str = "src/payments/invoice.go";
    const BINARY_IMPORTER: &str = "assets/session.bin";
    const LARGE_IMPORTER: &str = "docs/large.txt";
    const LIMIT_IMPORTER: &str = "docs/at_limit.txt";
    const ONE_BYTE_OVER_LIMIT: usize = 1;

    fn path(name: &str) -> RepoPath {
        RepoPath::try_new(name).expect("the fixture names a file inside the repository")
    }

    fn a_repository_with_session_consumers() -> GitProject {
        GitProject::with_files(&[
            (SESSION_PATH, SESSION_SOURCE),
            (AUTH_NEIGHBOR, "pub fn token() {}\n"),
            (SESSION_IMPORTER, "from auth import session\n"),
            (
                LONGER_NAME_SOURCE,
                "sessions session_cache presession session2 ésession sessioné\n",
            ),
            (UNRELATED_SOURCE, "package payments\n"),
        ])
    }

    fn index_for(repository: &GitProject) -> RepositoryIndex {
        RepositoryIndex::build(repository.root(), "HEAD")
            .expect("the committed project is indexable")
    }

    #[test]
    fn a_change_impacts_the_files_that_import_it() {
        let repository = a_repository_with_session_consumers();
        let index = index_for(&repository);

        let impacted = impact_set(&index, repository.root(), "HEAD", &[session_path()]);

        assert!(
            impacted.contains(&path(SESSION_IMPORTER)),
            "a Python consumer is impacted by the Rust module name without a language-specific parser"
        );
        assert!(
            impacted.contains(&path(AUTH_NEIGHBOR)),
            "files in the changed file's directory are part of its module neighborhood"
        );
        assert!(
            !impacted.contains(&path(UNRELATED_SOURCE)),
            "unrelated directories with no module reference are left out"
        );
    }

    #[test]
    fn a_file_that_only_mentions_the_name_inside_a_longer_word_is_not_impacted() {
        let repository = a_repository_with_session_consumers();
        let index = index_for(&repository);

        let impacted = impact_set(&index, repository.root(), "HEAD", &[session_path()]);

        assert!(
            !impacted.contains(&path(LONGER_NAME_SOURCE)),
            "letters, digits, underscore and Unicode letters keep the module name inside a longer word"
        );
    }

    #[test]
    fn the_impact_set_is_sorted_and_contains_the_change_itself() {
        let repository = a_repository_with_session_consumers();
        let index = index_for(&repository);
        let changes = [session_path(), path(SESSION_IMPORTER), session_path()];
        let mut expected = vec![session_path(), path(AUTH_NEIGHBOR), path(SESSION_IMPORTER)];
        expected.sort();

        let impacted = impact_set(&index, repository.root(), "HEAD", &changes);

        assert_eq!(
            impacted, expected,
            "changed files and neighbors appear exactly once in repository path order"
        );
    }

    #[test]
    fn oversized_and_non_utf8_blobs_are_not_scanned_for_references() {
        let repository = a_repository_with_session_consumers();
        let limit =
            usize::try_from(IMPACT_TEXT_LIMIT_BYTES).expect("the text limit fits in memory");
        let mut oversized = " ".repeat(limit + ONE_BYTE_OVER_LIMIT);
        oversized.replace_range(.."session".len(), "session");
        let at_limit = &oversized[..limit];
        std::fs::create_dir_all(repository.root().join("assets"))
            .expect("the binary asset directory is writable");
        std::fs::create_dir_all(repository.root().join("docs"))
            .expect("the documentation directory is writable");
        std::fs::write(repository.root().join(BINARY_IMPORTER), b"session \xff")
            .expect("the binary blob is writable");
        std::fs::write(repository.root().join(LARGE_IMPORTER), &oversized)
            .expect("the large blob is writable");
        std::fs::write(repository.root().join(LIMIT_IMPORTER), at_limit)
            .expect("the boundary blob is writable");
        repository.commit();
        let index = index_for(&repository);

        let impacted = impact_set(&index, repository.root(), "HEAD", &[session_path()]);

        assert!(
            !impacted.contains(&path(BINARY_IMPORTER)),
            "non-UTF-8 bytes do not become text references"
        );
        assert!(
            !impacted.contains(&path(LARGE_IMPORTER)),
            "a blob beyond the named scan limit is skipped"
        );
        assert!(
            impacted.contains(&path(LIMIT_IMPORTER)),
            "a UTF-8 blob exactly at the limit is still scanned"
        );
    }

    #[test]
    fn a_requested_revision_uses_its_blobs_instead_of_the_worktree_or_stale_index() {
        let repository = a_repository_with_session_consumers();
        let index = index_for(&repository);
        std::fs::write(
            repository.root().join(SESSION_IMPORTER),
            "from auth import token\n",
        )
        .expect("the importer can change");
        repository.commit();
        std::fs::write(
            repository.root().join(SESSION_IMPORTER),
            "from auth import session\n",
        )
        .expect("the worktree can diverge");

        let impacted = impact_set(&index, repository.root(), "HEAD", &[session_path()]);

        assert!(
            !impacted.contains(&path(SESSION_IMPORTER)),
            "only the requested commit defines imports, even with a stale index and dirty worktree"
        );
    }

    #[test]
    fn a_deleted_module_still_impacts_its_importers_and_neighbors() {
        let repository = a_repository_with_session_consumers();
        let index = index_for(&repository);
        std::fs::remove_file(repository.root().join(SESSION_PATH))
            .expect("the module can be deleted");
        repository.commit();

        let impacted = impact_set(&index, repository.root(), "HEAD", &[session_path()]);

        assert!(
            impacted.contains(&session_path()),
            "the changed path remains in the result after deletion"
        );
        assert!(
            impacted.contains(&path(SESSION_IMPORTER)),
            "removing a module affects references to its name"
        );
        assert!(
            impacted.contains(&path(AUTH_NEIGHBOR)),
            "the deleted file's directory still defines its neighborhood"
        );
    }
}
