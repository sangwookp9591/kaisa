//! Specification versions, content-addressed bytes and ledger-backed freeze receipts.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use kaisa_domain::{Event, EventKind, Sequence, SpecApproval, SpecId, Specification};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::EventStore;

/// Approval evidence read from the ledger, distinct from the authority to mint approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    /// `provisional` or `human-approved`.
    pub kind: String,
    /// OS account, absent for provisional permission.
    pub user: Option<String>,
    /// Full receipt, including policy or terminal method, time and digest.
    pub receipt: Value,
}
/// One exact version and its verified ledger provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSpec {
    /// The content whose bytes were re-hashed on read.
    pub specification: Specification,
    /// BLAKE3 content address recorded by Kaisa.
    pub digest: String,
    /// Approval for this version only, absent on a fresh version.
    pub approval: Option<ApprovalRecord>,
    /// Matching approval/freeze receipts, digest-verified bytes and a read-only file.
    pub frozen: bool,
}
/// Filesystem and event-store boundary, serialized by a process-safe file lock.
pub struct SpecStore {
    state: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Payload {
    digest: String,
    version: u32,
    approval_kind: String,
    user: Option<String>,
    approval: Option<ApprovalRecord>,
    #[serde(default)]
    attempts_unconfined_since_draft: bool,
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
fn ledger_error(error: super::StoreError) -> io::Error {
    invalid(error.to_string())
}
impl SpecStore {
    /// Uses an explicit state directory; does not touch it until an operation needs it.
    pub fn new(state: &Path) -> Self {
        Self {
            state: state.into(),
        }
    }
    fn lock(&self) -> io::Result<File> {
        std::fs::create_dir_all(self.state.join("specs"))?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.state.join("specs/.lock"))?;
        file.lock()?;
        Ok(file)
    }
    fn ledger(&self) -> io::Result<EventStore> {
        EventStore::open(&self.state.join("kaisa.db")).map_err(ledger_error)
    }
    fn path(&self, id: &SpecId, version: u32) -> PathBuf {
        self.state
            .join("specs")
            .join(id.as_str())
            .join(format!("v{version}.json"))
    }
    fn events(&self, ledger: &EventStore, id: &SpecId) -> io::Result<Vec<(EventKind, Payload)>> {
        ledger.verify().map_err(ledger_error)?;
        ledger
            .read_from(Sequence::FIRST)
            .map_err(ledger_error)?
            .into_iter()
            .filter(|e| {
                e.recorded.event.subject == id.as_str()
                    && matches!(
                        e.recorded.event.kind,
                        EventKind::SpecDrafted | EventKind::SpecApproved | EventKind::SpecFrozen
                    )
            })
            .map(|e| {
                let event = e.recorded.event;
                Ok((
                    event.kind,
                    serde_json::from_str(&event.payload).map_err(|e| invalid(e.to_string()))?,
                ))
            })
            .collect()
    }
    /// Saves canonical bytes as a new version; ledger-recorded versions are never overwritten.
    /// Missing acceptance criteria are allowed in drafts and reported by domain validation.
    pub fn draft(&self, mut specification: Specification) -> io::Result<StoredSpec> {
        let _lock = self.lock()?;
        let mut ledger = self.ledger()?;
        if specification.requirements.is_empty() {
            return Err(invalid("specification has no requirements"));
        }
        if specification.validate().iter().any(|f| {
            !matches!(
                f,
                kaisa_domain::spec::SpecFinding::MissingAcceptanceCriteria { .. }
            )
        }) {
            return Err(invalid("specification has invalid identifiers or links"));
        }
        let previous = self.events(&ledger, &specification.id)?;
        specification.version = previous
            .iter()
            .filter(|(kind, _)| *kind == EventKind::SpecDrafted)
            .map(|(_, p)| p.version)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| invalid("specification version exhausted"))?;
        let digest = specification.digest();
        let bytes = specification.canonical_json();
        let path = self.path(&specification.id, specification.version);
        std::fs::create_dir_all(path.parent().expect("specification has a directory"))?;
        // Publish the alias only after the ledger append. A pre-append crash leaves
        // a replaceable temporary file; an orphan alias is never version authority.
        let temporary = path.with_extension("json.tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        let payload = Payload {
            digest: digest.clone(),
            version: specification.version,
            approval_kind: "unapproved".into(),
            user: None,
            approval: None,
            attempts_unconfined_since_draft: false,
        };
        Self::record(
            &mut ledger,
            EventKind::SpecDrafted,
            &specification.id,
            &payload,
        )?;
        if path.exists() {
            eprintln!(
                "warning: replacing orphan specification file {}",
                path.display()
            );
            std::fs::remove_file(&path)?;
        }
        std::fs::rename(&temporary, &path)?;
        Ok(StoredSpec {
            specification,
            digest,
            approval: None,
            frozen: false,
        })
    }
    fn record(
        ledger: &mut EventStore,
        kind: EventKind,
        id: &SpecId,
        payload: &Payload,
    ) -> io::Result<()> {
        ledger
            .append(Event {
                kind,
                subject: id.to_string(),
                payload_version: 1,
                payload: serde_json::to_string(payload).map_err(|e| invalid(e.to_string()))?,
            })
            .map_err(ledger_error)?;
        Ok(())
    }
    fn read_from(
        &self,
        ledger: &EventStore,
        id: &SpecId,
        version: Option<u32>,
    ) -> io::Result<StoredSpec> {
        let events = self.events(ledger, id)?;
        let version = version
            .or_else(|| {
                events
                    .iter()
                    .filter(|(kind, _)| *kind == EventKind::SpecDrafted)
                    .map(|(_, p)| p.version)
                    .max()
            })
            .ok_or_else(|| invalid("unknown specification"))?;
        let draft = events
            .iter()
            .find(|(kind, p)| *kind == EventKind::SpecDrafted && p.version == version)
            .ok_or_else(|| invalid("unknown specification version"))?;
        let path = self.path(id, version);
        let bytes = std::fs::read(&path)
            .map_err(|e| invalid(format!("{id} v{version} ({}): {e}", path.display())))?;
        if blake3::hash(&bytes).to_hex().as_str() != draft.1.digest {
            return Err(invalid(
                "specification digest mismatch: stored bytes changed",
            ));
        }
        let specification: Specification =
            serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
        if specification.id != *id
            || specification.version != version
            || specification.canonical_json() != bytes
        {
            return Err(invalid("noncanonical or misbound specification"));
        }
        let mut approval = None;
        let mut frozen = false;
        for (kind, payload) in &events {
            if payload.version != version {
                continue;
            }
            if payload.digest != draft.1.digest {
                return Err(invalid("specification receipt digest mismatch"));
            }
            match kind {
                EventKind::SpecApproved => {
                    Self::verify_receipt(payload)?;
                    approval = payload.approval.clone();
                }
                EventKind::SpecFrozen => {
                    if approval.is_none() || payload.approval != approval {
                        return Err(invalid("freeze without matching approval"));
                    }
                    Self::verify_receipt(payload)?;
                    if !std::fs::metadata(&path)?.permissions().readonly() {
                        return Err(invalid("frozen specification file is not read-only"));
                    }
                    frozen = true;
                }
                _ => {}
            }
        }
        Ok(StoredSpec {
            specification,
            digest: draft.1.digest.clone(),
            approval,
            frozen,
        })
    }
    fn verify_receipt(payload: &Payload) -> io::Result<()> {
        let record = payload
            .approval
            .as_ref()
            .ok_or_else(|| invalid("missing approval receipt"))?;
        let receipt = &record.receipt;
        let common = record.kind == payload.approval_kind
            && record.user == payload.user
            && receipt["kind"].as_str() == Some(record.kind.as_str());
        let valid = match record.kind.as_str() {
            "human-approved" => {
                common
                    && record.user.as_ref().is_some_and(|user| {
                        !user.trim().is_empty() && receipt["user"].as_str() == Some(user.as_str())
                    })
                    && receipt["method"] == "terminal-confirmation"
                    && receipt["digest"].as_str() == Some(payload.digest.as_str())
                    && receipt["at"].as_u64().is_some_and(|at| at > 0)
                    && receipt["attempts_unconfined_since_draft"].as_bool()
                        == Some(payload.attempts_unconfined_since_draft)
            }
            "provisional" => {
                common
                    && record.user.is_none()
                    && receipt["policy"]
                        .as_str()
                        .is_some_and(|policy| !policy.trim().is_empty())
            }
            _ => false,
        };
        if !valid {
            return Err(invalid("invalid or inconsistent approval receipt"));
        }
        Ok(())
    }
    /// Serializes attempt admission with the final human approval check.
    /// Hold this guard through the admission ledger append, then release it before launch.
    pub fn attempt_admission_guard(&self) -> io::Result<File> {
        self.lock()
    }

