//! Learning from verified work only (T25a, ADR-0009 as amended by ADR-0031): a lesson becomes
//! trusted knowledge only when the delivery gate delivered the candidate it came from.

use kaisa_domain::{ContentDigest, KnowledgeKind};
use kaisa_ledger::{EventStore, StoreError, TrustedKnowledge};
use kaisa_workspace::Candidate;

use crate::gate::Delivery;

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
) -> Result<Option<ContentDigest>, StoreError> {
    if !matches!(delivery, Delivery::Deliver { .. }) || lesson.trim().is_empty() {
        return Ok(None);
    }
    // A blake3 hex digest is always well formed; if it ever were not, learning nothing is the
    // safe outcome.
    let Ok(digest) = ContentDigest::try_new(blake3::hash(lesson.as_bytes()).to_hex().as_str())
    else {
        return Ok(None);
    };
    ledger.promote_knowledge(&TrustedKnowledge {
        digest: digest.clone(),
        kind: KnowledgeKind::Lesson,
        text: lesson.to_owned(),
        source_tree: candidate.tree.clone(),
        scope: scope.to_vec(),
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
                learn_from_delivery(&mut ledger, delivery, &the_candidate(), LESSON, &scope)
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
        )
        .expect("recordable");
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
