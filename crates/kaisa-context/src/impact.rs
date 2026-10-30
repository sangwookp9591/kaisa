//! A deterministic, language-agnostic first pass at the files a change can affect.

use std::collections::BTreeSet;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use kaisa_domain::RepoPath;

use crate::RepositoryIndex;

/// Largest blob scanned for module references: one mebibyte. Larger blobs are not read.
pub const IMPACT_TEXT_LIMIT_BYTES: u64 = 1024 * 1024;

/// Returns changed paths, their directory neighbors and files naming a changed file's stem
/// as a whole word. Word characters are Unicode letters/numbers and underscore; matching is
/// case-sensitive. The result is sorted and contains no duplicates, including for deleted paths.
///
/// A private copy of `index` is refreshed to `revision`, so symbolic revisions and indexes from
/// an earlier revision cannot cause worktree edits or stale blobs to enter the result. Only
/// UTF-8 blobs of at most [`IMPACT_TEXT_LIMIT_BYTES`] are scanned for references; directory
/// neighbors are included regardless of their contents. A missing individual blob is skipped:
/// its absence does not invalidate the readable tree or prevent scanning the other blobs.
///
/// # Errors
///
/// Returns an error if the revision cannot be indexed or the batch reader cannot complete.
/// The revision is checked even when `changed` is empty.
pub fn impact_set(
    index: &RepositoryIndex,
    repository: &Path,
    revision: &str,
    changed: &[RepoPath],
) -> io::Result<Vec<RepoPath>> {
    let mut impacted: BTreeSet<RepoPath> = changed.iter().cloned().collect();
    let mut snapshot = index.clone();
    snapshot.refresh(repository, revision)?;
    if changed.is_empty() {
        return Ok(Vec::new());
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
    let mut blobs = BatchBlobReader::spawn(repository)?;
    for (path, file) in snapshot.files() {
        if directories.contains(directory(path)) {
            impacted.insert(path.clone());
            continue;
        }
        if file.size_bytes > IMPACT_TEXT_LIMIT_BYTES {
            continue;
        }
        let Some(bytes) = blobs.read_blob(&file.digest)? else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if modules.iter().any(|module| contains_word(text, module)) {
            impacted.insert(path.clone());
        }
    }
    blobs.finish()?;
    Ok(impacted.into_iter().collect())
}

#[cfg(test)]
thread_local! {
    static BATCH_PROCESS_SPAWNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Requests one blob at a time so neither pipe can fill while the other side is blocked.
struct BatchBlobReader {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl BatchBlobReader {
    fn spawn(repository: &Path) -> io::Result<Self> {
        let mut child = Command::new("git")
            .current_dir(repository)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        #[cfg(test)]
        BATCH_PROCESS_SPAWNS.with(|count| count.set(count.get() + 1));
        let input = child
            .stdin
            .take()
            .expect("the batch input pipe was requested");
        let output = child
            .stdout
            .take()
            .expect("the batch output pipe was requested");
        Ok(Self {
            child,
            input: Some(input),
            output: BufReader::new(output),
        })
    }

    fn read_blob(&mut self, digest: &str) -> io::Result<Option<Vec<u8>>> {
        let input = self.input.as_mut().expect("the batch reader is still open");
        writeln!(input, "{digest}")?;
        input.flush()?;
        let mut header = String::new();
        self.output.read_line(&mut header)?;
        // Git reports missing objects without a content record. Keep the stream aligned
        // and continue with the other files in this otherwise readable revision.
        if header.trim_end() == format!("{digest} missing") {
            return Ok(None);
        }
        let fields: Vec<_> = header.split_whitespace().collect();
        if fields.len() != 3 || fields[0] != digest || fields[1] != "blob" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid batch blob header",
            ));
        }
        let size: u64 = fields[2]
            .parse()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        // The index excludes large blobs before requesting them. Also bound allocation if
        // Git unexpectedly reports a different size in the batch response.
        let mut bytes = Vec::new();
        let mut content = self.output.by_ref().take(size);
        if size <= IMPACT_TEXT_LIMIT_BYTES {
            content.read_to_end(&mut bytes)?;
        } else {
            io::copy(&mut content, &mut io::sink())?;
        }
        if content.limit() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete batch blob",
            ));
        }
        let mut delimiter = [0];
        self.output.read_exact(&mut delimiter)?;
        if delimiter != *b"\n" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid batch blob delimiter",
            ));
        }
        Ok((size <= IMPACT_TEXT_LIMIT_BYTES).then_some(bytes))
    }

    fn finish(mut self) -> io::Result<()> {
        drop(self.input.take());
        if !self.child.wait()?.success() {
            return Err(io::Error::other("git cat-file --batch failed"));
        }
        Ok(())
    }
}

