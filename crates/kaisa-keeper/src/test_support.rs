//! Test vocabulary for the keeper: scratch attempt directories.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes directories created by tests running in parallel in one process.
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// An attempt directory in the temp folder, removed when dropped.
pub(crate) struct ScratchAttemptDirectory {
    path: PathBuf,
}

impl ScratchAttemptDirectory {
    /// The directory.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchAttemptDirectory {
    fn drop(&mut self) {
        // Best effort: a leftover temp directory must not fail the test that used it.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A new, empty attempt directory.
pub(crate) fn an_empty_attempt_directory() -> ScratchAttemptDirectory {
    let path: PathBuf = std::env::temp_dir().join(format!(
        "kaisa-keeper-test-{}-{}",
        std::process::id(),
        NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).expect("the temp directory is writable");
    ScratchAttemptDirectory { path }
}
