//! Admitting a run when the OS cannot give it full containment (architecture §5.7 "reduced
//! mode", §5.12 scoped Decisions, P13).
//!
//! A missing capability never degrades silently: the run is refused unless a human granted a
//! scoped, unexpired Decision covering exactly what is missing, or a documented default policy
//! fixed in code covers it, and an admitted reduced run carries the list of what is missing for
//! its whole life.

use std::fmt;

/// A containment guarantee the sandbox may or may not be able to give on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ContainmentCapability {
    /// Writes limited to the write set and scratch (Seatbelt, Landlock, AppContainer ACLs).
    WriteConfinement,
    /// Outgoing TCP limited to the egress broker.
    TcpEgress,
    /// Outgoing UDP limited to the egress broker.
    UdpEgress,
    /// Kaisa storage and other attempts' credentials are unreadable.
    StateReadIsolation,
    /// Every descendant process contained and endable together.
    DescendantContainment,
}

impl ContainmentCapability {
    /// Every capability: what a run without any OS sandbox is missing.
    pub const ALL: [Self; 5] = [
        Self::WriteConfinement,
        Self::TcpEgress,
        Self::UdpEgress,
        Self::StateReadIsolation,
        Self::DescendantContainment,
    ];

    /// The stable code a person types (`--accept-reduced`) and the ledger records.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::WriteConfinement => "write",
            Self::TcpEgress => "tcp",
            Self::UdpEgress => "udp",
            Self::StateReadIsolation => "state-read",
            Self::DescendantContainment => "descendants",
        }
    }

    /// The capability with `code`, or `None` if Kaisa knows no such capability.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|capability| capability.code() == code)
    }
}

impl fmt::Display for ContainmentCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WriteConfinement => "write confinement (write)",
            Self::TcpEgress => "TCP egress through the broker (tcp)",
            Self::UdpEgress => "UDP egress through the broker (udp)",
            Self::StateReadIsolation => "Kaisa state read isolation (state-read)",
            Self::DescendantContainment => "descendant containment (descendants)",
        })
    }
}

/// The capabilities a Landlock sandbox cannot give at kernel ABI `abi` (`None`: no Landlock),
/// with or without the seccomp filter that refuses internet datagram sockets.
///
/// ABI 1 and 2 do not handle truncation, so a file outside the write set can still be emptied;
/// only ABI 3 and later confine writes. TCP is handled from ABI 4. Landlock never covers UDP.
#[must_use]
pub fn landlock_missing(abi: Option<i64>, udp_filtered: bool) -> Vec<ContainmentCapability> {
    let Some(abi) = abi else {
        return ContainmentCapability::ALL.to_vec();
    };
    // The current ruleset handles writes and network, leaving file reads open.
    let mut missing: Vec<ContainmentCapability> = vec![ContainmentCapability::StateReadIsolation];
    if abi < LANDLOCK_TRUNCATE_ABI {
        missing.push(ContainmentCapability::WriteConfinement);
    }
    if abi < LANDLOCK_NETWORK_ABI {
        missing.push(ContainmentCapability::TcpEgress);
    }
    if !udp_filtered {
        missing.push(ContainmentCapability::UdpEgress);
    }
    missing
}

/// The first Landlock ABI that handles truncation, and so confines every kind of write.
pub const LANDLOCK_TRUNCATE_ABI: i64 = 3;
/// The first Landlock ABI that handles TCP connections.
pub const LANDLOCK_NETWORK_ABI: i64 = 4;

/// A human's scoped permission to run without some capabilities until a deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReducedModeDecision {
    /// The capabilities the human accepted running without.
    pub accepts_missing: Vec<ContainmentCapability>,
    /// When the permission ends, in milliseconds on the daemon's clock.
    pub expires_at_ms: u64,
}

