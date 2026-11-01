//! Versioned specifications, deterministic traceability and guarded approval.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A filesystem-safe specification identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SpecId(String);

impl SpecId {
    /// Validates the `spec_` prefix and a lowercase alphanumeric suffix.
    pub fn try_new(text: &str) -> Result<Self, String> {
        let suffix = text
            .strip_prefix("spec_")
            .ok_or("expected spec_ identifier")?;
        if suffix.is_empty()
            || suffix.len() > crate::MAX_SUFFIX_LEN
            || !suffix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return Err("invalid specification identifier".into());
        }
        Ok(Self(text.into()))
    }
    /// The complete identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for SpecId {
    type Error = String;
    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::try_new(&text)
    }
}
impl From<SpecId> for String {
    fn from(id: SpecId) -> Self {
        id.0
    }
}
impl fmt::Display for SpecId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An original, verbatim nonempty requirement line, assigned by Kaisa.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    /// Deterministic source order identifier, starting with R1.
    pub id: String,
    /// Original line, including bullet and indentation.
    pub text: String,
}
/// The behaviour an acceptance criterion exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CriterionKind {
    /// Ordinary behaviour.
    Normal,
    /// Boundary conditions.
    Edge,
    /// Failure behaviour.
    Error,
    /// A property that always holds.
    Invariant,
}
/// A traceable, observable acceptance condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceCriterion {
    /// Unique criterion identifier.
    pub id: String,
    /// Original requirements covered by this condition.
    pub requirement_ids: Vec<String>,
    /// Initial conditions.
    pub given: String,
    /// Trigger.
    pub when: String,
    /// Expected observation.
    pub then: String,
    /// Scenario category.
    pub kind: CriterionKind,
    /// Explicitly authorizes a legacy behaviour correction.
    #[serde(default)]
    pub correction: bool,
}
/// An unresolved question for the specification owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ambiguity {
    /// Unique question identifier.
    pub id: String,
    /// Requirements affected by the question.
    pub requirement_ids: Vec<String>,
    /// The question needing a decision.
    pub question: String,
}
/// Immutable content once a particular version is frozen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Specification {
    /// Stable identity across versions.
    pub id: SpecId,
    /// Monotonic version, starting at one.
    pub version: u32,
    /// Human-readable title.
    pub title: String,
    /// Source text partitioned by Kaisa.
    pub requirements: Vec<Requirement>,
    /// Agent-proposed acceptance conditions.
    pub criteria: Vec<AcceptanceCriterion>,
    /// Questions still requiring clarification.
    pub ambiguities: Vec<Ambiguity>,
    /// Properties that always hold.
    pub invariants: Vec<String>,
    /// Expected failure behaviour.
    pub error_behaviour: Vec<String>,
}
/// Deterministic findings; missing coverage names every uncovered source requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecFinding {
    /// Some requirements have no linked acceptance condition.
    MissingAcceptanceCriteria {
        /// Original requirement IDs with no acceptance condition.
        requirement_ids: Vec<String>,
    },
    /// An identifier is empty or duplicated.
    InvalidId(String),
    /// A criterion or ambiguity refers to a nonexistent requirement.
    UnknownRequirement(String),
    /// An acceptance condition has no requirement link.
    UnlinkedCriterion(String),
}
impl Specification {
    /// Sorted-key, compact JSON bytes, independent of struct field order.
    pub fn canonical_json(&self) -> Vec<u8> {
        serde_json::to_vec(&serde_json::to_value(self).expect("specification serializes"))
            .expect("JSON value serializes")
    }
    /// BLAKE3 address of the exact canonical bytes.
    pub fn digest(&self) -> String {
        blake3::hash(&self.canonical_json()).to_hex().to_string()
    }
    /// Reports invalid identifiers, references and missing acceptance coverage.
    pub fn validate(&self) -> Vec<SpecFinding> {
        let mut findings = Vec::new();
        let mut ids = BTreeSet::new();
        for id in self
            .requirements
            .iter()
            .map(|r| &r.id)
            .chain(self.criteria.iter().map(|c| &c.id))
            .chain(self.ambiguities.iter().map(|a| &a.id))
        {
            if id.is_empty() || !ids.insert(id) {
                findings.push(SpecFinding::InvalidId(id.clone()));
            }
        }
        let required: BTreeSet<_> = self.requirements.iter().map(|r| &r.id).collect();
        let mut covered = BTreeSet::new();
        for criterion in &self.criteria {
            if criterion.requirement_ids.is_empty() {
                findings.push(SpecFinding::UnlinkedCriterion(criterion.id.clone()));
            }
            for id in &criterion.requirement_ids {
                covered.insert(id);
            }
        }
        for id in covered
            .iter()
            .copied()
            .chain(self.ambiguities.iter().flat_map(|a| &a.requirement_ids))
        {
            if !required.contains(id) {
                findings.push(SpecFinding::UnknownRequirement(id.clone()));
            }
        }
        let missing: Vec<_> = required
            .difference(&covered)
            .map(|id| (*id).clone())
            .collect();
        if !missing.is_empty() {
            findings.push(SpecFinding::MissingAcceptanceCriteria {
                requirement_ids: missing,
            });
        }
        findings
    }
}
/// Splits nonempty original lines, preserving their bytes and source order.
pub fn split_requirements(text: &str) -> Vec<Requirement> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, text)| Requirement {
            id: format!("R{}", index + 1),
            text: text.into(),
        })
        .collect()
}

