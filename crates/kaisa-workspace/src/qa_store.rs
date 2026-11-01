//! Immutable private QA artifacts, kept outside all Git worktrees.

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The independently authored suite's purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QaKind {
    /// Tests mapped to the approved acceptance criteria.
    Acceptance,
    /// Boundary, malformed-input and recovery scenarios.
    Adversarial,
}

impl QaKind {
    /// Stable directory and event label.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Acceptance => "acceptance",
            Self::Adversarial => "adversarial",
        }
    }
}

/// One agent proposal. Paths are single filenames, never repository paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QaTestFile {
    /// Stable scenario identifier.
    pub test_id: String,
    /// Frozen criterion identifiers this scenario exercises.
    pub criterion_ids: Vec<String>,
    /// Filename ending in .rs, .js, .mjs or .cjs.
    pub file: String,
    /// Test source, private to QA.
    pub contents: String,
}

pub use kaisa_domain::spec::QaManifestEntry;

/// A frozen suite. The manifest digest is pinned in memory and in the ledger receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QaSuite {
    directory: PathBuf,
    manifest_digest: String,
    kind: QaKind,
}

impl QaSuite {
    /// Private storage path; never a developer checkout.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Digest to persist with the suite-recorded event.
    #[must_use]
    pub fn manifest_digest(&self) -> &str {
        &self.manifest_digest
    }

    /// Suite purpose.
    #[must_use]
    pub fn kind(&self) -> QaKind {
        self.kind
    }

    /// Reopens a receipt using the trusted digest from its ledger event, verifying all bytes.
    ///
    /// # Errors
    /// Rejects changed manifests, symlinks and changed test contents.
    pub fn open(directory: &Path, kind: QaKind, manifest_digest: &str) -> io::Result<Self> {
        outside_worktrees(directory)?;
        let suite = Self {
            directory: directory.canonicalize()?,
            manifest_digest: manifest_digest.to_owned(),
            kind,
        };
        suite.verified_tests()?;
        Ok(suite)
    }

    /// Rechecks the pinned manifest and every file before returning bytes for execution.
    /// The runner must use these bytes, rather than reopen files after validation.
    ///
    /// # Errors
    /// Any changed byte or symlink is a tampering error, including manifest changes.
    pub fn verified_tests(&self) -> io::Result<Vec<QaTestFile>> {
        outside_worktrees(&self.directory)?;
        let manifest = regular_bytes(&self.directory.join("manifest.json"))?;
        if digest(&manifest) != self.manifest_digest {
            return Err(invalid("private QA manifest was tampered with"));
        }
        let entries: Value = serde_json::from_slice(&manifest).map_err(io::Error::other)?;
        let entries = entries
            .as_array()
            .ok_or_else(|| invalid("invalid QA manifest"))?;
        let mut tests = Vec::new();
        for entry in entries {
            let text = |key| {
                entry
                    .get(key)
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("invalid QA manifest field"))
            };
            let file = text("file")?;
            safe_component(file)?;
            let contents = regular_bytes(&self.directory.join(file))?;
            if digest(&contents) != text("digest")? {
                return Err(invalid("private QA test was tampered with"));
            }
            let criterion_ids = entry
                .get("criterion_ids")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("invalid QA criterion ids"))?
                .iter()
                .map(|id| {
                    id.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| invalid("invalid QA criterion id"))
                })
                .collect::<io::Result<Vec<_>>>()?;
            tests.push(QaTestFile {
                test_id: text("test_id")?.to_owned(),
                criterion_ids,
                file: file.to_owned(),
                contents: String::from_utf8(contents).map_err(io::Error::other)?,
            });
        }
        validate_tests(&tests)?;
        Ok(tests)
    }
}

/// Records a suite once, under state/qa/run/kind, sealing its files and manifest read-only.
///
/// # Errors
/// Refuses unsafe names, empty mappings, duplicates, symlinks, existing suites and a state
/// directory inside any worktree (including linked worktrees with a .git pointer).
pub fn record_qa_suite(
    state: &Path,
    run: &str,
    kind: QaKind,
    tests: &[QaTestFile],
) -> io::Result<QaSuite> {
    safe_component(run)?;
    validate_tests(tests)?;
    outside_worktrees(state)?;
    private_directory(state)?;
    let mut directory = state.canonicalize()?;
    for component in ["qa", run] {
        directory.push(component);
        private_directory(&directory)?;
    }
    directory.push(kind.code());
    std::fs::create_dir(&directory)?;
    private_directory(&directory)?;
    let mut manifest = Vec::new();
    for test in tests {
        let hash = digest(test.contents.as_bytes());
        seal_file(&directory.join(&test.file), test.contents.as_bytes())?;
        manifest.push(
            json!({"test_id": test.test_id, "criterion_ids": test.criterion_ids,
            "file": test.file, "digest": hash}),
        );
    }
    let bytes = serde_json::to_vec(&manifest).map_err(io::Error::other)?;
    seal_file(&directory.join("manifest.json"), &bytes)?;
    Ok(QaSuite {
        directory,
        manifest_digest: digest(&bytes),
        kind,
    })
}