impl ReducedModeDecision {
    /// Remaining permission, bounded by both clocks. A backwards wall clock fails closed.
    #[must_use]
    pub fn remaining_ms(&self, started_ms: u64, now_ms: u64, elapsed_ms: u64) -> Option<u64> {
        if now_ms < started_ms {
            return None;
        }
        let wall = self.expires_at_ms.checked_sub(now_ms)?;
        let monotonic = self
            .expires_at_ms
            .checked_sub(started_ms)?
            .checked_sub(elapsed_ms)?;
        let remaining = wall.min(monotonic);
        (remaining > 0).then_some(remaining)
    }
}

/// The executable kind to which a default containment policy may apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainmentAgentKind {
    /// Claude Code.
    Claude,
    /// Codex under its own sandbox.
    Observed,
    /// Kaisa's loopback-only model client.
    LocalModel,
    /// A scripted or other agent with no default reduced permission.
    Other,
}

/// A documented default that admits a reduced run without a per-run Decision. The set is fixed
/// in code: nothing a person or an agent types can name a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DefaultReducedPolicy {
    /// Claude Code runs without OS confinement until a confined run is verified on every OS
    /// (T16.0; README "Protection status").
    ClaudeReducedUntilT16,
    /// Windows local models need loopback, unavailable inside AppContainer (README).
    WindowsLocalModel,
    /// An observed agent (Codex, T32c) writes under its own sandbox; its changes are checked
    /// against the write set afterwards, not on each write.
    ObservedCheckedAfterwards,
}

impl DefaultReducedPolicy {
    /// Each default is bound to its documented executable kind.
    #[must_use]
    pub fn applies_to(self, kind: ContainmentAgentKind) -> bool {
        match self {
            Self::ClaudeReducedUntilT16 => kind == ContainmentAgentKind::Claude,
            Self::ObservedCheckedAfterwards => kind == ContainmentAgentKind::Observed,
            Self::WindowsLocalModel => cfg!(windows) && kind == ContainmentAgentKind::LocalModel,
        }
    }

    /// The stable code the ledger records.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::ClaudeReducedUntilT16 => "claude-reduced-until-T16.0",
            Self::WindowsLocalModel => "windows-local-model-loopback",
            Self::ObservedCheckedAfterwards => "observed-checked-afterwards",
        }
    }
}

impl fmt::Display for DefaultReducedPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ClaudeReducedUntilT16 => {
                "default policy claude-reduced-until-T16.0: Claude Code runs reduced until its confined run is verified (T16.0)"
            }
            Self::WindowsLocalModel => "default policy windows-local-model-loopback: local models run reduced because AppContainer has no loopback",
            Self::ObservedCheckedAfterwards => {
                "default policy observed-checked-afterwards: an observed agent's changes are checked afterwards (T32c)"
            }
        })
    }
}

/// What may admit a run that is missing capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReducedModeBasis {
    /// Nothing: only a fully contained run starts.
    None,
    /// A person's scoped, expiring Decision.
    Decision(ReducedModeDecision),
    /// A documented default policy, fixed in code.
    DefaultPolicy(DefaultReducedPolicy),
}

impl ReducedModeBasis {
    /// The stable code the ledger records: `none`, `decision`, or the default policy's code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Decision(_) => "decision",
            Self::DefaultPolicy(policy) => policy.code(),
        }
    }

    /// When the basis stops admitting anything, if it ever does.
    #[must_use]
    pub fn expires_at_ms(&self) -> Option<u64> {
        match self {
            Self::Decision(decision) => Some(decision.expires_at_ms),
            Self::None | Self::DefaultPolicy(_) => None,
        }
    }
}

impl fmt::Display for ReducedModeBasis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("no Decision"),
            Self::Decision(decision) => write!(
                f,
                "your Decision accepting {} until {} ms",
                codes(&decision.accepts_missing),
                decision.expires_at_ms
            ),
            Self::DefaultPolicy(policy) => policy.fmt(f),
        }
    }
}