/// Approval provenance; human receipts can only be minted through [`approval::confirm_on_process_terminal`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SpecApproval {
    /// Policy-based permission, never human approval.
    Provisional {
        /// Policy authorizing provisional use.
        policy: String,
    },
    /// A guarded terminal confirmation; its receipt has private fields and no deserializer.
    HumanApproved(approval::HumanApproval),
}
impl SpecApproval {
    /// Stable provenance label for storage and display.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Provisional { .. } => "provisional",
            Self::HumanApproved(_) => "human-approved",
        }
    }
    /// Human identity only for a terminal confirmation.
    pub fn user(&self) -> Option<&str> {
        match self {
            Self::HumanApproved(receipt) => Some(receipt.user()),
            _ => None,
        }
    }
    /// Digest bound by the human's confirmation.
    pub fn approved_digest(&self) -> Option<&str> {
        match self {
            Self::HumanApproved(receipt) => Some(receipt.digest()),
            _ => None,
        }
    }
}
/// Guarded human approval; only the built-in terminal adapter is public.
pub mod approval {
    #[cfg(unix)]
    use std::io::{self, IsTerminal, Write};
    #[cfg(any(unix, test))]
    use std::path::Path;
    #[cfg(unix)]
    use std::process::Command;

    use serde::Serialize;

    use super::SpecApproval;

