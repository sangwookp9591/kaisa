//! The candidate a verification looks at: an attempt's preserved commit, frozen (T21a,
//! ADR-0015). Its tree hash is the identity every piece of evidence and every review binds to;
//! git derives it from the content, so a changed candidate can never pass for the reviewed one.

use std::path::Path;

use crate::git::{GitError, git_text, run_git};

/// A frozen candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The commit holding the attempt's work.
    pub commit: String,
    /// The hash of that commit's tree: what was actually reviewed and checked.
    pub tree: String,
}

/// Freezes `revision` of `repository` as a candidate, resolving it to a full commit and tree.
///
/// # Errors
///
/// Returns [`GitError`] if the revision does not name a commit.
pub fn freeze_candidate(repository: &Path, revision: &str) -> Result<Candidate, GitError> {
    let commit: String = git_text(
        repository,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{commit}}"),
        ],
    )?;
    let tree: String = git_text(repository, &["rev-parse", &format!("{commit}^{{tree}}")])?;
    Ok(Candidate { commit, tree })
}

/// File names that decide what a project's checks do. A candidate that changes one could make
/// its own checks pass, so evidence names such changes and delivery needs a person for them.
const CHECK_DEFINITION_FILES: [&str; 17] = [
    "package.json",
    "Cargo.toml",
    "build.rs",
    "pyproject.toml",
    "setup.cfg",
    "setup.py",
    "tox.ini",
    "pytest.ini",
    "conftest.py",
    "noxfile.py",
    "go.mod",
    "Makefile",
    "justfile",
    "config.toml",
    "jest.config.js",
    "vitest.config.ts",
    "tsconfig.json",
];

/// The check-definition files that differ between `base` and `candidate`, in git's order.
///
/// # Errors
///
/// Returns [`GitError`] if git cannot compare the two.
pub fn changed_check_definitions(
    repository: &Path,
    base: &str,
    candidate: &Candidate,
) -> Result<Vec<String>, GitError> {
    let names: String = git_text(
        repository,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            base,
            &candidate.commit,
            "--",
        ],
    )?;
    Ok(names
        .lines()
        .filter(|path| {
            let file: &str = path.rsplit('/').next().unwrap_or(path);
            CHECK_DEFINITION_FILES.contains(&file)
        })
        .map(str::to_owned)
        .collect())
}

/// Deterministic, conservative signals that a candidate weakened its tests.
/// Counts are compared per file and per construct, so adding tests elsewhere cannot hide removal.
/// This is a lexical safety check, not a proof of semantic test equivalence; a person resolves
/// intentional removals and conservative findings (including renames).
///
/// # Errors
/// Returns [`GitError`] if either committed tree cannot be read.
pub fn weakened_tests(
    repository: &Path,
    base: &str,
    candidate: &Candidate,
) -> Result<Vec<String>, GitError> {
    let mut findings = Vec::new();
    for path in changed_paths(repository, base, candidate)? {
        let before = committed_source(repository, base, &path)?;
        let after = committed_source(repository, &candidate.commit, &path)?;
        let file = path.rsplit('/').next().unwrap_or(&path);
        let extension = file.rsplit('.').next().unwrap_or("");
        let test_file = path
            .split('/')
            .any(|part| matches!(part, "test" | "tests" | "__tests__"))
            || matches!(file, "test.rs" | "tests.rs")
            || file.ends_with("_tests.rs")
            || file.starts_with("test_")
            || file.ends_with("_test.py")
            || file.ends_with("_test.go")
            || file.contains(".test.")
            || file.contains(".spec.");
        let supported = matches!(extension, "rs" | "py" | "js" | "jsx" | "ts" | "tsx" | "go");
        if !supported && !test_file {
            continue;
        }
        if before.is_some() && after.is_none() && test_file {
            findings.push(format!("{path}: test file deleted (1 -> 0)"));
        }
        let before = before.unwrap_or_default();
        let after = after.unwrap_or_default();
        for (label, old, new) in test_counts(extension, &before, &after) {
            if new < old {
                findings.push(format!("{path}: {label} decreased ({old} -> {new})"));
            }
        }
        for marker in ["#[ignore", ".skip", ".only", "xfail", "@pytest.mark.skip"] {
            let old = construct_count(&before, marker);
            let new = construct_count(&after, marker);
            if new > old {
                findings.push(format!("{path}: {marker} added ({old} -> {new})"));
            }
        }
    }
    Ok(findings)
}