impl Drop for BatchBlobReader {
    fn drop(&mut self) {
        // Early I/O errors must not leave a child waiting forever for another request.
        drop(self.input.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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

    use super::{BATCH_PROCESS_SPAWNS, BatchBlobReader, IMPACT_TEXT_LIMIT_BYTES, impact_set};
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
    const LARGE_REPOSITORY_CONSUMERS: usize = 256;
    const ONE_BATCH_PROCESS: usize = 1;
    const UNREADABLE_REVISION: &str = "refs/heads/nonexistent-impact-revision";

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

    fn a_large_repository_with_session_consumers() -> GitProject {
        let mut files = vec![(SESSION_PATH.to_owned(), SESSION_SOURCE.to_owned())];
        files.extend((0..LARGE_REPOSITORY_CONSUMERS).map(|number| {
            (
                format!("src/consumers/consumer_{number}.rs"),
                format!("// Consumer {number} imports session\n"),
            )
        }));
        let files: Vec<_> = files
            .iter()
            .map(|(name, text)| (name.as_str(), text.as_str()))
            .collect();
        GitProject::with_files(&files)
    }

    #[test]
    fn the_impact_set_of_a_large_repository_uses_one_git_process() {
        let repository = a_large_repository_with_session_consumers();
        let index = index_for(&repository);
        let expected_paths: Vec<_> = index.files().map(|(path, _)| path.clone()).collect();
        BATCH_PROCESS_SPAWNS.with(|count| count.set(0));

        let impacted = impact_set(&index, repository.root(), "HEAD", &[session_path()])
            .expect("the large revision is readable");

        assert_eq!(
            impacted, expected_paths,
            "every consumer must be scanned through the shared batch reader"
        );
        BATCH_PROCESS_SPAWNS.with(|count| {
            assert_eq!(
                count.get(),
                ONE_BATCH_PROCESS,
                "reading many candidate blobs must spawn exactly one batch process"
            );
        });
    }

    #[test]
    fn an_unreadable_revision_is_an_error_not_a_smaller_impact_set() {
        let repository = a_repository_with_session_consumers();
        let index = index_for(&repository);
        let changes = [session_path()];

        for changed in [changes.as_slice(), &[]] {
            let impacted = impact_set(&index, repository.root(), UNREADABLE_REVISION, changed);

            assert!(
                impacted.is_err(),
                "an unreadable revision must fail even with no changes, rather than claim a partial impact set"
            );
        }
    }

    #[test]
    fn a_missing_blob_is_skipped_without_losing_the_next_readable_blob() {
        let repository = a_repository_with_session_consumers();
        let index = index_for(&repository);
        let session = index.get(&session_path()).expect("the session is indexed");
        let missing_digest = "0".repeat(session.digest.len());
        let mut blobs = BatchBlobReader::spawn(repository.root())
            .expect("the readable repository supports batch requests");

        let missing = blobs
            .read_blob(&missing_digest)
            .expect("Git reports a missing object");
        let readable = blobs
            .read_blob(&session.digest)
            .expect("the session remains readable");
        blobs
            .finish()
            .expect("a missing object does not fail the batch process");

        assert!(
            missing.is_none(),
            "an individual missing blob cannot provide text references"
        );
        assert_eq!(
            readable.as_deref(),
            Some(SESSION_SOURCE.as_bytes()),
            "the next request must read its own bytes after a missing-object response"
        );
    }

    #[test]
    fn a_change_impacts_the_files_that_import_it() {
        let repository = a_repository_with_session_consumers();
        let index = index_for(&repository);

        let impacted = impact_set(&index, repository.root(), "HEAD", &[session_path()])
            .expect("the requested revision and its blobs are readable");

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

        let impacted = impact_set(&index, repository.root(), "HEAD", &[session_path()])
            .expect("the requested revision and its blobs are readable");

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

        let impacted = impact_set(&index, repository.root(), "HEAD", &changes)
            .expect("the requested revision and its blobs are readable");

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

        let impacted = impact_set(&index, repository.root(), "HEAD", &[session_path()])
            .expect("the requested revision and its blobs are readable");

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

        let impacted = impact_set(&index, repository.root(), "HEAD", &[session_path()])
            .expect("the requested revision and its blobs are readable");

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

        let impacted = impact_set(&index, repository.root(), "HEAD", &[session_path()])
            .expect("the requested revision and its blobs are readable");

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
