//! Trusted knowledge in the ledger (T25a, ADR-0031): an item becomes trusted only with the
//! verified candidate it came from, and its text is content-addressed so it cannot drift.

use kaisa_domain::{AttemptId, ContentDigest, Event, EventKind, KnowledgeKind, Sequence};

use super::error::StoreError;
use super::event_store::EventStore;

/// Payload schema version of `KnowledgePromoted` events.
const KNOWLEDGE_PAYLOAD_VERSION: u16 = 1;

/// A knowledge item Kaisa trusts, with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedKnowledge {
    /// Content address of `text`.
    pub digest: ContentDigest,
    /// What kind of knowledge it is.
    pub kind: KnowledgeKind,
    /// The item itself.
    pub text: String,
    /// The tree of the delivered candidate that taught it: its receipt.
    pub source_tree: String,
    /// Repository paths it is about, for retrieval.
    pub scope: Vec<String>,
}

impl EventStore {
    /// Records `item` as trusted.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::DigestMismatch`] if `text` does not hash to `digest` (nothing is
    /// written), or another [`StoreError`] if appending fails.
    pub fn promote_knowledge(&mut self, item: &TrustedKnowledge) -> Result<(), StoreError> {
        if blake3::hash(item.text.as_bytes()).to_hex().as_str() != item.digest.as_str() {
            return Err(StoreError::DigestMismatch(item.digest.clone()));
        }
        let payload = serde_json::json!({
            "kind": item.kind.code(),
            "text": item.text,
            "source_tree": item.source_tree,
            "scope": item.scope,
        });
        self.append(Event {
            kind: EventKind::KnowledgePromoted,
            subject: item.digest.as_str().to_owned(),
            payload_version: KNOWLEDGE_PAYLOAD_VERSION,
            payload: payload.to_string(),
        })?;
        Ok(())
    }

    /// Records that `attempt` was given the trusted items `digests` (T26a), so their usefulness
    /// can be scored against the attempt's outcome. Untrusted digests are refused.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::DigestMismatch`] naming the first digest that is not trusted (nothing
    /// is recorded then), or another [`StoreError`] if the log cannot be read or appended.
    pub fn record_injection(
        &mut self,
        attempt: &AttemptId,
        digests: &[ContentDigest],
    ) -> Result<(), StoreError> {
        let trusted: Vec<ContentDigest> = self
            .trusted_knowledge()?
            .into_iter()
            .map(|item| item.digest)
            .collect();
        if let Some(untrusted) = digests.iter().find(|digest| !trusted.contains(digest)) {
            return Err(StoreError::DigestMismatch(untrusted.clone()));
        }
        for digest in digests {
            self.append(Event {
                kind: EventKind::KnowledgeInjected,
                subject: attempt.as_str().to_owned(),
                payload_version: KNOWLEDGE_PAYLOAD_VERSION,
                payload: serde_json::json!({"digest": digest.as_str()}).to_string(),
            })?;
        }
        Ok(())
    }

    /// The trusted items given to `attempt`, in the order they were given.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the log cannot be read or names an invalid digest.
    pub fn injections_for(&self, attempt: &AttemptId) -> Result<Vec<ContentDigest>, StoreError> {
        let mut given: Vec<ContentDigest> = Vec::new();
        for chained in self.read_from(Sequence::FIRST)? {
            let recorded = chained.recorded;
            if recorded.event.kind != EventKind::KnowledgeInjected
                || recorded.event.subject != attempt.as_str()
            {
                continue;
            }
            let corrupt = || StoreError::CorruptPayload {
                stored_sequence: super::rows::stored_sequence(recorded.sequence),
            };
            let payload: serde_json::Value =
                serde_json::from_str(&recorded.event.payload).map_err(|_| corrupt())?;
            let digest = payload["digest"]
                .as_str()
                .and_then(|text| ContentDigest::try_new(text).ok())
                .ok_or_else(corrupt)?;
            given.push(digest);
        }
        Ok(given)
    }