    #[cfg(unix)]
    struct Terminal;
    #[cfg(unix)]
    fn command_text(program: &str, args: &[&str]) -> Result<String, String> {
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "{program} could not verify approval identity or ancestry"
            ));
        }
        String::from_utf8(output.stdout).map_err(|e| e.to_string())
    }
    #[cfg(unix)]
    fn attempt_ancestor() -> Result<bool, String> {
        let processes = command_text("/bin/ps", &["-axo", "pid=,ppid=,comm="])?;
        let mut rows = std::collections::BTreeMap::new();
        for line in processes.lines() {
            let mut fields = line.split_whitespace();
            let Some(pid) = fields.next().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            let parent = fields
                .next()
                .and_then(|n| n.parse::<u32>().ok())
                .ok_or("invalid process ancestry")?;
            let program = line
                .trim_start()
                .split_once(char::is_whitespace)
                .and_then(|(_, rest)| rest.trim_start().split_once(char::is_whitespace))
                .map(|(_, program)| program.trim())
                .unwrap_or_default();
            rows.insert(pid, (parent, program));
        }
        kaisa_ancestor(&rows, std::process::id())
    }
    /// Every ancestor must be accounted for; absence is not evidence of a human shell.
    #[cfg(any(unix, test))]
    pub(crate) fn kaisa_ancestor(
        rows: &std::collections::BTreeMap<u32, (u32, &str)>,
        pid: u32,
    ) -> Result<bool, String> {
        let mut current = pid;
        let mut visited = std::collections::BTreeSet::new();
        while current != 0 {
            if !visited.insert(current) {
                return Err("cycle in process ancestry".into());
            }
            let (parent, program) = rows.get(&current).ok_or("cannot verify process ancestry")?;
            let name = Path::new(program)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if current != pid
                && (name.eq_ignore_ascii_case("kaisa") || name.eq_ignore_ascii_case("kaisa.exe"))
            {
                return Ok(true);
            }
            current = *parent;
        }
        Ok(false)
    }
    #[cfg(unix)]
    impl ApprovalTerminal for Terminal {
        fn both_terminals(&self) -> bool {
            io::stdin().is_terminal() && io::stdout().is_terminal()
        }
        fn inside_attempt(&self) -> Result<bool, String> {
            if std::env::var_os("KAISA_ATTEMPT").is_some() {
                return Ok(true);
            }
            attempt_ancestor()
        }
        fn os_user(&self) -> Result<String, String> {
            Ok(command_text("/usr/bin/id", &["-un"])?.trim().into())
        }
        fn read_confirmation(&mut self, digest: &str) -> Result<String, String> {
            print!("Specification digest: {digest}\nType its first 8 hex characters to approve: ");
            io::stdout().flush().map_err(|e| e.to_string())?;
            let mut answer = String::new();
            io::stdin()
                .read_line(&mut answer)
                .map_err(|e| e.to_string())?;
            Ok(answer)
        }
    }

    /// Confirms using only the built-in process terminal adapter. Windows is unverified
    /// and refuses human approval; policy-controlled provisional approval remains available.
    ///
    /// ```compile_fail,E0603
    /// use kaisa_domain::spec::{approval::{ApprovalTerminal, confirm}, SpecApproval};
    /// struct Forged;
    /// impl ApprovalTerminal for Forged {
    ///     fn both_terminals(&self) -> bool { true }
    ///     fn inside_attempt(&self) -> Result<bool, String> { Ok(false) }
    ///     fn os_user(&self) -> Result<String, String> { Ok("agent".into()) }
    ///     fn read_confirmation(&mut self, digest: &str) -> Result<String, String> {
    ///         Ok(digest[..8].into())
    ///     }
    /// }
    /// let approval = confirm(&mut Forged, &"a".repeat(64)).unwrap();
    /// assert!(matches!(approval, SpecApproval::HumanApproved(_)));
    /// ```
    pub fn confirm_on_process_terminal(_digest: &str) -> Result<SpecApproval, String> {
        #[cfg(not(unix))]
        return Err("human approval is unsupported on Windows and other unverified platforms; use policy-controlled --provisional".into());
        #[cfg(unix)]
        confirm(&mut Terminal, _digest)
    }
    /// Number of digest characters the person must enter.
    pub const CONFIRMATION_PREFIX_LEN: usize = 8;
    /// A receipt whose fields cannot be created or deserialized outside this module.
    ///
    /// ```compile_fail
    /// use kaisa_domain::spec::approval::HumanApproval;
    /// let forged = HumanApproval {
    ///     user: "agent".into(), method: "terminal-confirmation".into(),
    ///     at: 0, digest: "forged".into(),
    /// };
    /// ```
    #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
    pub struct HumanApproval {
        user: String,
        method: String,
        at: u64,
        digest: String,
    }
    impl HumanApproval {
        /// OS account that confirmed.
        pub fn user(&self) -> &str {
            &self.user
        }
        /// Exact approved digest.
        pub fn digest(&self) -> &str {
            &self.digest
        }
    }
    /// Trusted adapter for terminal presence, attempt ancestry and OS identity.
    /// External crates cannot implement the terminal boundary.
    /// See [`confirm_on_process_terminal`] for the external forgery regression.
    #[cfg(any(unix, test))]
    pub(crate) trait ApprovalTerminal {
        /// Both stdin and stdout must be actual terminals.
        fn both_terminals(&self) -> bool;
        /// Checks the attempt marker and ancestor process identities, failing closed on errors.
        fn inside_attempt(&self) -> Result<bool, String>;
        /// Gets the OS identity rather than a user-supplied environment variable.
        fn os_user(&self) -> Result<String, String>;
        /// Shows the digest and reads the person's confirmation.
        fn read_confirmation(&mut self, digest: &str) -> Result<String, String>;
    }
    /// Mints human approval only after terminal, ancestry and exact digest confirmation checks.
    #[cfg(any(unix, test))]
    pub(crate) fn confirm(
        terminal: &mut dyn ApprovalTerminal,
        digest: &str,
    ) -> Result<SpecApproval, String> {
        if !terminal.both_terminals() {
            return Err("human approval requires terminal stdin AND stdout".into());
        }
        if terminal.inside_attempt()? {
            return Err("human approval is forbidden inside a Kaisa attempt".into());
        }
        if digest.len() != crate::DIGEST_HEX_LEN
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid specification digest".into());
        }
        if !terminal
            .read_confirmation(digest)?
            .trim()
            .eq_ignore_ascii_case(&digest[..CONFIRMATION_PREFIX_LEN])
        {
            return Err("digest confirmation did not match".into());
        }
        let user = terminal.os_user()?;
        if user.trim().is_empty() {
            return Err("OS user is unavailable".into());
        }
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs();
        Ok(SpecApproval::HumanApproved(HumanApproval {
            user,
            method: "terminal-confirmation".into(),
            at,
            digest: digest.into(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::approval::{ApprovalTerminal, CONFIRMATION_PREFIX_LEN, confirm};
    use super::{
        AcceptanceCriterion, CriterionKind, SpecApproval, SpecFinding, SpecId, Specification,
        split_requirements,
    };

    const REQUIREMENTS: &str = "- preserve sessions\n\n  reject expired tokens";
    const COVERED: &str = "R1";
    const UNCOVERED: &str = "R2";
    fn session_spec() -> Specification {
        Specification {
            id: SpecId::try_new("spec_sessions").expect("valid identity"),
            version: 1,
            title: "Sessions".into(),
            requirements: split_requirements(REQUIREMENTS),
            criteria: vec![AcceptanceCriterion {
                id: "AC-1".into(),
                requirement_ids: vec![COVERED.into()],
                given: "a session".into(),
                when: "refreshed".into(),
                then: "preserved".into(),
                kind: CriterionKind::Normal,
                correction: false,
            }],
            ambiguities: Vec::new(),
            invariants: Vec::new(),
            error_behaviour: Vec::new(),
        }
    }
    #[test]
    fn a_requirement_without_a_criterion_is_reported_by_its_source_id() {
        assert_eq!(
            session_spec().validate(),
            vec![SpecFinding::MissingAcceptanceCriteria {
                requirement_ids: vec![UNCOVERED.into()]
            }],
            "missing acceptance criteria must identify the uncovered original line"
        );
    }
    #[test]
    fn splitting_requirements_preserves_verbatim_lines_and_source_order() {
        let lines: Vec<_> = REQUIREMENTS
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let requirements = split_requirements(REQUIREMENTS);
        assert_eq!(
            requirements
                .iter()
                .map(|r| r.text.as_str())
                .collect::<Vec<_>>(),
            lines,
            "bullets and indentation remain source evidence"
        );
        assert_eq!(
            requirements
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            vec![COVERED, UNCOVERED],
            "Kaisa assigns stable source-order identifiers"
        );
    }
    #[test]
    fn duplicate_identifiers_and_unknown_links_are_validation_findings() {
        let mut spec = session_spec();
        spec.criteria.push(spec.criteria[0].clone());
        spec.criteria[0].requirement_ids.push("R999".into());
        let findings = spec.validate();
        assert!(
            findings.contains(&SpecFinding::InvalidId("AC-1".into())),
            "duplicate criteria cannot masquerade as distinct evidence"
        );
        assert!(
            findings.contains(&SpecFinding::UnknownRequirement("R999".into())),
            "invented requirement references are refused"
        );
    }
    #[test]
    fn canonical_json_has_sorted_keys_and_changed_content_has_a_new_digest() {
        let original = session_spec();
        let text = String::from_utf8(original.canonical_json()).expect("JSON is UTF8");
        assert!(
            text.starts_with("{\"ambiguities\":"),
            "canonical encoding sorts object keys"
        );
        let mut changed = original.clone();
        changed.criteria[0].correction = true;
        assert_ne!(
            original.digest(),
            changed.digest(),
            "legacy correction permission is covered by the approved digest"
        );
        let roundtrip: Specification =
            serde_json::from_str(&text).expect("canonical content parses");
        assert_eq!(
            roundtrip.digest(),
            original.digest(),
            "canonical bytes are stable across serialization"
        );
    }
    #[test]
    fn specification_ids_cannot_escape_the_state_directory() {
        for id in ["../spec_escape", "spec_../../escape", "spec_", "spec_UPPER"] {
            assert!(
                SpecId::try_new(id).is_err(),
                "identity must be safe as a path segment: {id}"
            );
        }
    }
    struct PersonTerminal {
        stdin_tty: bool,
        stdout_tty: bool,
        agent: bool,
        ancestry_unknown: bool,
        answer: String,
    }
    impl PersonTerminal {
        fn confirming(digest: &str) -> Self {
            Self {
                stdin_tty: true,
                stdout_tty: true,
                agent: false,
                ancestry_unknown: false,
                answer: digest[..CONFIRMATION_PREFIX_LEN].into(),
            }
        }
    }
    impl ApprovalTerminal for PersonTerminal {
        fn both_terminals(&self) -> bool {
            self.stdin_tty && self.stdout_tty
        }
        fn inside_attempt(&self) -> Result<bool, String> {
            if self.ancestry_unknown {
                Err("ancestry unavailable".into())
            } else {
                Ok(self.agent)
            }
        }
        fn os_user(&self) -> Result<String, String> {
            Ok("session-owner".into())
        }
        fn read_confirmation(&mut self, _: &str) -> Result<String, String> {
            Ok(self.answer.clone())
        }
    }
    #[test]
    fn either_nonterminal_stream_refuses_human_approval() {
        let digest = session_spec().digest();
        for (stdin_tty, stdout_tty) in [(false, true), (true, false), (false, false)] {
            let mut terminal = PersonTerminal::confirming(&digest);
            terminal.stdin_tty = stdin_tty;
            terminal.stdout_tty = stdout_tty;
            assert!(
                confirm(&mut terminal, &digest).is_err(),
                "both actual terminal streams are required"
            );
        }
    }
    #[test]
    fn an_attempt_marker_or_unknown_ancestry_refuses_human_approval() {
        let digest = session_spec().digest();
        let mut terminal = PersonTerminal::confirming(&digest);
        terminal.agent = true;
        assert!(
            confirm(&mut terminal, &digest).is_err(),
            "an agent cannot use terminal access to approve its own specification"
        );
        terminal.agent = false;
        terminal.ancestry_unknown = true;
        assert!(
            confirm(&mut terminal, &digest).is_err(),
            "unavailable ancestry fails closed"
        );
    }
    #[test]
    fn a_wrong_digest_prefix_never_mints_human_approval() {
        let digest = session_spec().digest();
        let mut terminal = PersonTerminal::confirming(&digest);
        terminal.answer = "wrong-prefix".into();
        assert!(
            confirm(&mut terminal, &digest).is_err(),
            "only exact digest confirmation grants approval"
        );
    }
    #[test]
    fn a_terminal_confirmation_records_identity_method_time_and_exact_digest() {
        let digest = session_spec().digest();
        let receipt = confirm(&mut PersonTerminal::confirming(&digest), &digest)
            .expect("the person confirms");
        assert_eq!(
            receipt.approved_digest(),
            Some(digest.as_str()),
            "approval binds exact content"
        );
        assert_eq!(
            receipt.user(),
            Some("session-owner"),
            "the trusted OS identity is retained"
        );
        let value = serde_json::to_value(receipt).expect("receipt serializes");
        assert_eq!(
            value["method"], "terminal-confirmation",
            "terminal presence provenance is explicit"
        );
        assert!(
            value["at"].as_u64().is_some(),
            "approval includes its recorded time"
        );
    }
    #[test]
    fn provisional_permission_has_no_human_identity_or_approved_digest() {
        let approval = SpecApproval::Provisional {
            policy: "allow_provisional_spec".into(),
        };
        assert_eq!(
            approval.kind(),
            "provisional",
            "policy use is never labelled human-approved"
        );
        assert_eq!(
            (approval.user(), approval.approved_digest()),
            (None, None),
            "provisional permission cannot impersonate a human receipt"
        );
    }
    #[test]
    fn a_kaisa_ancestor_is_refused_even_after_descendants_scrub_the_attempt_marker() {
        const ROOT: u32 = 1;
        const KAISA: u32 = 20;
        const AGENT: u32 = 21;
        const APPROVER: u32 = 22;
        let mut rows = std::collections::BTreeMap::from([
            (ROOT, (0, "init")),
            (KAISA, (ROOT, "/Applications/Kaisa Tools/kaisa")),
            (AGENT, (KAISA, "kaisa-fake-agent")),
            (APPROVER, (AGENT, "kaisa")),
        ]);
        assert_eq!(
            super::approval::kaisa_ancestor(&rows, APPROVER),
            Ok(true),
            "a scrubbed environment cannot hide Kaisa in the ancestor chain"
        );
        rows.insert(KAISA, (ROOT, "user-shell"));
        assert_eq!(
            super::approval::kaisa_ancestor(&rows, APPROVER),
            Ok(false),
            "the current approval process itself is excluded from ancestor checks"
        );
        rows.remove(&KAISA);
        assert!(
            super::approval::kaisa_ancestor(&rows, APPROVER).is_err(),
            "incomplete ancestry is refused rather than assumed human"
        );
    }
    #[cfg(not(unix))]
    #[test]
    fn unverified_platforms_refuse_human_approval_without_running_path_tools() {
        let error = super::approval::confirm_on_process_terminal(&session_spec().digest())
            .expect_err("unverified platform cannot mint human approval");
        assert!(
            error.contains("unsupported") && error.contains("--provisional"),
            "diagnostic names the platform limit and provisional alternative"
        );
    }
}

/// Criterion mapping shared by deterministic lint, review and traceability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestMapping {
    /// Declared test name, optionally qualified with its file path.
    pub test_id: String,
    /// Frozen criterion identifiers covered by this test.
    pub criterion_ids: Vec<String>,
}

/// Content-addressed private test receipt, shared by QA and reporting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaManifestEntry {
    /// Stable private scenario identifier.
    pub test_id: String,
    /// Frozen criterion identifiers exercised by the scenario.
    pub criterion_ids: Vec<String>,
    /// File relative to the private suite directory.
    pub file: String,
    /// BLAKE3 digest of the private source.
    pub digest: String,
}
