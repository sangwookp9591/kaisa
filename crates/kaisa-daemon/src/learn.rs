//! Learning from verified work only (T25a, ADR-0009 as amended by ADR-0031): a lesson becomes
//! trusted knowledge only when the delivery gate delivered the candidate it came from.

use std::path::Path;

use kaisa_domain::{ContentDigest, KnowledgeKind};
use kaisa_ledger::{EventStore, StoreError, TrustedKnowledge};
use kaisa_workspace::Candidate;

use crate::gate::Delivery;

/// Maximum UTF-8 bytes in a stored lesson.
pub(crate) const MAX_LESSON_BYTES: usize = 4096;

/// Canonical root plus all initial commits. Failure disables learning and recall.
#[must_use]
pub(crate) fn repository_identity(repository: &Path) -> Option<String> {
    let root = repository.canonicalize().ok()?;
    let output = std::process::Command::new("git")
        .args(["rev-list", "--max-parents=0", "HEAD"])
        .current_dir(&root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commits = String::from_utf8(output.stdout).ok()?;
    let mut commits: Vec<_> = commits.lines().collect();
    commits.sort_unstable();
    if commits.is_empty() {
        return None;
    }
    Some(serde_json::json!({"root": root.to_str()?, "initial_commits": commits}).to_string())
}

/// Bounded, quoted observations; no transcript or command arguments enter a lesson.
#[must_use]
pub(crate) fn observed_lesson(goal: &str, paths: &[String], checks: &[String]) -> Option<String> {
    if !kaisa_workspace::scan_for_secrets(goal).is_empty()
        || paths
            .iter()
            .chain(checks)
            .any(|text| !kaisa_workspace::scan_for_secrets(text).is_empty())
    {
        return None;
    }
    let paths: Vec<_> = paths
        .iter()
        .take(32)
        .filter(|path| {
            path.len() <= 256
                && !path.chars().any(char::is_control)
                && kaisa_domain::RepoPath::try_new(path.as_str()).is_ok()
        })
        .collect();
    let summary: String = goal.chars().take(512).collect();
    let text = serde_json::json!({"goal_summary": summary, "changed_paths": paths,
        "passed_checks": checks.iter().take(32).collect::<Vec<_>>()})
    .to_string();
    (text.len() <= MAX_LESSON_BYTES).then_some(text)
}

/// Records `lesson` as trusted if `delivery` delivered `candidate`; returns its digest then.
/// Refused, revised or undecided work teaches nothing.
///
/// # Errors
///
/// Returns [`StoreError`] if the ledger cannot record the lesson.
pub fn learn_from_delivery(
    ledger: &mut EventStore,
    delivery: &Delivery,
    candidate: &Candidate,
    lesson: &str,
    scope: &[String],
    repository: &str,
    provenance: &str,
) -> Result<Option<ContentDigest>, StoreError> {
    if !matches!(delivery, Delivery::Deliver { .. })
        || lesson.trim().is_empty()
        || lesson.len() > MAX_LESSON_BYTES
        || !kaisa_workspace::scan_for_secrets(lesson).is_empty()
    {
        return Ok(None);
    }
    let weakened = ledger
        .read_from(kaisa_domain::Sequence::FIRST)?
        .into_iter()
        .filter(|event| event.recorded.event.kind == kaisa_domain::EventKind::CheckEvidence)
        .filter_map(|event| {
            serde_json::from_str::<serde_json::Value>(&event.recorded.event.payload).ok()
        })
        .any(|piece| {
            piece["candidate_tree"] == candidate.tree
                && piece["weakened_tests"]
                    .as_array()
                    .is_some_and(|signals| !signals.is_empty())
        });
    if weakened {
        return Ok(None);
    }
    // A blake3 hex digest is always well formed; if it ever were not, learning nothing is the
    // safe outcome.
    let Ok(digest) = ContentDigest::try_new(blake3::hash(lesson.as_bytes()).to_hex().as_str())
    else {
        return Ok(None);
    };
    ledger.promote_knowledge(&TrustedKnowledge {
        repository: Some(repository.to_owned()),
        provenance: Some(provenance.to_owned()),
        digest: digest.clone(),
        kind: KnowledgeKind::Lesson,
        text: lesson.to_owned(),
        source_tree: candidate.tree.clone(),
        scope: scope
            .iter()
            .take(32)
            .filter(|path| {
                path.len() <= 256
                    && !path.chars().any(char::is_control)
                    && kaisa_domain::RepoPath::try_new(path.as_str()).is_ok()
            })
            .cloned()
            .collect(),
    })?;
    Ok(Some(digest))
}

#[cfg(test)]
mod tests {
    use kaisa_ledger::EventStore;
    use kaisa_workspace::Candidate;

    use super::learn_from_delivery;
    use crate::gate::Delivery;

    /// What the attempt learned.
    const LESSON: &str = "The login test must wait for the token refresh before asserting.";

    fn the_candidate() -> Candidate {
        Candidate {
            commit: "1f2e3d4c5b6a79881726354453627180918a7b6c".to_owned(),
            tree: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_owned(),
        }
    }

    #[test]
    fn secret_observations_are_dropped_and_long_observations_are_bounded() {
        let secret = "AKIAABCDEFGHIJKLMNOP";
        let long_goal = "login ".repeat(super::MAX_LESSON_BYTES);
        let paths = vec!["src/login.rs".to_owned()];
        let checks = vec!["check_verified".to_owned()];
        assert!(
            super::observed_lesson(secret, &paths, &checks).is_none(),
            "a goal containing a credential never becomes knowledge"
        );
        assert!(
            super::observed_lesson("login", &[secret.to_owned()], &checks).is_none(),
            "a credential in an observed path never becomes knowledge"
        );
        let bounded = super::observed_lesson(&long_goal, &paths, &checks).expect("bounded facts");
        assert!(
            bounded.len() <= super::MAX_LESSON_BYTES && !bounded.contains("sh -c"),
            "the learned facts stay bounded and contain check names rather than command arguments"
        );
    }

    #[test]
    fn only_a_verified_outcome_becomes_a_lesson() {
        let directory = std::env::temp_dir().join(format!("kaisa-learn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("writable temp dir");
        let mut ledger = EventStore::open(&directory.join("kaisa.db")).expect("opens");
        let scope = vec!["src/auth".to_owned()];
        let not_delivered = [
            Delivery::Refuse("no trusted check produced evidence".to_owned()),
            Delivery::NeedsPerson("the candidate changed its checks".to_owned()),
            Delivery::Revise(Vec::new()),
        ];

        let from_unverified: Vec<bool> = not_delivered
            .iter()
            .map(|delivery| {
                learn_from_delivery(
                    &mut ledger,
                    delivery,
                    &the_candidate(),
                    LESSON,
                    &scope,
                    "/repo#initial",
                    "confined",
                )
                .expect("recordable")
                .is_some()
            })
            .collect();
        let after_unverified: usize = ledger.trusted_knowledge().expect("readable").len();
        let from_delivered = learn_from_delivery(
            &mut ledger,
            &Delivery::Deliver { reduced: false },
            &the_candidate(),
            LESSON,
            &scope,
            "/repo#initial",
            "confined",
        )
        .expect("recordable");
        let oversized = "x".repeat(super::MAX_LESSON_BYTES + 1);
        for unsafe_lesson in ["AKIAABCDEFGHIJKLMNOP", oversized.as_str()] {
            assert!(
                learn_from_delivery(
                    &mut ledger,
                    &Delivery::Deliver { reduced: false },
                    &the_candidate(),
                    unsafe_lesson,
                    &scope,
                    "/repo#initial",
                    "confined"
                )
                .expect("skip is not an error")
                .is_none(),
                "secret or oversized lesson text must be dropped at the storage boundary"
            );
        }
        let trusted = ledger.trusted_knowledge().expect("readable");
        let _ = std::fs::remove_dir_all(&directory);

        assert!(
            from_unverified.iter().all(|learned| !learned) && after_unverified == 0,
            "refused, undecided or revised work teaches nothing"
        );
        assert!(
            from_delivered.is_some()
                && trusted
                    .iter()
                    .all(|item| item.source_tree == the_candidate().tree),
            "a delivered candidate's lesson is trusted, with that candidate as its receipt"
        );
    }
}