fn committed_source(
    repository: &Path,
    revision: &str,
    path: &str,
) -> Result<Option<String>, GitError> {
    let entry = run_git(repository, &["ls-tree", "-z", revision, "--", path])?;
    if entry.is_empty() {
        return Ok(None);
    }
    let bytes = run_git(repository, &["show", &format!("{revision}:{path}")])?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

// Preserve identifier boundaries: `split(` must never count as `it(` and `assertion`
// must never count as Python's `assert`. Comments and literals remain conservative signals.
fn tokens(source: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    for character in source.chars() {
        if character.is_alphanumeric() || character == '_' {
            word.push(character);
        } else {
            if !word.is_empty() {
                tokens.push(std::mem::take(&mut word));
            }
            if !character.is_whitespace() {
                tokens.push(character.to_string());
            }
        }
    }
    if !word.is_empty() {
        tokens.push(word);
    }
    tokens
}

fn construct_count(source: &str, marker: &str) -> usize {
    let source = tokens(source);
    match marker {
        "def test_" => source
            .windows(2)
            .filter(|pair| pair[0] == "def" && pair[1].starts_with("test_"))
            .count(),
        "func Test" => source
            .windows(2)
            .filter(|pair| pair[0] == "func" && pair[1].starts_with("Test"))
            .count(),
        _ => {
            let marker = tokens(marker);
            source
                .windows(marker.len())
                .filter(|part| *part == marker)
                .count()
        }
    }
}

fn test_counts(extension: &str, before: &str, after: &str) -> Vec<(&'static str, usize, usize)> {
    let count = construct_count;
    let markers: &[(&str, &str)] = match extension {
        "rs" => &[],
        "py" => &[("Python tests", "def test_"), ("assertions", "assert")],
        "js" | "jsx" | "ts" | "tsx" => &[
            ("test suites", "describe("),
            ("assertions", "expect("),
            ("assert calls", "assert("),
        ],
        "go" => &[("Go tests", "func Test"), ("assertions", "assert.")],
        _ => &[],
    };
    let mut counts: Vec<_> = markers
        .iter()
        .map(|(label, marker)| (*label, count(before, marker), count(after, marker)))
        .collect();
    if matches!(extension, "js" | "jsx" | "ts" | "tsx") {
        let declarations =
            |source: &str| construct_count(source, "it(") + construct_count(source, "test(");
        counts.push(("JS tests", declarations(before), declarations(after)));
    }
    if extension == "rs" {
        let assertions = |source: &str| {
            ["assert!", "assert_eq!", "assert_ne!"]
                .iter()
                .map(|marker| construct_count(source, marker))
                .sum()
        };
        counts.push(("assertions", assertions(before), assertions(after)));
        let rust_tests = |source: &str| {
            construct_count(source, "#[test]") + construct_count(source, "#[tokio::test")
        };
        counts.push(("Rust tests", rust_tests(before), rust_tests(after)));
        counts.push((
            "doctest blocks",
            doctest_blocks(before),
            doctest_blocks(after),
        ));
    }
    counts
}

fn doctest_blocks(source: &str) -> usize {
    // Include line docs, block docs and #[doc = ...] literals. As with the other
    // lexical counters, an example in an ordinary string is a conservative signal.
    source.matches("```").count().div_ceil(2)
}

/// The paths changed between `base` and `candidate`, sorted and deduplicated.
///
/// Includes deletions. Renames are compared as a deletion and an addition, naming both paths.
///
/// # Errors
///
/// Returns [`GitError`] if git cannot compare the two.
pub fn changed_paths(
    repository: &Path,
    base: &str,
    candidate: &Candidate,
) -> Result<Vec<String>, GitError> {
    // Unlike git_text, preserve leading whitespace in the first path as well as NULs.
    let names: Vec<u8> = run_git(
        repository,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            base,
            &candidate.commit,
            "--",
        ],
    )?;
    let mut changed: Vec<String> = String::from_utf8_lossy(&names)
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect();
    changed.sort();
    changed.dedup();
    Ok(changed)
}

