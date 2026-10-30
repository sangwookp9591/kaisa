//! Throwaway project directories for tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes scratch projects created by one test process.
static NEXT_PROJECT: AtomicU64 = AtomicU64::new(0);

/// A directory holding a few project files, removed when dropped.
pub(crate) struct ScratchProject {
    root: PathBuf,
}

impl ScratchProject {
    /// A project containing `files`, each given as a repository-relative path and its contents.
    pub(crate) fn with_files(files: &[(&str, &str)]) -> Self {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "kaisa-context-{}-{}",
            std::process::id(),
            NEXT_PROJECT.fetch_add(1, Ordering::Relaxed)
        ));
        for (relative, contents) in files {
            let path: PathBuf = root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("the scratch directory can be created");
            }
            std::fs::write(&path, contents).expect("the scratch file can be written");
        }
        std::fs::create_dir_all(&root).expect("the scratch root exists");
        Self { root }
    }

    /// The project's root directory.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for ScratchProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
