//! Who may do what (architecture §5.7 "Authority split", P10, ADR-0014 and ADR-0017).
//!
//! Authority comes from the endpoint a request arrived on, never from what the request says
//! about itself: the control endpoint carries Human authority, a requester bridge carries
//! Requester authority, and a per-attempt bridge carries Agent authority.

/// The authority a request carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Authority {
    /// A person, through the CLI, TUI or a presence-checked notification.
    Human,
    /// An agent session a person works in, such as `/kaisa` inside Claude Code.
    Requester,
    /// A worker or coordinator Kaisa started.
    Agent,
}

/// Something a request asks Kaisa to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Operation {
    /// Start a run within the user's budget and policy.
    StartRun,
    /// Read the status and results of runs.
    ReadStatus,
    /// Cancel a run the caller started.
    CancelOwnRun,
    /// Approve a candidate for delivery.
    Approve,
    /// Resolve a Gate: a decision that needs human authority.
    ResolveGate,
    /// Change policy, egress or budgets.
    ChangePolicy,
    /// Read or change stored secrets.
    ManageSecrets,
    /// Use the tools of the attempt's role.
    UseRoleTools,
}

impl Authority {
    /// True if a request with this authority may perform `operation`.
    #[must_use]
    pub fn allows(self, operation: Operation) -> bool {
        match self {
            Self::Human => true,
            Self::Requester => matches!(
                operation,
                Operation::StartRun | Operation::ReadStatus | Operation::CancelOwnRun
            ),
            Self::Agent => operation == Operation::UseRoleTools,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Authority, Operation};

    #[test]
    fn an_agent_can_never_approve() {
        for decision in [
            Operation::Approve,
            Operation::ResolveGate,
            Operation::ChangePolicy,
            Operation::ManageSecrets,
        ] {
            assert!(
                !Authority::Agent.allows(decision),
                "an agent must never approve its own work or widen its limits: {decision:?}"
            );
        }
    }

    #[test]
    fn a_requester_can_start_a_run_but_never_approve() {
        assert_eq!(
            (
                Authority::Requester.allows(Operation::StartRun),
                Authority::Requester.allows(Operation::Approve)
            ),
            (true, false),
            "asking from inside Claude Code may start work, but approving needs the person"
        );
    }

    #[test]
    fn a_human_can_resolve_a_gate() {
        assert!(
            Authority::Human.allows(Operation::ResolveGate),
            "decisions at permission boundaries belong to people"
        );
    }
}