/// `write, tcp`: the codes of `capabilities`, as a person types them.
#[must_use]
pub fn codes(capabilities: &[ContainmentCapability]) -> String {
    capabilities
        .iter()
        .map(|capability| capability.code())
        .collect::<Vec<_>>()
        .join(",")
}

/// Whether and how a run may start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Every capability is available.
    Full,
    /// Some capabilities are missing and a Decision accepts it; the run is labelled with them.
    Reduced {
        /// What the run is missing, shown on the run for its whole life.
        missing: Vec<ContainmentCapability>,
    },
    /// Some capabilities are missing and nothing accepts it.
    Refused {
        /// What a Decision would have to accept.
        missing: Vec<ContainmentCapability>,
    },
}

impl Admission {
    /// What the run is (or would be) missing; empty for a fully contained run.
    #[must_use]
    pub fn missing(&self) -> &[ContainmentCapability] {
        match self {
            Self::Full => &[],
            Self::Reduced { missing } | Self::Refused { missing } => missing,
        }
    }

    /// The stable code the ledger records.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Reduced { .. } => "reduced",
            Self::Refused { .. } => "refused",
        }
    }
}

/// Decides admission from the capabilities `missing` on this machine and an optional
/// Decision, at `now_ms`.
#[must_use]
pub fn admit(
    missing: &[ContainmentCapability],
    decision: Option<&ReducedModeDecision>,
    now_ms: u64,
) -> Admission {
    if missing.is_empty() {
        return Admission::Full;
    }
    let mut missing: Vec<ContainmentCapability> = missing.to_vec();
    missing.sort();
    missing.dedup();
    let accepted: bool = decision.is_some_and(|decision| {
        now_ms < decision.expires_at_ms
            && missing
                .iter()
                .all(|capability| decision.accepts_missing.contains(capability))
    });
    if accepted {
        Admission::Reduced { missing }
    } else {
        Admission::Refused { missing }
    }
}

/// Decides admission on `basis`: a person's Decision is checked for scope and expiry; a default
/// policy fixed in code accepts what it documents (every capability, without expiry).
#[must_use]
pub fn admit_on(
    missing: &[ContainmentCapability],
    basis: &ReducedModeBasis,
    now_ms: u64,
) -> Admission {
    match basis {
        ReducedModeBasis::None => admit(missing, None, now_ms),
        ReducedModeBasis::Decision(decision) => admit(missing, Some(decision), now_ms),
        ReducedModeBasis::DefaultPolicy(_) => admit(
            missing,
            Some(&ReducedModeDecision {
                accepts_missing: ContainmentCapability::ALL.to_vec(),
                expires_at_ms: u64::MAX,
            }),
            now_ms,
        ),
    }
}

/// Checks policy scope as well as Decision scope and expiry for an executable kind.
#[must_use]
pub fn admit_agent_on(
    missing: &[ContainmentCapability],
    basis: &ReducedModeBasis,
    kind: ContainmentAgentKind,
    now_ms: u64,
) -> Admission {
    if let ReducedModeBasis::DefaultPolicy(policy) = basis
        && !policy.applies_to(kind)
    {
        return admit(missing, None, now_ms);
    }
    admit_on(missing, basis, now_ms)
}

#[cfg(test)]
mod tests {
    use super::{
        Admission, ContainmentCapability, DefaultReducedPolicy, LANDLOCK_NETWORK_ABI,
        LANDLOCK_TRUNCATE_ABI, ReducedModeBasis, ReducedModeDecision, admit, admit_on,
        landlock_missing,
    };

    /// When the run is requested: 2026-10-30 09:00 UTC.
    const NINE_O_CLOCK_MS: u64 = 1_793_350_800_000;
    /// One hour, in milliseconds.
    const HOUR_MS: u64 = 60 * 60 * 1000;
    /// The wall clock moved backwards before any monotonic time elapsed.
    const NO_ELAPSED_TIME_MS: u64 = 0;

