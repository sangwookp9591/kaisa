//! External effects announced through the outbox (architecture §5.2, P9).
//!
//! Anything that changes the world outside the ledger is first recorded as an intent with one
//! of these kinds, then executed, then completed with a receipt.

/// What kind of external effect an intent announces. The stable code never changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EffectKind {
    /// Create a git worktree for an attempt.
    CreateWorktree,
    /// Start an agent process under a keeper.
    SpawnAgent,
    /// Preserve an attempt's work (WIP commit or encrypted archive).
    PreserveWork,
    /// Remove a worktree whose work is covered by a preservation receipt.
    RemoveWorktree,
}

impl EffectKind {
    /// Every effect kind, for exhaustive lookups.
    pub const ALL: [Self; 4] = [
        Self::CreateWorktree,
        Self::SpawnAgent,
        Self::PreserveWork,
        Self::RemoveWorktree,
    ];

    /// Stable textual code used in storage.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::CreateWorktree => "worktree.create",
            Self::SpawnAgent => "agent.spawn",
            Self::PreserveWork => "work.preserve",
            Self::RemoveWorktree => "worktree.remove",
        }
    }

    /// Looks a kind up by its stable code.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.code() == code)
    }
}

#[cfg(test)]
mod tests {
    use super::EffectKind;

    /// A stable code must identify a single effect kind.
    const KINDS_PER_CODE: usize = 1;

    #[test]
    fn every_effect_kind_is_found_again_by_its_stable_code() {
        for kind in EffectKind::ALL {
            assert_eq!(
                EffectKind::from_code(kind.code()),
                Some(kind),
                "the stored code must map back to the same effect kind"
            );
        }
    }

    #[test]
    fn an_unknown_effect_code_is_not_mapped_to_any_kind() {
        let invented_by_a_future_version: &str = "teleport.repository";

        assert_eq!(
            EffectKind::from_code(invented_by_a_future_version),
            None,
            "an unknown effect must be reported, never mapped to a guess"
        );
    }

    #[test]
    fn no_two_effect_kinds_share_a_stable_code() {
        for kind in EffectKind::ALL {
            let sharing: usize = EffectKind::ALL
                .iter()
                .filter(|other| other.code() == kind.code())
                .count();
            assert_eq!(
                sharing,
                KINDS_PER_CODE,
                "`{}` must name exactly one effect kind",
                kind.code()
            );
        }
    }
}