fn validate_tests(tests: &[QaTestFile]) -> io::Result<()> {
    if tests.is_empty() {
        return Err(invalid("a private suite must contain tests"));
    }
    let mut ids = BTreeSet::new();
    let mut files = BTreeSet::new();
    for test in tests {
        safe_component(&test.test_id)?;
        safe_component(&test.file)?;
        let extension = Path::new(&test.file).extension().and_then(|s| s.to_str());
        if !matches!(extension, Some("rs" | "js" | "mjs" | "cjs"))
            || test.contents.trim().is_empty()
            || test.criterion_ids.is_empty()
            || test
                .criterion_ids
                .iter()
                .any(|id| safe_component(id).is_err())
            || !ids.insert(&test.test_id)
            || !files.insert(&test.file)
        {
            return Err(invalid("invalid or duplicate private QA test"));
        }
    }
    Ok(())
}

fn safe_component(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(invalid(
            "QA identifiers and filenames must be safe single components",
        ));
    }
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn outside_worktrees(path: &Path) -> io::Result<()> {
    let absolute = std::path::absolute(path)?;
    for ancestor in absolute.ancestors() {
        if ancestor.join(".git").try_exists()? {
            return Err(invalid("private QA storage must be outside every worktree"));
        }
        if ancestor.exists() {
            for canonical in ancestor.canonicalize()?.ancestors() {
                if canonical.join(".git").try_exists()? {
                    return Err(invalid("private QA storage resolves into a worktree"));
                }
            }
            break;
        }
    }
    Ok(())
}

fn private_directory(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(invalid("QA directory must not be a symlink")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => std::fs::create_dir_all(path)?,
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn regular_bytes(path: &Path) -> io::Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("private QA artifacts must be regular files"));
    }
    std::fs::read(path)
}

fn seal_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    let mut permissions = file.metadata()?.permissions();
    permissions.set_readonly(true);
    file.set_permissions(permissions)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{QaKind, QaTestFile, record_qa_suite};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "kaisa-private-qa-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("isolated QA state");
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn boundary_test() -> QaTestFile {
        QaTestFile {
            test_id: "empty_input_is_rejected".into(),
            criterion_ids: vec!["AC-1".into()],
            file: "boundary.rs".into(),
            contents: "#[test] fn empty_input_is_rejected() { panic!(\"expected rejection\"); }"
                .into(),
        }
    }
    #[test]
    fn a_recorded_private_suite_is_outside_worktrees_and_its_files_are_read_only() {
        let fixture = Fixture::new();
        let tests = vec![boundary_test()];
        let suite = record_qa_suite(
            &fixture.0.join("state"),
            "run_qa",
            QaKind::Acceptance,
            &tests,
        )
        .expect("private suite");
        assert_eq!(
            suite.verified_tests().expect("unchanged bytes"),
            tests,
            "the runner receives exactly the sealed tests"
        );
        assert!(
            suite.directory().ends_with("state/qa/run_qa/acceptance"),
            "private path follows the per-run contract"
        );
        for file in ["boundary.rs", "manifest.json"] {
            assert!(
                std::fs::metadata(suite.directory().join(file))
                    .expect("sealed artifact")
                    .permissions()
                    .readonly(),
                "recorded {file} is read only"
            );
        }
        let worktree = fixture.0.join("developer");
        std::fs::create_dir_all(worktree.join(".git")).expect("worktree marker");
        assert!(
            record_qa_suite(
                &worktree.join("state"),
                "run_qa",
                QaKind::Acceptance,
                &tests
            )
            .is_err(),
            "developer worktrees must never hold private suites"
        );
        assert!(
            !worktree.join("state").exists(),
            "rejection must happen before any artifact is written"
        );
    }
    #[test]
    fn a_tampered_private_test_is_rejected_before_its_contents_can_run() {
        let fixture = Fixture::new();
        let suite = record_qa_suite(&fixture.0, "run_qa", QaKind::Acceptance, &[boundary_test()])
            .expect("suite");
        let path = suite.directory().join("boundary.rs");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("simulate attacker");
        }
        #[cfg(not(unix))]
        {
            let mut permissions = std::fs::metadata(&path).expect("test").permissions();
            // This branch changes the Windows read-only attribute, not Unix mode bits.
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(&path, permissions).expect("simulate attacker");
        }
        std::fs::write(path, "tampered test").expect("simulate attack");
        assert!(
            suite
                .verified_tests()
                .expect_err("tampering must fail")
                .to_string()
                .contains("tampered"),
            "digest verification must identify changed source"
        );
    }
    #[test]
    fn a_manifest_changed_to_match_a_tampered_test_still_fails_its_pinned_digest() {
        let fixture = Fixture::new();
        let suite = record_qa_suite(&fixture.0, "run_qa", QaKind::Acceptance, &[boundary_test()])
            .expect("suite");
        let path = suite.directory().join("manifest.json");
        #[cfg(not(unix))]
        {
            let mut permissions = std::fs::metadata(&path)
                .expect("private manifest")
                .permissions();
            // This branch changes the Windows read-only attribute, not Unix mode bits.
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(&path, permissions)
                .expect("simulate attacker replacing manifest");
        }
        std::fs::remove_file(&path).expect("simulate attacker replacing immutable manifest");
        std::fs::write(path, "[]").expect("forged manifest");
        assert!(
            suite.verified_tests().is_err(),
            "a trusted receipt pins the manifest as well as its files"
        );
    }
}
