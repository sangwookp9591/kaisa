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

/// CI configuration file names, matched anywhere in the tree: a pipeline decides what counts as
/// a passing build as much as a test script does.
const CI_CONFIG_FILES: [&str; 5] = [
    ".gitlab-ci.yml",
    "azure-pipelines.yml",
    "bitbucket-pipelines.yml",
    ".travis.yml",
    "Jenkinsfile",
];

/// Whether `path` (repository-relative, `/`-separated) defines what the project's checks run:
/// a build or test configuration file by name, or a CI pipeline by name or directory.
fn is_check_definition(path: &str) -> bool {
    let file: &str = path.rsplit('/').next().unwrap_or(path);
    if CHECK_DEFINITION_FILES.contains(&file) {
        return true;
    }
    let parts: Vec<&str> = path.split('/').collect();
    let directories = &parts[..parts.len() - 1];
    // `config.yml` only names a pipeline inside `.circleci/`; elsewhere it is ordinary data.
    CI_CONFIG_FILES.contains(&file)
        || (file == "config.yml" && directories.last() == Some(&".circleci"))
        || directories
            .windows(2)
            .any(|pair| pair == [".github", "workflows"])
        || directories.contains(&".buildkite")
}

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
    // NUL-delimited: without -z Git quotes non-ASCII and control-character paths
    // (`".github/workflows/\352..."`), which would no longer match a directory name.
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
    Ok(split_nul_paths(&names)
        .into_iter()
        .filter(|path| is_check_definition(path))
        .collect())
}

/// Splits Git's `-z` output into exact paths, in Git's order.
fn split_nul_paths(output: &[u8]) -> Vec<String> {
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
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
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type ComparisonCache = Mutex<HashMap<(String, String), Vec<String>>>;
    static CACHE: OnceLock<ComparisonCache> = OnceLock::new();
    let base_tree = git_text(repository, &["rev-parse", &format!("{base}^{{tree}}")])?;
    let key = (base_tree.clone(), candidate.tree.clone());
    // Hold the lock through comparison: parallel checks of the same immutable trees must
    // not repeat the scan. Tree identities also make results independent of repository paths.
    let mut cache = CACHE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(findings) = cache.get(&key) {
        return Ok(findings.clone());
    }
    let paths: Vec<_> = changed_paths(repository, &base_tree, candidate)?
        .into_iter()
        .filter(|path| supported_source(path) || is_test_file(path) || is_test_script_file(path))
        .collect();
    let sources = committed_sources(repository, &base_tree, &candidate.tree, &paths)?;
    let mut findings = Vec::new();
    for (path, pair) in paths.iter().zip(sources.as_chunks::<2>().0) {
        if is_test_script_file(path) {
            compare_test_script(
                path,
                pair[0].as_deref().unwrap_or_default(),
                pair[1].as_deref().unwrap_or_default(),
                &mut findings,
            );
            continue;
        }
        if pair[0].is_some() && pair[1].is_none() && is_test_file(path) {
            findings.push(format!("{path}: test file deleted (1 -> 0)"));
        }
        compare_test_source(
            path,
            pair[0].as_deref().unwrap_or_default(),
            pair[1].as_deref().unwrap_or_default(),
            &mut findings,
        );
    }
    cache.insert(key, findings.clone());
    Ok(findings)
}

/// Files whose test commands are scanned for a pass-regardless signal.
fn is_test_script_file(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    matches!(file, "package.json" | "Makefile" | "justfile" | "tox.ini")
}

/// Suffixes that make a test command succeed whatever its tests did, or whatever it found.
const PASS_REGARDLESS: [&str; 6] = [
    "--passWithNoTests",
    "--pass-with-no-tests",
    "--exit-zero",
    "|| true",
    "|| exit 0",
    "||true",
];

