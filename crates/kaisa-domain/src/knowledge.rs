//! Knowledge kinds and content addresses (architecture §5.9).
//!
//! Knowledge is how a project is remembered across sessions. Each item has one of these kinds,
//! and context packs are identified by the digest of their bytes, so a run can be replayed.

use std::fmt;

/// What kind of knowledge an item is. The stable code never changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KnowledgeKind {
    /// Languages, commands, layout and conventions, derived by Kaisa.
    ProjectProfile,
    /// A failure signature and the kind of change that fixed it, from delivered work.
    Lesson,
    /// A scoped, project-level choice and its rationale.
    Decision,
    /// What one attempt used and how it ended: the input to routing and usefulness scores.
    Trajectory,
    /// Agent-native memory read in with its provenance; untrusted until confirmed.
    IngestedNote,
}

impl KnowledgeKind {
    /// Every knowledge kind, for exhaustive lookups.
    pub const ALL: [Self; 5] = [
        Self::ProjectProfile,
        Self::Lesson,
        Self::Decision,
        Self::Trajectory,
        Self::IngestedNote,
    ];

    /// Stable textual code used in storage.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::ProjectProfile => "knowledge.profile",
            Self::Lesson => "knowledge.lesson",
            Self::Decision => "knowledge.decision",
            Self::Trajectory => "knowledge.trajectory",
            Self::IngestedNote => "knowledge.ingested",
        }
    }

    /// Looks a kind up by its stable code.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.code() == code)
    }
}

/// Number of hexadecimal characters in a 256-bit digest.
pub const DIGEST_HEX_LEN: usize = 64;

/// Why a string is not a content digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DigestError {
    /// The digest does not have exactly [`DIGEST_HEX_LEN`] characters.
    BadLength,
    /// A character is not lowercase hexadecimal.
    NotLowercaseHex,
}

impl fmt::Display for DigestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadLength => write!(f, "a digest has exactly {DIGEST_HEX_LEN} hex characters"),
            Self::NotLowercaseHex => f.write_str("a digest uses lowercase hex only"),
        }
    }
}

impl std::error::Error for DigestError {}

/// The blake3 digest of some content, as 64 lowercase hex characters. One canonical spelling,
/// so equal content always has an equal address.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentDigest(String);

impl ContentDigest {
    /// Parses a digest written as 64 lowercase hex characters.
    ///
    /// # Errors
    ///
    /// Returns [`DigestError`] if the length or the characters are wrong.
    pub fn try_new(text: &str) -> Result<Self, DigestError> {
        if text.len() != DIGEST_HEX_LEN {
            return Err(DigestError::BadLength);
        }
        if !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DigestError::NotLowercaseHex);
        }
        Ok(Self(text.to_owned()))
    }

    /// The hex text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{ContentDigest, DIGEST_HEX_LEN, DigestError, KnowledgeKind};

    /// A digest of the right length in lowercase hex: what blake3 prints.
    fn a_pack_digest() -> String {
        "ab".repeat(DIGEST_HEX_LEN / 2)
    }

    #[test]
    fn every_knowledge_kind_round_trips_through_its_code() {
        for kind in KnowledgeKind::ALL {
            assert_eq!(
                KnowledgeKind::from_code(kind.code()),
                Some(kind),
                "stored knowledge must come back as the same kind"
            );
        }
    }

    #[test]
    fn a_digest_of_the_right_shape_is_accepted() {
        let digest: String = a_pack_digest();

        assert_eq!(
            ContentDigest::try_new(&digest).map(|parsed| parsed.as_str().to_owned()),
            Ok(digest),
            "a blake3 hex digest is kept exactly"
        );
    }

    #[test]
    fn a_digest_of_the_wrong_length_is_rejected() {
        let truncated: String = a_pack_digest()[1..].to_owned();

        assert_eq!(
            ContentDigest::try_new(&truncated),
            Err(DigestError::BadLength),
            "a shortened digest could match more than one pack"
        );
    }

    #[test]
    fn a_digest_with_uppercase_hex_is_rejected() {
        let shouted: String = a_pack_digest().to_uppercase();

        assert_eq!(
            ContentDigest::try_new(&shouted),
            Err(DigestError::NotLowercaseHex),
            "one spelling per digest, so equal packs have equal addresses"
        );
    }
}
