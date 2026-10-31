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
    repository: Option<&str>,
    goal: &str,
    write_set: &WriteSet,
) -> Result<Vec<(ContentDigest, String)>, StoreError> {
    let trusted: Vec<_> = ledger
        .trusted_knowledge()?
        .into_iter()
        .filter(|item| repository.is_some() && item.repository.as_deref() == repository)
        .collect();
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

#[cfg(test)]
mod tests {
    use kaisa_domain::KnowledgeKind;
    use kaisa_ledger::TrustedKnowledge;

    use super::*;

    #[test]
    fn legacy_and_other_repository_lessons_are_never_recalled() {
        let directory = std::env::temp_dir().join(format!("kaisa-recall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("scratch directory");
        let mut ledger = EventStore::open(&directory.join("kaisa.db")).expect("fresh ledger");
        for (repository, text) in [
            (None, "login legacy lesson"),
            (Some("repository-a"), "login repository a lesson"),
        ] {
            ledger
                .promote_knowledge(&TrustedKnowledge {
                    repository: repository.map(str::to_owned),
                    provenance: None,
                    digest: ContentDigest::try_new(blake3::hash(text.as_bytes()).to_hex().as_str())
                        .expect("digest"),
                    kind: KnowledgeKind::Lesson,
                    text: text.to_owned(),
                    source_tree: "tree".to_owned(),
                    scope: vec![],
                })
                .expect("promoted");
        }
        assert!(
            relevant_lessons(&ledger, Some("repository-b"), "login", &WriteSet::default())
                .expect("recall")
                .is_empty(),
            "neither a legacy item nor another repository's facts may enter a pack"
        );
        assert!(
            relevant_lessons(&ledger, None, "login", &WriteSet::default())
                .expect("recall")
                .is_empty(),
            "unknown current identity must fail closed too"
        );
        drop(ledger);
        let _ = std::fs::remove_dir_all(directory);
    }
}