/// Occurrences of `signal` in the commands of `script` that run tests. Entries are lines and,
/// within a minified JSON object, quote-comma separated members. A line also counts when it is
/// an indented recipe line under a test target or section (Makefile `test:`, justfile `test:`,
/// tox `[testenv]` `commands`), so `test:\n\tjest || true` is recognised while an unrelated
/// command that ends in `|| true` elsewhere is not mistaken for a weakened test.
fn pass_regardless_count(script: &str, signal: &str) -> usize {
    let script = script.to_ascii_lowercase();
    let signal = signal.to_ascii_lowercase();
    // `section` is the latest `[ini section]` or `target:` header; `commands` is true after a
    // tox `commands =` key, whose continuation lines are indented.
    let (mut section_tests, mut target_tests) = (false, false);
    let mut total = 0;
    for line in script.lines() {
        let trimmed = line.trim();
        let indented = line.starts_with(['\t', ' ']);
        if !indented && !trimmed.is_empty() {
            let names_tests = |text: &str| text.contains("test") || text.contains("mocha");
            if trimmed.starts_with('[') {
                // `[testenv]` is tox's default environment, which runs the tests; in
                // `[testenv:lint]` only the part after the colon says what it runs.
                let bare_testenv = trimmed == "[testenv]";
                let named = trimmed.strip_prefix("[testenv:").unwrap_or(trimmed);
                section_tests = bare_testenv || names_tests(named);
                target_tests = false;
            } else if trimmed.starts_with("commands") {
                target_tests = section_tests;
            } else {
                // `test:` / `test-all: deps` (Makefile, justfile): the name before the colon.
                let name = trimmed.split(':').next().unwrap_or_default();
                target_tests = trimmed.contains(':') && names_tests(name);
            }
        }
        for entry in line.split("\",") {
            if target_tests || entry.contains("test") || entry.contains("mocha") {
                total += entry.matches(&signal).count();
            }
        }
    }
    total
}

/// Reports each pass-regardless signal that `after` has more of than `before`, so that a
/// candidate which lets an empty or failing test run succeed is named rather than reported
/// only as a generic changed file.
fn compare_test_script(path: &str, before: &str, after: &str, findings: &mut Vec<String>) {
    for signal in PASS_REGARDLESS {
        let (old, new) = (
            pass_regardless_count(before, signal),
            pass_regardless_count(after, signal),
        );
        if new > old {
            findings.push(format!(
                "{path}: `{signal}` added to a test command ({old} -> {new}); \
                 the tests can no longer fail the build"
            ));
        }
    }
}

fn is_test_file(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    path.split('/')
        .any(|part| matches!(part, "test" | "tests" | "__tests__"))
        || matches!(file, "test.rs" | "tests.rs")
        || file.ends_with("_tests.rs")
        || file.ends_with("_test.rs")
        || file.starts_with("test_")
        || file.ends_with("_test.py")
        || file.ends_with("_test.go")
        || file.contains(".test.")
        || file.contains(".spec.")
}

fn supported_source(path: &str) -> bool {
    matches!(
        path.rsplit('.').next(),
        Some("rs" | "py" | "js" | "jsx" | "ts" | "tsx" | "go")
    )
}

fn committed_sources(
    repository: &Path,
    base: &str,
    candidate: &str,
    paths: &[String],
) -> Result<Vec<Option<String>>, GitError> {
    use std::io::{BufRead, Read, Write};
    use std::process::{Command, Stdio};
    // -Z keeps even newline-bearing paths unambiguous; all blobs use one Git process.
    let mut child = Command::new("git")
        .args(["cat-file", "--batch", "-Z"])
        .current_dir(repository)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(GitError::Spawn)?;
    let mut input = child.stdin.take().expect("piped input");
    let requests: Vec<_> = paths
        .iter()
        .flat_map(|path| [format!("{base}:{path}\0"), format!("{candidate}:{path}\0")])
        .collect();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for request in requests {
            input.write_all(request.as_bytes())?;
        }
        Ok(())
    });
    let mut output = std::io::BufReader::new(child.stdout.take().expect("piped output"));
    let mut sources = Vec::new();
    for _ in 0..paths.len() * 2 {
        let mut header = Vec::new();
        output.read_until(0, &mut header).map_err(GitError::Spawn)?;
        let header = String::from_utf8_lossy(&header);
        if header.trim_end_matches('\0').ends_with(" missing") {
            sources.push(None);
            continue;
        }
        let size = header
            .trim_end_matches('\0')
            .rsplit(' ')
            .next()
            .and_then(|size| size.parse::<usize>().ok())
            .ok_or_else(|| GitError::Failed {
                args: vec!["cat-file".into()],
                stderr: format!("invalid batch header: {header}"),
            })?;
        let mut bytes = vec![0; size];
        output.read_exact(&mut bytes).map_err(GitError::Spawn)?;
        let mut terminator = [0];
        output
            .read_exact(&mut terminator)
            .map_err(GitError::Spawn)?;
        sources.push(Some(String::from_utf8_lossy(&bytes).into_owned()));
    }
    writer
        .join()
        .expect("batch writer completed")
        .map_err(GitError::Spawn)?;
    if !child.wait().map_err(GitError::Spawn)?.success() {
        return Err(GitError::Failed {
            args: vec!["cat-file".into()],
            stderr: "batch read failed".into(),
        });
    }
    Ok(sources)
}

