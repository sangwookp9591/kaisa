//! Recalling trusted lessons for a task (T25c, ADR-0031): what verified work taught about this
//! repository is retrieved for the next task on it, so a second session needs no
//! re-explanation. Only trusted knowledge is searched; nothing else is ever offered.

use kaisa_context::LessonIndex;
use kaisa_domain::{ContentDigest, RepoPath, WriteSet};
use kaisa_ledger::{EventStore, StoreError};

/// Lessons a pack carries at most: enough to help, few enough not to crowd out the task.
pub const MAX_LESSONS_PER_PACK: usize = 5;

/// The trusted lessons most relevant to `goal` on `write_set`, most relevant first.
///
/// # Errors
///
/// Returns [`StoreError`] if the ledger's knowledge cannot be read.
pub fn relevant_lessons(
    ledger: &EventStore,
    goal: &str,
    write_set: &WriteSet,
) -> Result<Vec<(ContentDigest, String)>, StoreError> {
    let trusted = ledger.trusted_knowledge()?;
    let mut index = LessonIndex::new();
    for item in &trusted {
        let scope: Vec<RepoPath> = item
            .scope
            .iter()
            .filter_map(|path| RepoPath::try_new(path).ok())
            .collect();
        index.add(item.digest.clone(), item.kind, &item.text, &scope);
    }
    Ok(index
        .search(goal, write_set.entries(), MAX_LESSONS_PER_PACK)
        .into_iter()
        .filter_map(|hit| {
            trusted
                .iter()
                .find(|item| item.digest == hit.id)
                .map(|item| (item.digest.clone(), item.text.clone()))
        })
        .collect())
}
