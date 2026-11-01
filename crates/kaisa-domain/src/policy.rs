//! Personal policy and the immutable copy held by a run (architecture §5.7).

use std::fmt;

use crate::grade::Grade;

/// Window and constraints for measured routing. Rates are basis points (10,000 = one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingPolicy {
    /// Last completed task episodes retained per cell.
    pub window: usize,
    /// Completed task episodes required before trusting a cell.
    pub min_samples: usize,
    /// Minimum delivered rate in basis points.
    pub target_delivery_bps: u16,
    /// Exploration probability in basis points.
    pub exploration_bps: u16,
    /// Optional p90 attempt latency ceiling in milliseconds.
    pub latency_budget_ms: Option<u64>,
    /// Seed combined with task identity for reproducible exploration.
    pub seed: u64,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            window: 100,
            min_samples: 5,
            target_delivery_bps: 8000,
            exploration_bps: 500,
            latency_budget_ms: None,
            seed: 0,
        }
    }
}

/// The user's policy for newly started runs; changes apply to future snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// Accepts human approval after unconfined attempts, with reduced assurance labelled.
    pub accept_reduced_assurance_approval: bool,
    /// Allows explicitly labelled provisional specification approval.
    pub allow_provisional_spec: bool,
    /// Measured routing constraints captured with the run.
    pub routing: RoutingPolicy,
    /// How many revisions may follow a failed review.
    pub revision_limit: u32,
    /// Hosts that agents may reach through the egress broker.
    pub allowed_egress_hosts: Vec<String>,
    /// Whether agents must run under confinement.
    pub confine_agents: bool,
    /// The lowest model grade allowed for security review.
    pub min_grade_for_security_review: Grade,
    /// Whether changing check definitions requires a person's approval. Only `true` is
    /// admitted: a candidate that edits what its own checks run is never a pass on its own.
    pub require_person_when_check_definitions_change: bool,
    /// Whether test weakening requires a person, regardless of reviewer approval. Only `true`
    /// is admitted: a flagged candidate never counts as a pass (architecture §5.8).
    pub require_person_when_tests_weaken: bool,
}

/// Why a policy cannot be admitted for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyError {
    /// Invalid routing window, sample count, probability or latency budget.
    InvalidRoutingPolicy,
    /// Security review was assigned a grade below T3.
    SecurityReviewFloorBelowT3,
    /// The egress allow-list contains an empty host.
    EmptyEgressHost,
    /// Weakened tests were set not to stop for a person.
    TestWeakeningMustStopForAPerson,
    /// Changed check definitions were set not to stop for a person.
    CheckDefinitionChangesMustStopForAPerson,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRoutingPolicy => f.write_str("routing requires 1 <= min_samples <= window, rates <= 10000, and a positive latency budget"),
            Self::SecurityReviewFloorBelowT3 => {
                f.write_str("security review requires a grade of at least T3")
            }
            Self::EmptyEgressHost => f.write_str("an allowed egress host must not be empty"),
            Self::TestWeakeningMustStopForAPerson => f.write_str(
                "require_person_when_tests_weaken cannot be false: a candidate that weakens \
                 tests always stops for a person",
            ),
            Self::CheckDefinitionChangesMustStopForAPerson => f.write_str(
                "require_person_when_check_definitions_change cannot be false: a candidate \
                 that changes what its checks run always stops for a person",
            ),
        }
    }
}

impl std::error::Error for PolicyError {}

impl Policy {
    /// The personal default: two revisions, egress only to `api.anthropic.com` and
    /// `api.openai.com`, confinement enabled, T3 security review, and a person required
    /// when check definitions change.
    #[must_use]
    pub fn personal_default() -> Self {
        Self {
            accept_reduced_assurance_approval: false,
            allow_provisional_spec: false,
            routing: RoutingPolicy::default(),
            revision_limit: 2,
            allowed_egress_hosts: vec!["api.anthropic.com".into(), "api.openai.com".into()],
            confine_agents: true,
            min_grade_for_security_review: Grade::T3,
            require_person_when_check_definitions_change: true,
            require_person_when_tests_weaken: true,
        }
    }