// Replace comments/literals with whitespace before tokenization, preserving line boundaries.
// Rust block comments nest; Python triple quotes are docstrings; Go raw strings use backticks.
fn executable_source(source: &str, extension: &str) -> String {
    let b = source.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        if (extension == "py" && b[i] == b'#') || (extension != "py" && b[i..].starts_with(b"//")) {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if extension != "py" && b[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < b.len() && depth > 0 {
                if b[i..].starts_with(b"/*") && extension == "rs" {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if extension == "rs" && (b[i] == b'r' || b[i..].starts_with(b"br")) {
            let mut quote = i + if b[i] == b'b' { 2 } else { 1 };
            while quote < b.len() && b[quote] == b'#' {
                quote += 1;
            }
            if quote < b.len() && b[quote] == b'"' {
                let hashes = &b[(i + if b[i] == b'b' { 2 } else { 1 })..quote];
                i = quote + 1;
                while i < b.len() && !(b[i] == b'"' && b[i + 1..].starts_with(hashes)) {
                    i += 1;
                }
                i = (i + 1 + hashes.len()).min(b.len());
            } else {
                i += 1;
                continue;
            }
        } else if b[i] == b'"'
            || b[i] == b'`'
            || (b[i] == b'\''
                && (extension != "rs"
                    || (i + 2 < b.len() && (b[i + 2] == b'\'' || b[i + 1] == b'\\'))))
        {
            let quote = b[i];
            let triple = extension == "py" && b[i..].starts_with(&[quote; 3]);
            let width = if triple { 3 } else { 1 };
            i += width;
            while i < b.len() {
                if b[i..].starts_with(&vec![quote; width]) {
                    i += width;
                    break;
                }
                if b[i] == b'\\' && quote != b'`' {
                    i = (i + 2).min(b.len());
                } else {
                    i += 1;
                }
            }
        } else {
            i += 1;
            continue;
        }
        for c in &mut out[start..i] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    }
    String::from_utf8(out).expect("replacing complete UTF-8 regions preserves UTF-8")
}

// Preserve identifier boundaries: `split(` must never count as `it(` and `assertion`
// must never count as Python's `assert`. Callers strip comments and literals first.
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

fn construct_count(source: &[String], marker: &str) -> usize {
    match marker {
        "def test_" => source
            .windows(2)
            .filter(|p| p[0] == "def" && p[1].starts_with("test_"))
            .count(),
        "func Test" => source
            .windows(2)
            .filter(|p| p[0] == "func" && p[1].starts_with("Test"))
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

fn declarations(t: &[String], extension: &str) -> usize {
    let markers: &[&str] = match extension {
        "rs" => &["#[test]", "#[tokio::test"],
        "py" => &["def test_"],
        "go" => &["func Test"],
        "js" | "jsx" | "ts" | "tsx" => &["it(", "test(", "describe("],
        _ => &[],
    };
    markers.iter().map(|m| construct_count(t, m)).sum()
}

fn assertions(t: &[String], extension: &str) -> usize {
    let markers: &[&str] = match extension {
        "rs" => &["assert!", "assert_eq!", "assert_ne!"],
        "py" => &["assert", "self.assertEqual(", "self.assertTrue("],
        "go" => &["assert."],
        _ => &["expect(", "assert("],
    };
    markers.iter().map(|m| construct_count(t, m)).sum()
}

fn rust_test_scope(t: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut i = 0;
    let markers = [
        tokens("#[test]"),
        tokens("#[tokio::test"),
        tokens("#[cfg(test)]"),
    ];
    while i < t.len() {
        if markers.iter().any(|marker| t[i..].starts_with(marker)) {
            let start = i;
            while i < t.len() && t[i] != "{" {
                i += 1;
            }
            let mut depth = 0;
            while i < t.len() {
                if t[i] == "{" {
                    depth += 1;
                }
                if t[i] == "}" {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
            result.extend_from_slice(&t[start..i]);
        } else {
            i += 1;
        }
    }
    result
}

fn test_scope(source: &str, t: &[String], extension: &str, test_file: bool) -> Vec<String> {
    if test_file {
        return t.to_vec();
    }
    if extension == "rs" {
        return rust_test_scope(t);
    }
    if extension == "py" {
        let mut selected = String::new();
        let mut indent = None;
        for line in source.lines() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() {
                continue;
            }
            let current = line.len() - trimmed.len();
            if trimmed.starts_with("def test_") || trimmed.starts_with("async def test_") {
                indent = Some(current);
            } else if indent.is_some_and(|start| current <= start) {
                indent = None;
            }
            if indent.is_some() {
                selected.push_str(line);
                selected.push('\n');
            }
        }
        return tokens(&selected);
    }
    let mut selected = Vec::new();
    let mut i = 0;
    while i < t.len() {
        let starts_test = if extension == "go" {
            t[i] == "func" && t.get(i + 1).is_some_and(|name| name.starts_with("Test"))
        } else {
            matches!(t[i].as_str(), "it" | "test" | "describe")
                && t.get(i + 1).is_some_and(|next| next == "(")
        };
        if !starts_test {
            i += 1;
            continue;
        }
        let start = i;
        while i < t.len() && t[i] != "{" {
            i += 1;
        }
        let mut depth = 0;
        while i < t.len() {
            if t[i] == "{" {
                depth += 1;
            }
            if t[i] == "}" {
                depth -= 1;
                if depth == 0 {
                    i += 1;
                    break;
                }
            }
            i += 1;
        }
        selected.extend_from_slice(&t[start..i]);
    }
    selected
}

fn empty_rust_tests(t: &[String]) -> usize {
    let markers = [tokens("#[test]"), tokens("#[tokio::test")];
    (0..t.len())
        .filter(|&index| {
            if !markers.iter().any(|marker| t[index..].starts_with(marker)) {
                return false;
            }
            let Some(body) = t[index..].iter().position(|token| token == "{") else {
                return false;
            };
            t.get(index + body + 1).is_some_and(|token| token == "}")
        })
        .count()
}

fn tautologies(t: &[String]) -> usize {
    let mut count = 0;
    for i in 0..t.len() {
        if t[i] == "assert" && t.get(i + 1).is_some_and(|s| s == "True") {
            count += 1;
        }
        if !matches!(t[i].as_str(), "assert" | "assert_eq")
            || t.get(i + 1).is_none_or(|s| s != "!")
            || t.get(i + 2).is_none_or(|s| s != "(")
        {
            continue;
        }
        let args = &t[i + 3..];
        let mut depth = 0;
        let end = args
            .iter()
            .position(|s| {
                if s == ")" && depth == 0 {
                    return true;
                }
                if matches!(s.as_str(), "(" | "[" | "{") {
                    depth += 1;
                }
                if matches!(s.as_str(), ")" | "]" | "}") {
                    depth -= 1;
                }
                false
            })
            .unwrap_or(args.len());
        let args = &args[..end];
        if args.first().is_some_and(|arg| arg == "true")
            && (args.len() == 1 || args.get(1).is_some_and(|arg| arg == ","))
        {
            count += 1;
            continue;
        }
        if t[i] == "assert_eq" {
            if let Some(comma) = args.iter().position(|s| s == ",") {
                let rhs = &args[comma + 1..];
                let end = rhs.iter().position(|s| s == ",").unwrap_or(rhs.len());
                if comma > 0 && args[..comma] == rhs[..end] {
                    count += 1;
                }
            }
        } else if let Some(eq) = args.windows(2).position(|p| p == ["=", "="]) {
            let rhs = &args[eq + 2..];
            let end = rhs.iter().position(|s| s == ",").unwrap_or(rhs.len());
            if eq > 0 && args[..eq] == rhs[..end] {
                count += 1;
            }
        }
    }
    count
}

fn doctest_counts(source: &str) -> (usize, usize) {
    // Documentation is intentionally read before stripping comments/literals. Only doc
    // comments/attributes carry executable examples; ordinary string examples do not.
    let mut docs = String::new();
    let mut block = false;
    for line in source.lines() {
        let line = line.trim_start();
        if line.starts_with("/**") || line.starts_with("/*!") {
            block = true;
        }
        if block || line.starts_with("///") || line.starts_with("//!") || line.starts_with("#[doc")
        {
            docs.push_str(line);
            docs.push('\n');
        }
        if block && line.contains("*/") {
            block = false;
        }
    }
    let mut blocks = 0;
    let mut disabled = 0;
    for (index, part) in docs.split("```").skip(1).enumerate() {
        if index % 2 == 0 {
            blocks += 1;
            let info = part.lines().next().unwrap_or_default();
            if info
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|word| {
                    matches!(
                        word,
                        "ignore" | "no_run" | "text" | "compile_fail" | "should_panic"
                    )
                })
            {
                disabled += 1;
            }
        }
    }
    (blocks, disabled)
}

fn compare_test_source(path: &str, before: &str, after: &str, findings: &mut Vec<String>) {
    let extension = path.rsplit('.').next().unwrap_or_default();
    // Rust tests require attributes and doctests require documentation. Plain production
    // files cannot contribute test assertions, so avoid allocating tokens for large generated
    // sources with neither. Check both versions so removing the last attribute is still seen.
    if extension == "rs"
        && !is_test_file(path)
        && ![before, after].iter().any(|source| {
            source.contains('#')
                || source.contains("///")
                || source.contains("//!")
                || source.contains("/**")
                || source.contains("/*!")
        })
    {
        return;
    }
    let old_source = executable_source(before, extension);
    let new_source = executable_source(after, extension);
    let old = tokens(&old_source);
    let new = tokens(&new_source);
    let old_scope = test_scope(&old_source, &old, extension, is_test_file(path));
    let new_scope = test_scope(&new_source, &new, extension, is_test_file(path));
    let test_context = is_test_file(path) || !old_scope.is_empty() || !new_scope.is_empty();
    let old_assert = if test_context {
        assertions(&old_scope, extension)
    } else {
        0
    };
    let new_assert = if test_context {
        assertions(&new_scope, extension)
    } else {
        0
    };
    let old_tests = declarations(&old, extension);
    let new_tests = declarations(&new, extension);
    let mut signal = |label: &str, a: usize, b: usize| {
        findings.push(format!("{path}: {label} ({a} -> {b})"));
    };
    // Declaration OR assertion loss alone is ambiguous (merged tests or extracted helpers).
    // Require both, except explicit deletion/disable/comment/tautology signals. A newly
    // introduced loop with retained tests is a common table-driven merge (R6 F1); accepting
    // it is lexical evidence, not proof that every old example remains covered.
    let table_merge =
        new_tests > 0 && construct_count(&new_scope, "for") > construct_count(&old_scope, "for");
    if new_tests < old_tests && new_assert < old_assert && !table_merge {
        signal("tests and assertions decreased", old_tests, new_tests);
    }
    // Commenting a test out removes executable declarations but leaves their text behind.
    if new_tests < old_tests && declarations(&tokens(after), extension) >= old_tests {
        signal("tests commented out", old_tests, new_tests);
    }
    if extension == "rs" {
        let old_cfg = construct_count(&old, "#[cfg(test)]");
        let new_cfg = construct_count(&new, "#[cfg(test)]");
        if new_cfg < old_cfg {
            signal("#[cfg(test)] decreased", old_cfg, new_cfg);
        }
        for marker in [
            "#[cfg(any())]",
            "#[cfg(feature",
            "#[cfg(FALSE)]",
            "#[cfg(not(test))]",
        ] {
            let a = construct_count(&old, marker);
            let b = construct_count(&new, marker);
            if b > a && old_tests > 0 {
                signal(marker, a, b);
            }
        }
        let cfg_ignore = |t: &[String]| {
            t.split(|s| s == "]")
                .filter(|attr| {
                    construct_count(attr, "#[cfg_attr(") > 0 && attr.iter().any(|s| s == "ignore")
                })
                .count()
        };
        let a = cfg_ignore(&old);
        let b = cfg_ignore(&new);
        if b > a {
            signal("cfg_attr ignore added", a, b);
        }
        let (a, disabled_a) = doctest_counts(before);
        let (b, disabled_b) = doctest_counts(after);
        if b < a {
            signal("doctest blocks decreased", a, b);
        }
        if disabled_b > disabled_a {
            signal("disabled doctest added", disabled_a, disabled_b);
        }
    }
    for marker in [
        "#[ignore",
        ".skip",
        ".skipif",
        ".only",
        "xfail",
        "@pytest.mark.skip",
        "pytest.skip(",
        "unittest.skip",
        "xit(",
        "xdescribe(",
        "it.todo(",
        "test.todo(",
        "t.Skip(",
    ] {
        let a = construct_count(&old, marker);
        let b = construct_count(&new, marker);
        if b > a {
            signal(&format!("{marker} added"), a, b);
        }
    }
    let a = tautologies(&old_scope);
    let b = tautologies(&new_scope);
    if b > a && test_context {
        signal("tautological assertions added", a, b);
    }
    if extension == "rs" {
        let a = empty_rust_tests(&old);
        let b = empty_rust_tests(&new);
        if b > a && old_tests > 0 {
            signal("empty test bodies added", a, b);
        }
    }
    // Early return is a useful explicit bypass signal, without pretending to analyze control flow.
    let a = construct_count(&old_scope, "{ return ;");
    let b = construct_count(&new_scope, "{ return ;");
    if b > a && old_tests > 0 {
        signal("test early return added", a, b);
    }
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

    fn check_definitions_changed_by(path: &str) -> Vec<String> {
        let repository: TemporaryRepository = a_repository_with_one_commit_on_main();
        let base: String =
            super::git_text(repository.path(), &["rev-parse", "HEAD"]).expect("the base exists");
        repository.commit_file(path, "changed: true\n", "edit");
        let candidate: Candidate = freeze_candidate(repository.path(), "HEAD").expect("frozen");
        changed_check_definitions(repository.path(), &base, &candidate).expect("compared")
    }

    #[test]
    fn a_candidate_that_only_edits_ci_configuration_is_named_as_changing_check_definitions() {
        for path in [
            ".github/workflows/ci.yml",
            ".github/workflows/nested/release.yaml",
            ".gitlab-ci.yml",
            ".circleci/config.yml",
            "azure-pipelines.yml",
            "Jenkinsfile",
            "services/api/Jenkinsfile",
            ".buildkite/pipeline.yml",
            ".buildkite/steps/test.sh",
            "bitbucket-pipelines.yml",
            ".travis.yml",
        ] {
            assert_eq!(
                check_definitions_changed_by(path),
                vec![path.to_owned()],
                "{path} decides what a passing build is"
            );
        }
    }

    #[test]
    fn files_that_only_look_like_ci_configuration_are_not_check_definitions() {
        for path in [
            "docs/workflows/ci.yml",
            ".github/ISSUE_TEMPLATE/bug.yml",
            "config.yml",
            "app/config.yml",
            "notes/Jenkinsfile.md",
        ] {
            assert!(
                check_definitions_changed_by(path).is_empty(),
                "{path} does not define the project's checks"
            );
        }
    }

    #[test]
    fn a_ci_file_whose_name_git_would_quote_is_still_a_check_definition() {
        for path in [
            ".github/workflows/검사.yml",
            ".github/workflows/caf\u{e9}.yml",
            ".github/workflows/tab\there.yml",
            ".buildkite/\"quoted\".yml",
        ] {
            #[cfg(not(unix))]
            if path.contains(['\t', '"']) {
                continue;
            }
            assert_eq!(
                check_definitions_changed_by(path),
                vec![path.to_owned()],
                "{path:?} is a CI definition under Git's default path quoting"
            );
        }
    }

    #[test]
    fn nul_delimited_paths_keep_their_exact_bytes() {
        assert_eq!(
            super::split_nul_paths("a b.yml\0 lead\nnl.yml\0검사.yml\0".as_bytes()),
            vec!["a b.yml", " lead\nnl.yml", "검사.yml"],
            "no trimming, quoting or newline splitting"
        );
    }

    #[test]
    fn a_swallowed_failure_in_a_multiline_test_recipe_is_a_weakened_signal() {
        for (path, before, after) in [
            ("Makefile", "test:\n\tjest\n", "test:\n\tjest || true\n"),
            ("justfile", "test:\n    jest\n", "test:\n    jest || true\n"),
            (
                "tox.ini",
                "[testenv]\ncommands =\n    pytest\n",
                "[testenv]\ncommands =\n    pytest || true\n",
            ),
            (
                "Makefile",
                "check: build\n\tcargo x\n",
                "check: build\n\tcargo x\n.PHONY: all\ntest-unit:\n\tgo run ./... --exit-zero\n",
            ),
        ] {
            let findings = weakening_for(path, before, Some(after));
            assert!(
                findings
                    .iter()
                    .any(|finding| finding.starts_with(&format!("{path}:"))),
                "{path} recipe now swallows test failures: {findings:?}"
            );
        }
    }

    #[test]
    fn a_swallowed_failure_in_a_non_test_recipe_is_not_a_weakened_signal() {
        for (path, before, after) in [
            (
                "Makefile",
                "clean:\n\trm -rf dist\n",
                "clean:\n\trm -rf dist || true\n",
            ),
            (
                "tox.ini",
                "[testenv:lint]\ncommands =\n    flake8\n",
                "[testenv:lint]\ncommands =\n    flake8 || true\n",
            ),
        ] {
            assert!(
                weakening_for(path, before, Some(after)).is_empty(),
                "{path}: only recipes that run tests can weaken them"
            );
        }
    }

    fn script_weakening(before: &str, after: &str) -> Vec<String> {
        weakening_for("package.json", before, Some(after))
    }

    #[test]
    fn adding_pass_with_no_tests_or_a_swallowed_exit_to_a_test_script_is_a_weakened_signal() {
        let before = r#"{"scripts":{"test":"jest"}}"#;
        for (after, signal) in [
            (
                r#"{"scripts":{"test":"jest --passWithNoTests"}}"#,
                "--passWithNoTests",
            ),
            (
                r#"{"scripts":{"test":"vitest run --pass-with-no-tests"}}"#,
                "--pass-with-no-tests",
            ),
            (r#"{"scripts":{"test":"jest || true"}}"#, "|| true"),
            (
                r#"{"scripts":{"test":"pytest --exit-zero"}}"#,
                "--exit-zero",
            ),
        ] {
            let findings = script_weakening(before, after);
            assert!(
                findings
                    .iter()
                    .any(|finding| finding.starts_with("package.json:") && finding.contains(signal)),
                "{after} makes the tests unable to fail the build: {findings:?}"
            );
        }
    }

    #[test]
    fn a_test_script_that_keeps_failing_builds_or_swallows_an_unrelated_command_is_not_weakening() {
        assert!(
            script_weakening(
                r#"{"scripts":{"test":"jest","clean":"rm -rf dist"}}"#,
                r#"{"scripts":{"test":"jest --coverage","clean":"rm -rf dist || true"}}"#,
            )
            .is_empty(),
            "only commands that run tests can weaken them"
        );
        assert!(
            script_weakening(
                r#"{"scripts":{"test":"jest --passWithNoTests"}}"#,
                r#"{"scripts":{"test":"jest --passWithNoTests","lint":"eslint ."}}"#,
            )
            .is_empty(),
            "a signal that was already there is not new"
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
                "#[tokio::test] async fn works() { assert!(ready()); }",
                "pub fn works() {}",
                "tests and assertions",
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
                "tests and assertions",
            ),
            (
                "login.test.ts",
                "describe('login', () => { it('works', () => expect(true)); });",
                "const login = () => {};",
                "tests and assertions",
            ),
            (
                "login_test.go",
                "func TestLogin(t *testing.T) { assert.Equal(t, got, want) }",
                "func Login() {}",
                "tests and assertions",
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

    struct CoverageChange {
        path: &'static str,
        before: &'static str,
        after: &'static str,
    }

    fn source_change(before: &'static str, after: &'static str) -> CoverageChange {
        CoverageChange {
            path: "src/lib.rs",
            before,
            after,
        }
    }

    const ACTIVE_TEST: &str = "#[cfg(test)] mod tests { #[test] fn checks_value() { assert_eq!(actual(), expected()); } }";

    #[test]
    fn commented_disabled_tautological_and_early_return_tests_are_reported() {
        let changes = [
            source_change(
                ACTIVE_TEST,
                "// #[cfg(test)] mod tests { #[test] fn checks_value() { assert_eq!(actual(), expected()); } }",
            ),
            source_change(
                ACTIVE_TEST,
                "/* outer /* nested */ #[cfg(test)] mod tests { #[test] fn checks_value() { assert_eq!(actual(), expected()); } } */",
            ),
            source_change(
                ACTIVE_TEST,
                "#[cfg(any())] mod tests { #[test] fn checks_value() { assert_eq!(actual(), expected()); } }",
            ),
            source_change(
                ACTIVE_TEST,
                "#[cfg(feature = \"missing\")] mod tests { #[test] fn checks_value() { assert_eq!(actual(), expected()); } }",
            ),
            source_change(
                ACTIVE_TEST,
                "#[cfg(FALSE)] mod tests { #[test] fn checks_value() { assert_eq!(actual(), expected()); } }",
            ),
            source_change(
                ACTIVE_TEST,
                "#[cfg(not(test))] mod tests { #[test] fn checks_value() { assert_eq!(actual(), expected()); } }",
            ),
            source_change(
                ACTIVE_TEST,
                "#[cfg(test)] mod tests { #[test] #[cfg_attr(test, ignore)] fn checks_value() { assert_eq!(actual(), expected()); } }",
            ),
            source_change(
                ACTIVE_TEST,
                "#[cfg(test)] mod tests { #[test] fn checks_value() { assert!(true, \"context\"); } }",
            ),
            source_change(
                ACTIVE_TEST,
                "#[cfg(test)] mod tests { #[test] fn checks_value() { assert!(1 == 1); } }",
            ),
            source_change(
                ACTIVE_TEST,
                "#[cfg(test)] mod tests { #[test] fn checks_value() { assert_eq!(value, value); } }",
            ),
            source_change(
                ACTIVE_TEST,
                "#[cfg(test)] mod tests { #[test] fn checks_value() { return; assert_eq!(actual(), expected()); } }",
            ),
            CoverageChange {
                path: "test_login.py",
                before: "def test_login(): assert login()",
                after: "def test_login(): assert True",
            },
            CoverageChange {
                path: "test_login.py",
                before: "def test_login(): assert login()",
                after: "def test_login(): pytest.skip('reason'); assert login()",
            },
            CoverageChange {
                path: "test_login.py",
                before: "def test_login(): assert login()",
                after: "@pytest.mark.skipif(condition)\ndef test_login(): assert login()",
            },
            CoverageChange {
                path: "test_login.py",
                before: "def test_login(): assert login()",
                after: "@unittest.skip('reason')\ndef test_login(): assert login()",
            },
            CoverageChange {
                path: "login.test.ts",
                before: "it('login', () => expect(login()));",
                after: "xit('login', () => expect(login()));",
            },
            CoverageChange {
                path: "login.test.ts",
                before: "it('login', () => expect(login()));",
                after: "it.todo('login', () => expect(login()));",
            },
            CoverageChange {
                path: "login_test.go",
                before: "func TestLogin(t *testing.T) {}",
                after: "func TestLogin(t *testing.T) { t.Skip(\"reason\"); }",
            },
        ];
        for change in changes {
            let signals = weakening_for(change.path, change.before, Some(change.after));
            assert!(
                !signals.is_empty(),
                "explicit weakening must be visible for {}: {}",
                change.path,
                change.after
            );
        }
        for info in ["ignore", "no_run", "text", "compile_fail", "should_panic"] {
            let before = "/// ```\n/// assert_eq!(actual(), expected());\n/// ```";
            let after = before.replacen("```", &format!("```{info}"), 1);
            let signals = weakening_for("src/lib.rs", before, Some(&after));
            assert!(
                signals
                    .iter()
                    .any(|signal| signal.contains("disabled doctest")),
                "{info} changes doctest execution: {signals:?}"
            );
        }
    }

    #[test]
    fn table_merges_helper_extraction_and_production_assertion_removal_are_allowed() {
        let changes = [
            source_change(
                "#[test] fn first() { assert!(first()); } #[test] fn second() { assert!(second()); }",
                "#[test] fn table() { for case in cases() { assert!(case); } }",
            ),
            source_change(
                "#[test] fn first() { assert!(first()); assert!(second()); }",
                "fn check(value: bool) { assert!(value); } #[test] fn first() { check(first()); check(second()); }",
            ),
            source_change(
                "pub fn production() { assert!(ready()); }",
                "pub fn production() {}",
            ),
            CoverageChange {
                path: "login.py",
                before: "def login(): assert ready()",
                after: "def login(): pass",
            },
            CoverageChange {
                path: "login.ts",
                before: "function login() { assert(ready()); }",
                after: "function login() {}",
            },
            CoverageChange {
                path: "login.go",
                before: "func Login() { assert.True(ready()) }",
                after: "func Login() {}",
            },
            source_change(
                "const EXAMPLE: &str = r###\"#[test] fn fake() { assert!(true); }\"###;",
                "const EXAMPLE: &str = \"example\";",
            ),
            CoverageChange {
                path: "test_login.py",
                before: "\"\"\"def test_fake(): assert True\"\"\"\ndef test_login(): assert ready()",
                after: "def test_login(): assert ready()",
            },
        ];
        for change in changes {
            let signals = weakening_for(change.path, change.before, Some(change.after));
            assert!(
                signals.is_empty(),
                "refactoring and non-executable examples keep coverage: {signals:?}"
            );
        }
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