/// A clean, detached checkout of a candidate for trusted checks (T21b): nothing the agent left
/// in its own worktree is here, only what the candidate commit holds.
#[derive(Debug)]
pub struct CandidateCheckout {
    repository: std::path::PathBuf,
    path: std::path::PathBuf,
}

impl CandidateCheckout {
    /// Checks `candidate` out, detached, at `path`, which must not exist yet.
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] if git cannot create the checkout.
    pub fn create(repository: &Path, candidate: &Candidate, path: &Path) -> Result<Self, GitError> {
        let path_text: String = path.to_string_lossy().into_owned();
        run_git(
            repository,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                &path_text,
                &candidate.commit,
            ],
        )?;
        Ok(Self {
            repository: repository.to_owned(),
            path: path.to_owned(),
        })
    }

    /// Where the checkout is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for CandidateCheckout {
    fn drop(&mut self) {
        // A check checkout holds nothing to preserve: it is the candidate itself, which stays
        // in its commit. Whatever checks wrote there is scratch.
        let path_text: String = self.path.to_string_lossy().into_owned();
        let _ = run_git(
            &self.repository,
            &["worktree", "remove", "--force", &path_text],
        );
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Candidate, CandidateCheckout, changed_check_definitions, changed_paths, freeze_candidate,
    };
    use crate::test_support::{TemporaryRepository, a_repository_with_one_commit_on_main};

    #[test]
    fn a_candidate_is_the_preserved_commit_and_its_tree() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        repository.commit_file("src/auth/session.rs", "pub fn refresh() {}\n", "wip");

        let candidate: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");

        assert_eq!(
            (candidate.commit.len(), candidate.tree.len()),
            (GIT_OBJECT_ID_HEX, GIT_OBJECT_ID_HEX),
            "a candidate is pinned by full object ids, never a movable name"
        );
        assert_ne!(
            candidate.commit, candidate.tree,
            "commit and tree are distinct objects"
        );
    }

    #[test]
    fn a_changed_candidate_has_a_different_tree() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        repository.commit_file("src/auth/session.rs", "pub fn refresh() {}\n", "reviewed");
        let reviewed: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");
        repository.commit_file(
            "src/auth/session.rs",
            "pub fn refresh() { todo() }\n",
            "after",
        );

        let changed: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");

        assert_ne!(
            reviewed.tree, changed.tree,
            "any change to the files gives a new identity, so old evidence no longer applies"
        );
    }

    #[test]
    fn a_name_that_is_not_a_commit_is_not_a_candidate() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();

        let frozen = freeze_candidate(repository.path(), "kaisa/att_missing");

        assert!(
            frozen.is_err(),
            "verification never runs on something that does not exist"
        );
    }

    #[test]
    fn a_check_checkout_holds_the_candidate_and_nothing_left_in_the_agents_worktree() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        repository.commit_file("src/auth/session.rs", "pub fn refresh() {}\n", "candidate");
        std::fs::write(repository.path().join("uncommitted.txt"), "agent leftovers")
            .expect("writable");
        let candidate: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");
        let place = repository.sibling("check-41a");

        let checkout =
            CandidateCheckout::create(repository.path(), &candidate, &place).expect("checked out");
        let has_candidate_file: bool = checkout.path().join("src/auth/session.rs").exists();
        let has_leftover: bool = checkout.path().join("uncommitted.txt").exists();
        drop(checkout);

        assert!(
            has_candidate_file,
            "the checkout holds the candidate's files"
        );
        assert!(
            !has_leftover,
            "nothing outside the candidate commit reaches the checks"
        );
        assert!(
            !place.exists(),
            "the checkout is discarded once the checks are done"
        );
    }

    #[test]
    fn a_candidate_that_rewrites_its_test_script_is_named_as_changing_check_definitions() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let base: String =
            super::git_text(repository.path(), &["rev-parse", "HEAD"]).expect("the base exists");
        repository.commit_file("package.json", r#"{"scripts":{"test":"exit 0"}}"#, "tamper");
        repository.commit_file("src/auth/session.rs", "pub fn refresh() {}\n", "work");
        let candidate: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");

        let changed: Vec<String> =
            changed_check_definitions(repository.path(), &base, &candidate).expect("compared");

        assert_eq!(
            changed,
            vec!["package.json".to_owned()],
            "a changed test script is named; ordinary source changes are not"
        );
    }

    fn weakening_for(path: &str, before: &str, after: Option<&str>) -> Vec<String> {
        let repository = a_repository_with_one_commit_on_main();
        repository.commit_file(path, before, "add coverage");
        let base = freeze_candidate(repository.path(), "HEAD").expect("base");
        if let Some(after) = after {
            repository.commit_file(path, after, "change coverage");
        } else {
            std::fs::remove_file(repository.path().join(path)).expect("delete fixture test");
            commit_tracked_changes(&repository);
        }
        let candidate = freeze_candidate(repository.path(), "HEAD").expect("candidate");
        super::weakened_tests(repository.path(), &base.commit, &candidate).expect("comparison")
    }

    #[test]
    fn deleting_or_disabling_coverage_in_each_language_is_reported_with_the_file() {
        let changes = [
            (
                "src/lib.rs",
                "#[tokio::test] async fn works() {}",
                "pub fn works() {}",
                "Rust tests",
            ),
            (
                "src/block_docs.rs",
                "/**\n```\nassert!(true);\n```\n*/\npub fn login() {}",
                "pub fn login() {}",
                "doctest blocks",
            ),
            (
                "test_login.py",
                "def test_login(): assert True",
                "def login(): pass",
                "Python tests",
            ),
            (
                "login.test.ts",
                "describe('login', () => { it('works', () => expect(true)); });",
                "const login = () => {};",
                "JS tests",
            ),
            (
                "login_test.go",
                "func TestLogin(t *testing.T) {}",
                "func Login() {}",
                "Go tests",
            ),
            (
                "login.test.js",
                "test('login', () => {});",
                "test.skip('login', () => {});",
                ".skip",
            ),
            (
                "login.test.jsx",
                "it('login', () => {});",
                "it.only('login', () => {});",
                ".only",
            ),
            (
                "test_skip.py",
                "def test_login(): pass",
                "@pytest.mark.skip\ndef test_login(): pass",
                "@pytest.mark.skip",
            ),
            (
                "test_fail.py",
                "def test_login(): pass",
                "@pytest.mark.xfail\ndef test_login(): pass",
                "xfail",
            ),
        ];
        for (path, before, after, marker) in changes {
            let signals = weakening_for(path, before, Some(after));
            assert!(
                signals
                    .iter()
                    .any(|signal| signal.contains(path) && signal.contains(marker)),
                "the per-file signal must identify {marker}: {signals:?}"
            );
        }
    }

    #[test]
    fn changing_test_or_assertion_syntax_without_losing_coverage_is_not_weakening() {
        for (path, before, after) in [
            (
                "src/lib.rs",
                "#[test] fn works() { assert_eq!(1, 1); }",
                "#[tokio::test] async fn works() { assert!(1 == 1); }",
            ),
            (
                "login.test.ts",
                "it('works', () => expect(true));",
                "test('works', () => expect(true));",
            ),
        ] {
            let signals = weakening_for(path, before, Some(after));
            assert!(
                signals.is_empty(),
                "equivalent declaration and assertion counts survive syntax changes: {signals:?}"
            );
        }
    }

    #[test]
    fn deleting_an_empty_test_file_is_reported_as_test_file_deletion() {
        let path = "tests/login.rs";
        let signals = weakening_for(path, "// test fixture\n", None);
        assert!(
            signals
                .iter()
                .any(|signal| signal.contains(path) && signal.contains("test file deleted")),
            "even a test file with no recognized declarations remains coverage: {signals:?}"
        );
    }

    /// The original source path before a rename.
    const ORIGINAL_SOURCE: &str = "src/auth/session.rs";
    /// A changed source path.
    const CHANGED_SOURCE: &str = "src/billing/invoice.rs";
    /// Another changed path, earlier in sorted order.
    const CHANGED_DOCUMENT: &str = "docs/billing.md";
    /// A path whose leading and embedded newlines must survive git's output.
    #[cfg(unix)]
    const NEWLINE_PATH: &str = "\noutside\n.txt";
    /// Identity used only to commit fixture deletions and renames.
    const FIXTURE_IDENTITY: [&str; 4] = [
        "-c",
        "user.name=Kaisa Test",
        "-c",
        "user.email=kaisa-test@example.invalid",
    ];

    fn commit_tracked_changes(repository: &TemporaryRepository) {
        let mut args: Vec<&str> = FIXTURE_IDENTITY.to_vec();
        args.extend(["commit", "--quiet", "-am", "preserve tracked changes"]);
        super::run_git(repository.path(), &args).expect("fixture changes are committed");
    }

    #[test]
    fn a_changed_path_is_listed() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let base: Candidate = freeze_candidate(repository.path(), "HEAD").expect("the base exists");
        repository.commit_file(CHANGED_SOURCE, "new source\n", "change source");
        repository.commit_file(CHANGED_DOCUMENT, "new notes\n", "change document");
        let candidate: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");

        let changed: Vec<String> =
            changed_paths(repository.path(), &base.commit, &candidate).expect("compared");

        assert_eq!(
            changed,
            vec![CHANGED_DOCUMENT.to_owned(), CHANGED_SOURCE.to_owned()],
            "every changed path is named once in sorted order"
        );
    }

    #[test]
    fn an_unchanged_path_is_not_listed() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        repository.commit_file(ORIGINAL_SOURCE, "existing source\n", "add unchanged source");
        let base: Candidate = freeze_candidate(repository.path(), "HEAD").expect("the base exists");
        repository.commit_file(CHANGED_SOURCE, "new source\n", "add changed source");
        let candidate: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");

        let changed: Vec<String> =
            changed_paths(repository.path(), &base.commit, &candidate).expect("compared");

        assert_eq!(
            changed,
            vec![CHANGED_SOURCE.to_owned()],
            "only the changed path is listed; the existing source remains unchanged"
        );
    }

    #[test]
    fn a_deleted_path_is_listed() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        repository.commit_file(CHANGED_SOURCE, "existing source\n", "add source");
        let base: Candidate = freeze_candidate(repository.path(), "HEAD").expect("the base exists");
        std::fs::remove_file(repository.path().join(CHANGED_SOURCE)).expect("fixture file exists");
        commit_tracked_changes(&repository);
        let candidate: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");

        let changed: Vec<String> =
            changed_paths(repository.path(), &base.commit, &candidate).expect("compared");

        assert_eq!(
            changed,
            vec![CHANGED_SOURCE.to_owned()],
            "a deletion still changes the deleted path"
        );
    }

    #[test]
    fn a_rename_lists_both_the_old_and_the_new_path() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        repository.commit_file(ORIGINAL_SOURCE, "existing source\n", "add original source");
        let base: Candidate = freeze_candidate(repository.path(), "HEAD").expect("the base exists");
        let destination = repository.path().join(CHANGED_SOURCE);
        std::fs::create_dir_all(
            destination
                .parent()
                .expect("the destination source has a parent"),
        )
        .expect("fixture is writable");
        super::run_git(repository.path(), &["mv", ORIGINAL_SOURCE, CHANGED_SOURCE])
            .expect("fixture source is renamed");
        commit_tracked_changes(&repository);
        let candidate: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");

        let changed: Vec<String> =
            changed_paths(repository.path(), &base.commit, &candidate).expect("compared");

        assert_eq!(
            changed,
            vec![ORIGINAL_SOURCE.to_owned(), CHANGED_SOURCE.to_owned()],
            "a rename changes both the original and destination paths in sorted order"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_path_with_a_newline_is_reported_exactly() {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let base: Candidate = freeze_candidate(repository.path(), "HEAD").expect("the base exists");
        repository.commit_file(NEWLINE_PATH, "new source\n", "add unusual path");
        let candidate: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");

        let changed: Vec<String> =
            changed_paths(repository.path(), &base.commit, &candidate).expect("compared");

        assert_eq!(
            changed,
            vec![NEWLINE_PATH.to_owned()],
            "NUL-delimited paths preserve leading and embedded newlines exactly"
        );
    }

    /// Hex digits of a SHA-1 git object id (the default object format).
    const GIT_OBJECT_ID_HEX: usize = 40;
}