    fn refuse_live_attempts(&self, ledger: &EventStore) -> io::Result<()> {
        for row in ledger.attempts_from_log().map_err(ledger_error)? {
            // A completed execution no longer owns a live attempt, even if its CLI remains alive.
            if matches!(row.execution, kaisa_domain::Execution::Exited(_))
                && matches!(
                    row.cleanup,
                    kaisa_domain::Cleanup::Reclaimed | kaisa_domain::Cleanup::Retained
                )
            {
                continue;
            }
            #[cfg(windows)]
            let parent = "owners";
            #[cfg(not(windows))]
            let parent = "attempts";
            let owner = self
                .state
                .join(parent)
                .join(row.attempt.as_str())
                .join("owner.identity");
            let agent = self
                .state
                .join("attempts")
                .join(row.attempt.as_str())
                .join("agent.identity");
            let mut owner_dead = false;
            for (index, path) in [owner, agent].iter().enumerate() {
                match std::fs::read_to_string(path) {
                    Ok(line) => {
                        let identity = kaisa_sys::ProcessIdentity::from_line(&line)
                            .ok_or_else(|| invalid("cannot verify live Kaisa attempt identity"))?;
                        if identity.is_running()? {
                            return Err(invalid(format!(
                                "human approval refused: live Kaisa attempt {}",
                                row.attempt
                            )));
                        }
                        if index == 0 {
                            owner_dead = true;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound && index == 1 && owner_dead => {}
                    Err(e) => {
                        return Err(invalid(format!(
                            "cannot exclude live Kaisa attempt {}: {e}",
                            row.attempt
                        )));
                    }
                }
            }
        }
        Ok(())
    }
    /// Refuses human confirmation while a recorded attempt is live or cannot be inspected.
    pub fn check_human_approval_available(&self) -> io::Result<()> {
        let _lock = self.lock()?;
        self.refuse_live_attempts(&self.ledger()?)
    }
    fn unconfined_since_draft(
        &self,
        ledger: &EventStore,
        id: &SpecId,
        version: u32,
    ) -> io::Result<bool> {
        let events = ledger.read_from(Sequence::FIRST).map_err(ledger_error)?;
        let drafted = events
            .iter()
            .find(|event| {
                event.recorded.event.kind == EventKind::SpecDrafted
                    && event.recorded.event.subject == id.as_str()
                    && serde_json::from_str::<Payload>(&event.recorded.event.payload)
                        .is_ok_and(|p| p.version == version)
            })
            .ok_or_else(|| invalid("missing draft event"))?
            .recorded
            .sequence;
        let mut reduced = std::collections::BTreeSet::new();
        let mut running = std::collections::BTreeSet::new();
        for event in &events {
            let recorded = &event.recorded;
            let payload: Value =
                serde_json::from_str(&recorded.event.payload).unwrap_or(Value::Null);
            if recorded.event.kind == EventKind::AttemptAdmitted {
                // Missing containment evidence is reduced, never silently full assurance.
                if payload["containment"]["admission"] != "full" {
                    reduced.insert(recorded.event.subject.clone());
                }
            }
            if recorded.event.kind == EventKind::AttemptStateChanged {
                match payload["execution"].as_str() {
                    Some("starting" | "running") => {
                        running.insert(recorded.event.subject.clone());
                    }
                    Some(code) if code.starts_with("exited:") => {
                        running.remove(&recorded.event.subject);
                    }
                    _ => {}
                }
            }
            // Include reduced attempts already running when this version was drafted.
            if recorded.sequence == drafted && !reduced.is_disjoint(&running) {
                return Ok(true);
            }
            if recorded.sequence > drafted
                && reduced.contains(&recorded.event.subject)
                && recorded.event.kind == EventKind::AttemptStateChanged
                && matches!(payload["execution"].as_str(), Some("starting" | "running"))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// Selects an approval version without silently choosing between pending versions.
    pub fn for_approval(&self, id: &SpecId, version: Option<u32>) -> io::Result<StoredSpec> {
        let _lock = self.lock()?;
        let ledger = self.ledger()?;
        if let Some(version) = version {
            return self.read_from(&ledger, id, Some(version));
        }
        let events = self.events(&ledger, id)?;
        let versions = events
            .iter()
            .filter(|(kind, _)| *kind == EventKind::SpecDrafted)
            .map(|(_, p)| p.version)
            .collect::<std::collections::BTreeSet<_>>();
        let mut pending = Vec::new();
        let mut approved = 0;
        for version in versions {
            let spec = self.read_from(&ledger, id, Some(version))?;
            if spec.approval.is_some() {
                approved = approved.max(version);
            } else {
                pending.push(spec);
            }
        }
        if pending.len() != 1 {
            return Err(invalid(
                "approval requires explicit --version when there is not exactly one unapproved version",
            ));
        }
        let spec = pending.pop().expect("one pending version");
        if spec.specification.version < approved {
            return Err(invalid(
                "approving an older version requires explicit --version",
            ));
        }
        Ok(spec)
    }
    /// Reads and verifies a version; omitted version selects the latest draft.
    pub fn read(&self, id: &SpecId, version: Option<u32>) -> io::Result<StoredSpec> {
        let _lock = self.lock()?;
        self.read_from(&self.ledger()?, id, version)
    }
    /// Latest versions of every ledger-recorded specification, verified on read.
    pub fn list(&self) -> io::Result<Vec<StoredSpec>> {
        let _lock = self.lock()?;
        let ledger = self.ledger()?;
        ledger.verify().map_err(ledger_error)?;
        let ids: std::collections::BTreeSet<_> = ledger
            .read_from(Sequence::FIRST)
            .map_err(ledger_error)?
            .into_iter()
            .filter(|e| e.recorded.event.kind == EventKind::SpecDrafted)
            .map(|e| e.recorded.event.subject)
            .collect();
        ids.into_iter()
            .map(|id| self.read_from(&ledger, &SpecId::try_new(&id).map_err(invalid)?, None))
            .collect()
    }
    /// Records approval bound to exact bytes, then makes this version read-only and freezes it.
    /// Policy is checked independently here, so a caller cannot authorize provisional use by label alone.
    pub fn approve(
        &self,
        id: &SpecId,
        version: u32,
        approval: &SpecApproval,
        policy: &kaisa_domain::Policy,
    ) -> io::Result<StoredSpec> {
        let _lock = self.lock()?;
        let mut ledger = self.ledger()?;
        let current = self.read_from(&ledger, id, Some(version))?;
        if current.frozen {
            return Err(invalid("specification version is already frozen"));
        }
        if !current.specification.validate().is_empty() {
            return Err(invalid("specification has unresolved validation findings"));
        }
        match approval {
            SpecApproval::Provisional { .. } if !policy.allow_provisional_spec => {
                return Err(invalid("allow_provisional_spec policy is false"));
            }
            SpecApproval::HumanApproved(_)
                if approval.approved_digest() != Some(current.digest.as_str()) =>
            {
                return Err(invalid("human approval digest does not match this version"));
            }
            _ => {}
        }
        if matches!(approval, SpecApproval::HumanApproved(_)) {
            self.refuse_live_attempts(&ledger)?;
        }
        // Admission shares our lock. Read history after the live check so an attempt
        // completing concurrently cannot be omitted from the assurance receipt.
        let reduced = self.unconfined_since_draft(&ledger, id, version)?;
        let mut receipt = serde_json::to_value(approval).map_err(|e| invalid(e.to_string()))?;
        receipt["attempts_unconfined_since_draft"] = Value::Bool(reduced);
        let record = ApprovalRecord {
            kind: approval.kind().into(),
            user: approval.user().map(str::to_owned),
            receipt,
        };
        let payload = Payload {
            digest: current.digest,
            version,
            approval_kind: record.kind.clone(),
            user: record.user.clone(),
            approval: Some(record),
            attempts_unconfined_since_draft: reduced,
        };
        Self::record(&mut ledger, EventKind::SpecApproved, id, &payload)?;
        let path = self.path(id, version);
        let mut permissions = std::fs::metadata(&path)?.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions)?;
        Self::record(&mut ledger, EventKind::SpecFrozen, id, &payload)?;
        self.read_from(&ledger, id, Some(version))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use kaisa_domain::{
        AcceptanceCriterion, CriterionKind, Policy, SpecApproval, SpecId, Specification,
    };

    use super::SpecStore;

    const NEXT_VERSION_STEP: u32 = 1;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Scenario {
        directory: std::path::PathBuf,
        store: SpecStore,
    }
    impl Scenario {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "kaisa-spec-store-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let store = SpecStore::new(&directory);
            Self { directory, store }
        }
        fn draft(&self) -> super::StoredSpec {
            self.store.draft(session_spec()).expect("draft recorded")
        }
        fn freeze(&self) -> super::StoredSpec {
            let draft = self.draft();
            self.store
                .approve(
                    &draft.specification.id,
                    draft.specification.version,
                    &provisional(),
                    &permissive_policy(),
                )
                .expect("policy explicitly allows provisional freeze")
        }
    }
    impl Drop for Scenario {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
    fn provisional() -> SpecApproval {
        SpecApproval::Provisional {
            policy: "allow_provisional_spec".into(),
        }
    }
    fn permissive_policy() -> Policy {
        let mut policy = Policy::personal_default();
        policy.allow_provisional_spec = true;
        policy
    }
    fn session_spec() -> Specification {
        Specification {
            id: SpecId::try_new("spec_session").expect("valid id"),
            version: 1,
            title: "Preserve sessions".into(),
            requirements: kaisa_domain::spec::split_requirements("preserve sessions"),
            criteria: vec![AcceptanceCriterion {
                id: "AC-1".into(),
                requirement_ids: vec!["R1".into()],
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
    fn a_provisional_freeze_is_recorded_and_never_claims_human_approval() {
        let scenario = Scenario::new();
        let frozen = scenario.freeze();
        let approval = frozen.approval.expect("freeze has policy receipt");
        assert_eq!(
            approval.kind, "provisional",
            "policy authorization is always labelled provisional"
        );
        assert_eq!(
            approval.user, None,
            "a provisional receipt names no confirming human"
        );
        assert!(frozen.frozen, "approval finishes with a freeze receipt");
        assert!(
            std::fs::metadata(
                scenario
                    .store
                    .path(&frozen.specification.id, frozen.specification.version)
            )
            .expect("file exists")
            .permissions()
            .readonly(),
            "frozen bytes are read-only"
        );
        let events = scenario
            .store
            .ledger()
            .expect("ledger opens")
            .read_from(kaisa_domain::Sequence::FIRST)
            .expect("events readable");
        let kinds: Vec<_> = events.iter().map(|e| e.recorded.event.kind).collect();
        assert_eq!(
            kinds,
            vec![
                kaisa_domain::EventKind::SpecDrafted,
                kaisa_domain::EventKind::SpecApproved,
                kaisa_domain::EventKind::SpecFrozen
            ],
            "draft approval and freeze each have durable evidence"
        );
    }
    #[test]
    fn default_policy_refuses_provisional_approval_without_recording_it() {
        let scenario = Scenario::new();
        let draft = scenario.draft();
        assert!(
            scenario
                .store
                .approve(
                    &draft.specification.id,
                    draft.specification.version,
                    &provisional(),
                    &Policy::personal_default()
                )
                .is_err(),
            "provisional use requires explicit policy"
        );
        let read = scenario
            .store
            .read(&draft.specification.id, None)
            .expect("draft intact");
        assert!(
            read.approval.is_none() && !read.frozen,
            "a denied policy cannot leave approval evidence"
        );
    }
    #[cfg(unix)]
    #[test]
    fn editing_frozen_specification_bytes_is_detected_as_a_digest_mismatch() {
        use std::os::unix::fs::PermissionsExt;
        let scenario = Scenario::new();
        let frozen = scenario.freeze();
        let path = scenario
            .store
            .path(&frozen.specification.id, frozen.specification.version);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("model same-user tampering");
        let mut changed = frozen.specification.clone();
        changed.title = "tampered title".into();
        std::fs::write(path, changed.canonical_json()).expect("tampered content written");
        let error = scenario
            .store
            .read(&frozen.specification.id, None)
            .expect_err("tampered freeze refused");
        assert!(
            error.to_string().contains("digest mismatch"),
            "failure specifically identifies changed approved bytes: {error}"
        );
    }
    #[test]
    fn a_changed_specification_creates_a_new_version_without_inheriting_approval() {
        let scenario = Scenario::new();
        let frozen = scenario.freeze();
        let mut changed = frozen.specification.clone();
        changed.title = "new session behaviour".into();
        let next = scenario
            .store
            .draft(changed)
            .expect("change creates a new version");
        assert_eq!(
            next.specification.version,
            frozen.specification.version + NEXT_VERSION_STEP,
            "versions only advance"
        );
        assert!(
            next.approval.is_none() && !next.frozen,
            "approval cannot carry over to changed content"
        );
        assert_ne!(
            next.digest, frozen.digest,
            "changed content has a new address"
        );
        let prior = scenario
            .store
            .read(&frozen.specification.id, Some(frozen.specification.version))
            .expect("prior freeze remains readable");
        assert_eq!(prior, frozen, "new drafts cannot overwrite frozen evidence");
    }
    #[test]
    fn missing_acceptance_criteria_remain_visible_in_drafts_and_prevent_freeze() {
        let scenario = Scenario::new();
        let mut spec = session_spec();
        spec.criteria.clear();
        let draft = scenario
            .store
            .draft(spec)
            .expect("incomplete draft is reviewable");
        assert!(
            !draft.specification.validate().is_empty(),
            "the missing requirement remains explicit"
        );
        assert!(
            scenario
                .store
                .approve(
                    &draft.specification.id,
                    draft.specification.version,
                    &provisional(),
                    &permissive_policy()
                )
                .is_err(),
            "approval cannot bless uncovered requirements"
        );
    }
    #[test]
    fn a_forged_human_approval_with_an_empty_receipt_is_invalid() {
        let scenario = Scenario::new();
        let draft = scenario.draft();
        let mut ledger = scenario.store.ledger().expect("ledger opens");
        let payload = super::Payload {
            digest: draft.digest,
            version: 1,
            approval_kind: "human-approved".into(),
            attempts_unconfined_since_draft: false,
            user: Some("forged".into()),
            approval: Some(super::ApprovalRecord {
                kind: "human-approved".into(),
                user: Some("forged".into()),
                receipt: serde_json::json!({}),
            }),
        };
        SpecStore::record(
            &mut ledger,
            kaisa_domain::EventKind::SpecApproved,
            &draft.specification.id,
            &payload,
        )
        .expect("model state-directory writer");
        assert!(
            scenario.store.read(&draft.specification.id, None).is_err(),
            "a ledger label with an empty receipt is never approval"
        );
    }
    #[test]
    fn an_orphan_version_file_does_not_block_the_next_draft() {
        let scenario = Scenario::new();
        let first = scenario.draft();
        std::fs::write(scenario.store.path(&first.specification.id, 2), b"orphan")
            .expect("model crash before ledger append");
        let next = scenario
            .store
            .draft(session_spec())
            .expect("orphan is replaceable");
        assert_eq!(next.specification.version, 2, "only ledger versions count");
        assert_eq!(
            scenario
                .store
                .read(&first.specification.id, None)
                .expect("canonical version"),
            next,
            "orphan bytes have been replaced by ledger-bound bytes"
        );
    }
    #[cfg(unix)]
    #[test]
    fn writable_frozen_bytes_are_invalid_even_when_the_digest_matches() {
        use std::os::unix::fs::PermissionsExt;
        let scenario = Scenario::new();
        let frozen = scenario.freeze();
        std::fs::set_permissions(
            scenario.store.path(&frozen.specification.id, 1),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("model permission tampering");
        assert!(
            scenario.store.read(&frozen.specification.id, None).is_err(),
            "frozen requires both digest verification and read-only bytes"
        );
    }
    #[test]
    fn approving_an_older_pending_version_requires_explicit_selection() {
        let scenario = Scenario::new();
        let first = scenario.draft();
        let second = scenario.draft();
        scenario
            .store
            .approve(
                &second.specification.id,
                2,
                &provisional(),
                &permissive_policy(),
            )
            .expect("explicit newer version approved");
        let error = scenario
            .store
            .for_approval(&first.specification.id, None)
            .expect_err("implicit rollback refused");
        assert!(
            error.to_string().contains("--version"),
            "diagnostic explains explicit override"
        );
        assert_eq!(
            scenario
                .store
                .for_approval(&first.specification.id, Some(1))
                .expect("explicit older version")
                .specification
                .version,
            1,
            "only the explicit version bypasses the rollback guard"
        );
    }
}