    /// The Linux gap today: Landlock confines TCP but not UDP.
    fn the_linux_udp_gap() -> Vec<ContainmentCapability> {
        vec![ContainmentCapability::UdpEgress]
    }

    fn a_decision_accepting_the_udp_gap_until(expires_at_ms: u64) -> ReducedModeDecision {
        ReducedModeDecision {
            accepts_missing: vec![ContainmentCapability::UdpEgress],
            expires_at_ms,
        }
    }

    #[test]
    fn a_missing_capability_refuses_the_run_without_a_decision() {
        assert_eq!(
            admit(&the_linux_udp_gap(), None, NINE_O_CLOCK_MS),
            Admission::Refused {
                missing: the_linux_udp_gap()
            },
            "weaker containment is never the silent default"
        );
    }

    #[test]
    fn a_reduced_run_is_labelled() {
        let decision = a_decision_accepting_the_udp_gap_until(NINE_O_CLOCK_MS + HOUR_MS);

        assert_eq!(
            admit(&the_linux_udp_gap(), Some(&decision), NINE_O_CLOCK_MS),
            Admission::Reduced {
                missing: the_linux_udp_gap()
            },
            "a run without full containment says exactly what it lacks"
        );
    }

    #[test]
    fn an_expired_decision_does_not_allow_a_reduced_run() {
        let decision = a_decision_accepting_the_udp_gap_until(NINE_O_CLOCK_MS);

        assert_eq!(
            admit(
                &the_linux_udp_gap(),
                Some(&decision),
                NINE_O_CLOCK_MS + HOUR_MS
            ),
            Admission::Refused {
                missing: the_linux_udp_gap()
            },
            "yesterday's permission is not today's"
        );
    }

    /// A kernel whose Landlock predates truncation rules (ABI 1 and 2).
    const ABI_WITHOUT_TRUNCATION: i64 = LANDLOCK_TRUNCATE_ABI - 1;
    /// A kernel whose Landlock confines writes but not TCP.
    const ABI_WITHOUT_NETWORK: i64 = LANDLOCK_NETWORK_ABI - 1;

    #[test]
    fn a_landlock_abi_without_truncation_rules_does_not_confine_writes() {
        let missing = landlock_missing(Some(ABI_WITHOUT_TRUNCATION), true);

        assert!(
            missing.contains(&ContainmentCapability::WriteConfinement),
            "a file outside the write set can still be truncated, so writes are not confined: \
             {missing:?}"
        );
    }

    #[test]
    fn a_landlock_abi_without_network_rules_does_not_confine_tcp() {
        let missing = landlock_missing(Some(ABI_WITHOUT_NETWORK), true);

        assert_eq!(
            missing,
            vec![
                ContainmentCapability::StateReadIsolation,
                ContainmentCapability::TcpEgress
            ],
            "ABI 3 confines every write but leaves TCP open"
        );
    }

    #[test]
    fn a_landlock_sandbox_without_the_datagram_filter_does_not_confine_udp() {
        let missing = landlock_missing(Some(LANDLOCK_NETWORK_ABI), false);

        assert_eq!(
            missing,
            vec![
                ContainmentCapability::StateReadIsolation,
                ContainmentCapability::UdpEgress
            ],
            "Landlock never covers UDP; only the seccomp filter does"
        );
    }

    #[test]
    fn a_landlock_sandbox_always_reports_that_kaisa_state_remains_readable() {
        assert!(
            landlock_missing(Some(LANDLOCK_NETWORK_ABI), true)
                .contains(&ContainmentCapability::StateReadIsolation),
            "write and network rules do not restrict reads of state or other attempt tokens"
        );
    }

    #[test]
    fn a_kernel_without_landlock_is_missing_every_capability() {
        assert_eq!(
            landlock_missing(None, true),
            ContainmentCapability::ALL.to_vec(),
            "no Landlock means the agent runs as if unconfined"
        );
    }

