//! Context packs in the ledger (architecture §5.9): each attempt records the digest of the pack it
//! started from, and the pack's bytes are stored once per digest, so any run can be replayed.

use kaisa_domain::{AttemptId, ContentDigest, Event, EventKind};
use rusqlite::{OptionalExtension, Transaction, params};

use super::error::StoreError;
use super::event_store::{EventStore, append_in};
use crate::chain::ChainedEvent;
use crate::json::json_string;

/// Payload schema version of `ContextPackRecorded` events.
const PACK_PAYLOAD_VERSION: u16 = 1;

impl EventStore {
    /// Records that `attempt` starts from the pack with `digest` and `bytes`, atomically: the
    /// event, and the bytes if no pack with this digest is stored yet.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::DigestMismatch`] if `bytes` do not hash to `digest`; nothing is
    /// written then. Returns another [`StoreError`] if the transaction fails.
    pub fn record_context_pack(
        &mut self,
        attempt: &AttemptId,
        digest: &ContentDigest,
        bytes: &[u8],
    ) -> Result<ChainedEvent, StoreError> {
        if blake3::hash(bytes).to_hex().as_str() != digest.as_str() {
            return Err(StoreError::DigestMismatch(digest.clone()));
        }
        let transaction: Transaction<'_> = self.connection.transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO context_packs (digest, bytes) VALUES (?1, ?2)",
            params![digest.as_str(), bytes],
        )?;
        let chained: ChainedEvent = append_in(
            &transaction,
            Event {
                kind: EventKind::ContextPackRecorded,
                subject: attempt.as_str().to_owned(),
                payload_version: PACK_PAYLOAD_VERSION,
                payload: format!(r#"{{"digest":{}}}"#, json_string(digest.as_str())),
            },
        )?;
        transaction.commit()?;
        Ok(chained)
    }

    /// The bytes of the pack with `digest`, if one was recorded.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the query fails.
    pub fn context_pack(&self, digest: &ContentDigest) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT bytes FROM context_packs WHERE digest = ?1",
                params![digest.as_str()],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// How many distinct packs are stored.
    #[cfg(test)]
    pub(crate) fn stored_context_pack_count(&self) -> Result<usize, StoreError> {
        let count: i64 =
            self.connection
                .query_row("SELECT COUNT(*) FROM context_packs", [], |row| row.get(0))?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{AttemptId, ContentDigest};

    use super::super::test_support::{ScratchLedger, a_ledger_path_that_does_not_exist_yet};
    use super::super::{EventStore, StoreError};

    /// The bytes of a small context pack, as `kaisa-context` would encode it.
    const LOGIN_FIX_PACK: &[u8] = b"kaisa-context-pack v1\nspec: make the login test wait";
    /// One pack, however many attempts start from it.
    const ONE_STORED_PACK: usize = 1;

    fn digest_of(bytes: &[u8]) -> ContentDigest {
        ContentDigest::try_new(blake3::hash(bytes).to_hex().as_str()).expect("blake3 hex")
    }

    fn attempt(text: &str) -> AttemptId {
        AttemptId::try_new(text).expect("a valid attempt id")
    }

    fn an_open_ledger(scratch: &ScratchLedger) -> EventStore {
        EventStore::open(&scratch.path()).expect("a new ledger opens")
    }

    #[test]
    fn a_recorded_pack_can_be_read_back_by_its_digest() {
        let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
        let mut store: EventStore = an_open_ledger(&scratch);
        let digest: ContentDigest = digest_of(LOGIN_FIX_PACK);

        store
            .record_context_pack(&attempt("att_41a"), &digest, LOGIN_FIX_PACK)
            .expect("recording succeeds");

        assert_eq!(
            store.context_pack(&digest).expect("reading succeeds"),
            Some(LOGIN_FIX_PACK.to_vec()),
            "a run can be replayed from exactly the context it started with"
        );
    }

    #[test]
    fn recording_the_same_pack_twice_stores_it_once() {
        let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
        let mut store: EventStore = an_open_ledger(&scratch);
        let digest: ContentDigest = digest_of(LOGIN_FIX_PACK);

        for candidate in ["att_41a", "att_41b"] {
            store
                .record_context_pack(&attempt(candidate), &digest, LOGIN_FIX_PACK)
                .expect("recording succeeds");
        }

        assert_eq!(
            store
                .stored_context_pack_count()
                .expect("counting succeeds"),
            ONE_STORED_PACK,
            "candidates of one task share their pack instead of copying it"
        );
    }

    #[test]
    fn a_pack_whose_bytes_do_not_match_its_digest_is_refused() {
        let scratch: ScratchLedger = a_ledger_path_that_does_not_exist_yet();
        let mut store: EventStore = an_open_ledger(&scratch);
        let digest_of_other_bytes: ContentDigest = digest_of(b"a different pack");

        let recorded =
            store.record_context_pack(&attempt("att_41a"), &digest_of_other_bytes, LOGIN_FIX_PACK);

        assert!(
            matches!(recorded, Err(StoreError::DigestMismatch(_))),
            "an address that does not match its content would make replays lie, got {recorded:?}"
        );
    }
}