    /// Checks the policy's security floor and egress list before run admission.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] if security review is below T3, an egress host is empty, or a
    /// rule that keeps tampered candidates from passing is switched off.
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.routing.min_samples == 0
            || self.routing.window < self.routing.min_samples
            || self.routing.target_delivery_bps > 10000
            || self.routing.exploration_bps > 10000
            || self.routing.latency_budget_ms == Some(0)
        {
            return Err(PolicyError::InvalidRoutingPolicy);
        }
        if self.min_grade_for_security_review < Grade::T3 {
            return Err(PolicyError::SecurityReviewFloorBelowT3);
        }
        if self.allowed_egress_hosts.iter().any(String::is_empty) {
            return Err(PolicyError::EmptyEgressHost);
        }
        if !self.require_person_when_tests_weaken {
            return Err(PolicyError::TestWeakeningMustStopForAPerson);
        }
        if !self.require_person_when_check_definitions_change {
            return Err(PolicyError::CheckDefinitionChangesMustStopForAPerson);
        }
        Ok(())
    }

    /// Encodes every field as a `key=value` line, sorted by key, with a final newline.
    ///
    /// Hosts are sorted by their original text and deduplicated, then comma-separated.
    /// Host bytes outside ASCII letters, digits, `-`, `.`, `_` and `~` are percent-encoded
    /// with uppercase hex, so delimiters cannot make different lists encode alike.
    /// Grades use the stable names `T0` through `T3`; booleans use `true` and `false`.
    /// The caller hashes these UTF-8 bytes into a [`crate::ContentDigest`]; this module
    /// performs no hashing. Encoding does not validate the policy.
    #[must_use]
    pub fn canonical_text(&self) -> String {
        let mut hosts: Vec<&str> = self
            .allowed_egress_hosts
            .iter()
            .map(String::as_str)
            .collect();
        hosts.sort_unstable();
        hosts.dedup();
        let hosts: String = hosts
            .into_iter()
            .map(encode_host)
            .collect::<Vec<_>>()
            .join(",");
        let grade: &str = match self.min_grade_for_security_review {
            Grade::T0 => "T0",
            Grade::T1 => "T1",
            Grade::T2 => "T2",
            Grade::T3 => "T3",
        };
        format!(
            "accept_reduced_assurance_approval={}\nallow_provisional_spec={}\nallowed_egress_hosts={hosts}\nconfine_agents={}\nmin_grade_for_security_review={grade}\nrequire_person_when_check_definitions_change={}\nrequire_person_when_tests_weaken={}\nrevision_limit={}\nrouting_exploration_bps={}\nrouting_latency_budget_ms={}\nrouting_min_samples={}\nrouting_seed={}\nrouting_target_delivery_bps={}\nrouting_window={}\n",
            self.accept_reduced_assurance_approval,
            self.allow_provisional_spec,
            self.confine_agents,
            self.require_person_when_check_definitions_change,
            self.require_person_when_tests_weaken,
            self.revision_limit,
            self.routing.exploration_bps,
            self.routing
                .latency_budget_ms
                .map_or_else(|| "none".to_owned(), |v| v.to_string()),
            self.routing.min_samples,
            self.routing.seed,
            self.routing.target_delivery_bps,
            self.routing.window,
        )
    }

    /// Validates this policy, then copies it and its canonical text for a run. Later edits to
    /// this policy cannot change the snapshot or the text that identifies it.
    ///
    /// # Errors
    ///
    /// Returns the [`PolicyError`] of [`Self::validate`]: an invalid policy never governs a run.
    pub fn snapshot(&self) -> Result<PolicySnapshot, PolicyError> {
        self.validate()?;
        Ok(PolicySnapshot {
            policy: self.clone(),
            canonical: self.canonical_text(),
        })
    }
}

/// An owned policy and its canonical text, fixed for the lifetime of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySnapshot {
    policy: Policy,
    canonical: String,
}

impl PolicySnapshot {
    /// The policy captured when the run started.
    #[must_use]
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// The captured canonical text whose UTF-8 bytes the caller may hash.
    #[must_use]
    pub fn canonical(&self) -> &str {
        &self.canonical
    }
}

