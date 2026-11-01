//! One lock for the canonical common Git directory, shared by every worktree command.
//!
//! Lock order: attempt owner lock (when present), repository mutex, advisory file lock.
//! The registry mutex is released before waiting for either repository lock. Nothing in
//! the critical section acquires an owner lock or runs agents, checks, or ledger writes;
//! it only runs one Git worktree command. Never nest these non-reentrant locks.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::git::GitError;

type RepositoryMutexes = HashMap<PathBuf, Weak<Mutex<()>>>;
static REPOSITORIES: OnceLock<Mutex<RepositoryMutexes>> = OnceLock::new();

pub(crate) fn with_lock<T>(
    common_directory: &Path,
    operation: impl FnOnce() -> Result<T, GitError>,
) -> Result<T, GitError> {
    let common = common_directory
        .canonicalize()
        .map_err(GitError::MetadataLock)?;
    let mutex = {
        let mut repositories = REPOSITORIES
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        repositories.retain(|_, mutex| mutex.strong_count() != 0);
        if let Some(mutex) = repositories.get(&common).and_then(Weak::upgrade) {
            mutex
        } else {
            let mutex = Arc::new(Mutex::new(()));
            repositories.insert(common.clone(), Arc::downgrade(&mutex));
            mutex
        }
    };
    let _thread_guard = mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Keep this inode permanently: unlinking it allows two owners to lock different files
    // under the same name. It lives outside worktrees/ so Git cleanup cannot delete it.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(common.join("kaisa-worktree.lock"))
        .map_err(GitError::MetadataLock)?;
    file.lock().map_err(GitError::MetadataLock)?;
    // Closing the file releases the OS lock on success, error, unwind, and process exit.
    operation()
}