    #[test]
    fn a_decision_covering_only_part_of_the_gap_refuses_the_run() {
        let unconfined = ContainmentCapability::ALL.to_vec();
        let decision = a_decision_accepting_the_udp_gap_until(NINE_O_CLOCK_MS + HOUR_MS);

        assert_eq!(
            admit(&unconfined, Some(&decision), NINE_O_CLOCK_MS),
            Admission::Refused {
                missing: unconfined.clone()
            },
            "a Decision is scoped: accepting UDP does not accept unconfined writes"
        );
    }

    #[test]
    fn a_default_policy_admits_a_reduced_run_with_its_missing_list() {
        let unconfined = ContainmentCapability::ALL.to_vec();
        let basis = ReducedModeBasis::DefaultPolicy(DefaultReducedPolicy::ClaudeReducedUntilT16);

        assert_eq!(
            admit_on(&unconfined, &basis, NINE_O_CLOCK_MS),
            Admission::Reduced {
                missing: unconfined.clone()
            },
            "the documented default still labels the run with everything it lacks"
        );
    }

    #[test]
    fn no_basis_refuses_a_run_that_is_missing_anything() {
        assert_eq!(
            admit_on(
                &the_linux_udp_gap(),
                &ReducedModeBasis::None,
                NINE_O_CLOCK_MS
            ),
            Admission::Refused {
                missing: the_linux_udp_gap()
            },
            "without a Decision or a default policy, only full containment starts"
        );
    }

    #[test]
    fn every_capability_code_reads_back_as_that_capability() {
        for capability in ContainmentCapability::ALL {
            assert_eq!(
                ContainmentCapability::from_code(capability.code()),
                Some(capability),
                "what the person types names exactly the capability shown to them"
            );
        }
    }

    #[test]
    fn an_unknown_capability_code_names_nothing() {
        assert_eq!(
            ContainmentCapability::from_code("everything"),
            None,
            "a Decision cannot accept a capability Kaisa does not know"
        );
    }
    #[test]
    fn a_backwards_wall_clock_expires_a_previously_observed_decision() {
        let decision = a_decision_accepting_the_udp_gap_until(NINE_O_CLOCK_MS + HOUR_MS);
        assert_eq!(
            decision.remaining_ms(
                NINE_O_CLOCK_MS,
                NINE_O_CLOCK_MS - HOUR_MS,
                NO_ELAPSED_TIME_MS
            ),
            None,
            "clock rollback must fail closed instead of extending human permission"
        );
    }

    #[test]
    fn elapsed_monotonic_time_expires_a_decision_even_when_wall_time_stalls() {
        let decision = a_decision_accepting_the_udp_gap_until(NINE_O_CLOCK_MS + HOUR_MS);
        assert_eq!(
            decision.remaining_ms(NINE_O_CLOCK_MS, NINE_O_CLOCK_MS, HOUR_MS),
            None,
            "the original monotonic deadline bounds permission even when wall time stops"
        );
    }

    #[test]
    fn the_claude_default_refuses_every_other_agent_kind() {
        use super::{ContainmentAgentKind, admit_agent_on};
        let basis = ReducedModeBasis::DefaultPolicy(DefaultReducedPolicy::ClaudeReducedUntilT16);
        for kind in [
            ContainmentAgentKind::LocalModel,
            ContainmentAgentKind::Other,
            ContainmentAgentKind::Observed,
        ] {
            assert!(
                matches!(
                    admit_agent_on(&ContainmentCapability::ALL, &basis, kind, NINE_O_CLOCK_MS),
                    Admission::Refused { .. }
                ),
                "the Claude default must never admit {kind:?}"
            );
        }
        assert!(
            matches!(
                admit_agent_on(
                    &ContainmentCapability::ALL,
                    &basis,
                    ContainmentAgentKind::Claude,
                    NINE_O_CLOCK_MS
                ),
                Admission::Reduced { .. }
            ),
            "the documented Claude policy still applies to Claude"
        );
    }
}