fn encode_host(host: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded: String = String::new();
    for byte in host.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::{Policy, PolicyError};
    use crate::grade::Grade;

    const PERSONAL_REVISION_LIMIT: u32 = 2;
    const UPDATED_REVISION_LIMIT: u32 = 5;

    #[test]
    fn a_policy_change_mid_run_does_not_affect_the_running_attempt() {
        let mut personal_policy: Policy = Policy::personal_default();
        let running_attempt = personal_policy
            .snapshot()
            .expect("the personal default is valid");
        let original_policy: Policy = personal_policy.clone();
        let original_canonical: String = personal_policy.canonical_text();

        personal_policy.revision_limit = UPDATED_REVISION_LIMIT;
        personal_policy.allowed_egress_hosts.clear();
        personal_policy.confine_agents = false;

        assert_eq!(
            running_attempt.policy(),
            &original_policy,
            "every captured rule survives later policy edits"
        );
        assert_eq!(
            running_attempt.canonical(),
            original_canonical,
            "the running attempt keeps the bytes identifying its policy"
        );
        assert_ne!(
            personal_policy
                .snapshot()
                .expect("the edited policy is still valid"),
            running_attempt,
            "a future run captures the changed policy"
        );
    }

    #[test]
    fn the_canonical_text_does_not_depend_on_host_order_or_duplicates() {
        let personal_policy: Policy = Policy::personal_default();
        let mut reordered_policy: Policy = personal_policy.clone();
        reordered_policy.allowed_egress_hosts = vec![
            "api.openai.com".into(),
            "api.anthropic.com".into(),
            "api.openai.com".into(),
        ];

        assert_eq!(
            personal_policy.canonical_text(),
            reordered_policy.canonical_text(),
            "the allow-list is a set when identifying policy content"
        );
    }

    #[test]
    fn the_canonical_text_contains_every_rule_in_key_order() {
        let expected: &str = concat!(
            "accept_reduced_assurance_approval=false\n",
            "allow_provisional_spec=false\n",
            "allowed_egress_hosts=api.anthropic.com,api.openai.com\n",
            "confine_agents=true\n",
            "min_grade_for_security_review=T3\n",
            "require_person_when_check_definitions_change=true\n",
            "require_person_when_tests_weaken=true\n",
            "revision_limit=2\n",
            "routing_exploration_bps=500\nrouting_latency_budget_ms=none\nrouting_min_samples=5\nrouting_seed=0\nrouting_target_delivery_bps=8000\nrouting_window=100\n",
        );

        assert_eq!(
            Policy::personal_default().canonical_text(),
            expected,
            "the digest input has a stable spelling for every policy rule"
        );
    }

    #[test]
    fn host_delimiters_cannot_collide_with_a_different_egress_list() {
        let mut one_host: Policy = Policy::personal_default();
        one_host.allowed_egress_hosts = vec!["a,b\n%".into()];
        let mut two_hosts: Policy = one_host.clone();
        two_hosts.allowed_egress_hosts = vec!["a".into(), "b\n%".into()];

        assert_ne!(
            one_host.canonical_text(),
            two_hosts.canonical_text(),
            "host separators and line breaks must not hide different permissions"
        );
        assert!(
            one_host
                .canonical_text()
                .lines()
                .any(|line| line == "allowed_egress_hosts=a%2Cb%0A%25"),
            "escaped host bytes preserve one key-value line per field"
        );
    }

    #[test]
    fn a_policy_that_lowers_the_security_review_floor_is_rejected() {
        for weaker_grade in [Grade::T0, Grade::T1, Grade::T2] {
            let mut personal_policy: Policy = Policy::personal_default();
            personal_policy.min_grade_for_security_review = weaker_grade;

            assert_eq!(
                personal_policy.validate(),
                Err(PolicyError::SecurityReviewFloorBelowT3),
                "security review must never be routed to {weaker_grade:?}"
            );
        }
    }

    #[test]
    fn a_policy_that_lets_tampered_candidates_pass_without_a_person_is_rejected() {
        let mut weakening: Policy = Policy::personal_default();
        weakening.require_person_when_tests_weaken = false;
        let mut definitions: Policy = Policy::personal_default();
        definitions.require_person_when_check_definitions_change = false;

        assert_eq!(
            weakening.snapshot(),
            Err(PolicyError::TestWeakeningMustStopForAPerson),
            "weakened tests always need a person; no policy lifts that"
        );
        assert_eq!(
            definitions.snapshot(),
            Err(PolicyError::CheckDefinitionChangesMustStopForAPerson),
            "changed check definitions always need a person; no policy lifts that"
        );
    }

    #[test]
    fn a_policy_with_an_empty_egress_host_is_rejected() {
        let mut personal_policy: Policy = Policy::personal_default();
        personal_policy.allowed_egress_hosts.push(String::new());

        assert_eq!(
            personal_policy.validate(),
            Err(PolicyError::EmptyEgressHost),
            "an empty host cannot grant a meaningful egress permission"
        );
    }

    #[test]
    fn the_personal_default_is_valid_and_confines_agents() {
        let personal_policy: Policy = Policy::personal_default();

        assert_eq!(
            personal_policy.validate(),
            Ok(()),
            "the built-in policy must admit a personal run"
        );
        assert!(
            personal_policy.confine_agents,
            "personal runs require agent confinement"
        );
        assert_eq!(
            personal_policy.revision_limit, PERSONAL_REVISION_LIMIT,
            "personal runs bound revisions to two attempts"
        );
        assert_eq!(
            personal_policy.allowed_egress_hosts,
            vec!["api.anthropic.com", "api.openai.com"],
            "only the default model providers receive egress permission"
        );
        assert_eq!(
            personal_policy.min_grade_for_security_review,
            Grade::T3,
            "security review requires the heavy grade"
        );
        assert!(
            personal_policy.require_person_when_check_definitions_change,
            "a person approves changes to the checks used as evidence"
        );
    }

    #[test]
    fn an_invalid_policy_cannot_govern_a_run() {
        let mut lowered: Policy = Policy::personal_default();
        lowered.min_grade_for_security_review = Grade::T2;

        let snapshot = lowered.snapshot();

        assert_eq!(
            snapshot,
            Err(PolicyError::SecurityReviewFloorBelowT3),
            "a run cannot start under a policy that weakens security review"
        );
    }
}
