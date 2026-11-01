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
    /// Whether approval was followed by a freeze receipt.
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
    /// Saves canonical bytes as a new version; existing versions are never overwritten.
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
        // The blob address and version alias both contain exactly the hashed canonical bytes.
        let blobs = self.state.join("specs/objects");
        std::fs::create_dir_all(&blobs)?;
        let blob = blobs.join(format!("{digest}.json"));
        match OpenOptions::new().create_new(true).write(true).open(&blob) {
            Ok(mut file) => {
                file.write_all(&bytes)?;
                file.sync_all()?;
                let mut permissions = file.metadata()?.permissions();
                permissions.set_readonly(true);
                file.set_permissions(permissions)?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if std::fs::read(&blob)? != bytes {
                    return Err(invalid(
                        "content-addressed specification blob digest mismatch",
                    ));
                }
            }
            Err(error) => return Err(error),
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        let payload = Payload {
            digest: digest.clone(),
            version: specification.version,
            approval_kind: "provisional".into(),
            user: None,
            approval: None,
        };
        Self::record(
            &mut ledger,
            EventKind::SpecDrafted,
            &specification.id,
            &payload,
        )?;
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
        let bytes = std::fs::read(self.path(id, version))?;
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
                    approval = payload.approval.clone();
                }
                EventKind::SpecFrozen => {
                    if approval.is_none() || payload.approval != approval {
                        return Err(invalid("freeze without matching approval"));
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
        let record = ApprovalRecord {
            kind: approval.kind().into(),
            user: approval.user().map(str::to_owned),
            receipt: serde_json::to_value(approval).map_err(|e| invalid(e.to_string()))?,
        };
        let payload = Payload {
            digest: current.digest,
            version,
            approval_kind: record.kind.clone(),
            user: record.user.clone(),
            approval: Some(record),
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
}
