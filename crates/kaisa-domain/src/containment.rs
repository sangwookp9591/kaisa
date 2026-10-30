//! Admitting a run when the OS cannot give it full containment (architecture §5.7 "reduced
//! mode", §5.12 scoped Decisions, P13).
//!
//! A missing capability never degrades silently: the run is refused unless a human granted a
//! scoped, unexpired Decision covering exactly what is missing, and an admitted reduced run
//! carries the list of what is missing for its whole life.

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
    /// Every descendant process contained and endable together.
    DescendantContainment,
}

/// A human's scoped permission to run without some capabilities until a deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReducedModeDecision {
    /// The capabilities the human accepted running without.
    pub accepts_missing: Vec<ContainmentCapability>,
    /// When the permission ends, in milliseconds on the daemon's clock.
    pub expires_at_ms: u64,
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

#[cfg(test)]
mod tests {
    use super::{Admission, ContainmentCapability, ReducedModeDecision, admit};

    /// When the run is requested: 2026-10-30 09:00 UTC.
    const NINE_O_CLOCK_MS: u64 = 1_793_350_800_000;
    /// One hour, in milliseconds.
    const HOUR_MS: u64 = 60 * 60 * 1000;

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
}