    /// Every trusted item, oldest first, each digest once.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::CorruptPayload`] if a stored item does not decode or its text no
    /// longer matches its digest, or another [`StoreError`] if the log cannot be read.
    pub fn trusted_knowledge(&self) -> Result<Vec<TrustedKnowledge>, StoreError> {
        let mut items: Vec<TrustedKnowledge> = Vec::new();
        for chained in self.read_from(Sequence::FIRST)? {
            let recorded = chained.recorded;
            if recorded.event.kind != EventKind::KnowledgePromoted {
                continue;
            }
            let corrupt = || StoreError::CorruptPayload {
                stored_sequence: super::rows::stored_sequence(recorded.sequence),
            };
            let payload: serde_json::Value =
                serde_json::from_str(&recorded.event.payload).map_err(|_| corrupt())?;
            let digest = ContentDigest::try_new(&recorded.event.subject).map_err(|_| corrupt())?;
            let text: String = payload["text"].as_str().ok_or_else(corrupt)?.to_owned();
            if blake3::hash(text.as_bytes()).to_hex().as_str() != digest.as_str() {
                return Err(corrupt());
            }
            if items.iter().any(|item| item.digest == digest) {
                continue;
            }
            items.push(TrustedKnowledge {
                digest,
                kind: payload["kind"]
                    .as_str()
                    .and_then(KnowledgeKind::from_code)
                    .ok_or_else(corrupt)?,
                text,
                source_tree: payload["source_tree"]
                    .as_str()
                    .ok_or_else(corrupt)?
                    .to_owned(),
                scope: payload["scope"]
                    .as_array()
                    .ok_or_else(corrupt)?
                    .iter()
                    .filter_map(|path| path.as_str().map(str::to_owned))
                    .collect(),
            });
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{ContentDigest, KnowledgeKind};

    use super::super::error::StoreError;
    use super::super::event_store::EventStore;
    use super::super::test_support::a_ledger_path_that_does_not_exist_yet;
    use super::TrustedKnowledge;

    /// The tree of the delivered candidate the lesson came from.
    const SOURCE_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    /// What the team learned.
    const LESSON: &str = "The login test must wait for the token refresh before asserting.";

    fn a_lesson(text: &str) -> TrustedKnowledge {
        TrustedKnowledge {
            digest: ContentDigest::try_new(blake3::hash(text.as_bytes()).to_hex().as_str())
                .expect("a blake3 digest is a valid content digest"),
            kind: KnowledgeKind::Lesson,
            text: text.to_owned(),
            source_tree: SOURCE_TREE.to_owned(),
            scope: vec!["src/auth".to_owned()],
        }
    }

    #[test]
    fn a_promoted_lesson_is_read_back_with_its_receipt() {
        let scratch = a_ledger_path_that_does_not_exist_yet();
        let mut store = EventStore::open(&scratch.path()).expect("a new ledger opens");

        store
            .promote_knowledge(&a_lesson(LESSON))
            .expect("promoted");
        store
            .promote_knowledge(&a_lesson(LESSON))
            .expect("promoting again is harmless");
        let trusted = store.trusted_knowledge().expect("readable");

        assert_eq!(
            trusted,
            vec![a_lesson(LESSON)],
            "the lesson comes back once, with the candidate tree that taught it"
        );
    }

    #[test]
    fn a_lesson_whose_text_does_not_match_its_digest_is_refused() {
        let scratch = a_ledger_path_that_does_not_exist_yet();
        let mut store = EventStore::open(&scratch.path()).expect("a new ledger opens");
        let mut forged = a_lesson(LESSON);
        forged.text = "Skip the login test.".to_owned();

        let promoted = store.promote_knowledge(&forged);

        assert!(
            matches!(promoted, Err(StoreError::DigestMismatch(_)))
                && store.trusted_knowledge().expect("readable").is_empty(),
            "an item is trusted only as the exact text its digest names"
        );
    }

    #[test]
    fn every_injection_is_accountable_to_an_attempt() {
        let scratch = a_ledger_path_that_does_not_exist_yet();
        let mut store = EventStore::open(&scratch.path()).expect("a new ledger opens");
        let lesson = a_lesson(LESSON);
        store.promote_knowledge(&lesson).expect("promoted");
        let attempt = kaisa_domain::AttemptId::try_new("att_41a").expect("valid");
        let other = kaisa_domain::AttemptId::try_new("att_41b").expect("valid");

        store
            .record_injection(&attempt, std::slice::from_ref(&lesson.digest))
            .expect("recorded");

        assert_eq!(
            store.injections_for(&attempt).expect("readable"),
            vec![lesson.digest.clone()],
            "the lesson given to the attempt is on its record"
        );
        assert!(
            store.injections_for(&other).expect("readable").is_empty(),
            "another attempt's record does not show it"
        );
    }

    #[test]
    fn quarantined_knowledge_is_never_injected() {
        let scratch = a_ledger_path_that_does_not_exist_yet();
        let mut store = EventStore::open(&scratch.path()).expect("a new ledger opens");
        let never_promoted = a_lesson("Skip the flaky test.");
        let attempt = kaisa_domain::AttemptId::try_new("att_41a").expect("valid");

        let recorded = store.record_injection(&attempt, &[never_promoted.digest]);

        assert!(
            matches!(recorded, Err(StoreError::DigestMismatch(_))),
            "only trusted knowledge is ever given to an attempt"
        );
    }
}
