//! Roles and the sandbox each one gets (architecture §5.2 role table, §5.7 containment).
//!
//! The profile is decided by Kaisa from the role and the Task's write set, never by the agent.
//! OS backends (Seatbelt, Landlock, AppContainer) enforce it for every process of the attempt.

use crate::repo_path::RepoPath;
use crate::write_set::WriteSet;

/// What part an agent plays in a run. Each role has a fixed containment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Role {
    /// Reads code and documents; may reach the allowed web through the broker.
    Researcher,
    /// Reads and proposes a plan.
    Architect,
    /// Changes code in its own worktree.
    Developer,
    /// Writes and runs tests in its own worktree.
    Tester,
    /// Reviews a frozen candidate.
    Reviewer,
    /// Reviews a frozen candidate for security.
    Security,
    /// Writes documentation in its own worktree.
    Documentation,
    /// Plans and decides through coordinator tools only.
    Coordinator,
}

/// Where an attempt's processes may connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// Nowhere at all.
    None,
    /// Only the attempt's egress broker, which applies the domain allow-list.
    BrokerOnly,
}

/// What every process of an attempt may touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxProfile {
    /// Whether the repository (worktree or snapshot) may be read at all.
    pub may_read_repository: bool,
    /// Paths that may be changed; empty for read-only roles.
    pub writable: Vec<RepoPath>,
    /// Whether the attempt gets a scratch area for build output and caches.
    pub scratch: bool,
    /// Where processes may connect.
    pub network: Network,
}

/// The sandbox for `role` working on a Task with `write_set`.
#[must_use]
pub fn sandbox_profile(role: Role, write_set: &WriteSet) -> SandboxProfile {
    let writes: bool = matches!(role, Role::Developer | Role::Tester | Role::Documentation);
    SandboxProfile {
        may_read_repository: role != Role::Coordinator,
        writable: if writes {
            write_set.entries().to_vec()
        } else {
            Vec::new()
        },
        scratch: matches!(role, Role::Developer | Role::Tester),
        network: match role {
            Role::Researcher | Role::Developer | Role::Tester => Network::BrokerOnly,
            _ => Network::None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{Network, Role, SandboxProfile, sandbox_profile};
    use crate::repo_path::RepoPath;
    use crate::write_set::WriteSet;

    fn the_auth_write_set() -> WriteSet {
        WriteSet::new(vec![RepoPath::try_new("src/auth").expect("a valid path")])
    }

    #[test]
    fn a_developer_may_write_only_its_write_set_and_scratch() {
        let profile: SandboxProfile = sandbox_profile(Role::Developer, &the_auth_write_set());

        assert_eq!(
            (profile.writable, profile.scratch, profile.network),
            (
                the_auth_write_set().entries().to_vec(),
                true,
                Network::BrokerOnly
            ),
            "a developer changes its own paths, builds in scratch and reaches the web only via the broker"
        );
    }

    #[test]
    fn a_reviewer_may_write_nothing() {
        let profile: SandboxProfile = sandbox_profile(Role::Reviewer, &the_auth_write_set());

        assert_eq!(
            (
                profile.writable.is_empty(),
                profile.scratch,
                profile.network
            ),
            (true, false, Network::None),
            "a reviewer cannot change what it judges, nor send it anywhere"
        );
    }
}
