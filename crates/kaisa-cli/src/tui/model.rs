//! A read-only view of recorded verification runs and attempts. Links come only from events.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use kaisa_domain::{EventKind, Sequence};
use kaisa_ledger::{AttemptRow, AttemptUsage, EventStore};
use serde_json::Value;

use super::messages::{Message, format as message, text};

/// Limit each transcript read, including during live refresh.
const TRANSCRIPT_BYTES: u64 = 16 * 1024;
const VERDICT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Default)]
pub(super) struct Snapshot {
    pub runs: Vec<RunCard>,
}

#[derive(Default)]
pub(super) struct RunCard {
    pub id: String,
    pub goal: String,
    pub decision: String,
    pub reason: String,
    pub attempts: Vec<Attempt>,
    pub evidence: Vec<Value>,
    pub reviews: Vec<Value>,
    pub candidate_attempt: Option<String>,
    pub result_branch: Option<String>,
    pub repository: Option<String>,
    pub order: u64,
    pub waiting_order: u64,
}

pub(super) struct Attempt {
    pub row: AttemptRow,
    pub role: String,
    pub containment: String,
    pub usage: Option<AttemptUsage>,
    pub tail: String,
    pub findings: Vec<String>,
}

impl RunCard {
    pub(super) fn needs_person(&self) -> bool {
        self.decision == "NeedsPerson"
    }

    pub(super) fn failed(&self) -> bool {
        matches!(self.decision.as_str(), "Refuse" | "revision_exhausted")
            || self.attempts.iter().any(|attempt| {
                matches!(attempt.row.execution, kaisa_domain::Execution::Exited(exit) if !exit.is_success())
            })
    }

    pub(super) fn status(&self) -> &str {
        if self.decision.is_empty() {
            text(Message::GateUnknown)
        } else {
            &self.decision
        }
    }
}

/// Never creates directories, opens a writer, rebuilds projections, or migrates a database.
pub(super) fn load(state: &Path) -> Result<Snapshot, Box<dyn std::error::Error>> {
    let database = state.join("kaisa.db");
    if !database.exists() {
        return Ok(Snapshot::default());
    }
    let ledger = EventStore::open_read_only(&database)?;
    // Read the rows before their metadata: append-only events then include every admission
    // seen by the row fold, even when another process admits an attempt during this refresh.
    let rows = ledger.attempts_from_log()?;
    let events = ledger.read_from(Sequence::FIRST)?;
    let mut runs: BTreeMap<String, RunCard> = BTreeMap::new();
    let mut links: BTreeMap<String, String> = BTreeMap::new();
    let mut admissions: BTreeMap<String, Value> = BTreeMap::new();
    let mut review_trees: BTreeMap<String, String> = BTreeMap::new();
    for chained in &events {
        let event = &chained.recorded.event;
        let payload: Value = serde_json::from_str(&event.payload)?;
        if event.kind == EventKind::AttemptAdmitted {
            admissions.insert(event.subject.clone(), payload.clone());
        }
        let Some(run_id) = payload["run_id"].as_str() else {
            continue;
        };
        let run = runs.entry(run_id.to_owned()).or_insert_with(|| RunCard {
            id: run_id.to_owned(),
            order: chained.recorded.sequence.value(),
            ..RunCard::default()
        });
        if let Some(repository) = payload["repository"].as_str() {
            run.repository = Some(repository.to_owned());
        }
        match event.kind {
            EventKind::MissionCreated => {
                run.goal = payload["goal"].as_str().unwrap_or_default().to_owned();
            }
            EventKind::CandidateFrozen => {
                run.candidate_attempt = payload["developer_attempt"]
                    .as_str()
                    .and_then(|attempt| kaisa_domain::AttemptId::try_new(attempt).ok())
                    .map(|attempt| attempt.as_str().to_owned());
            }
            EventKind::CheckEvidence => run.evidence.push(payload.clone()),
            EventKind::ReviewVerdict => {
                run.reviews.push(payload.clone());
                if let (Some(attempt), Some(tree)) = (
                    payload["reviewer_attempt"].as_str(),
                    payload["candidate_tree"].as_str(),
                ) {
                    review_trees.insert(attempt.to_owned(), tree.to_owned());
                }
            }
            EventKind::GateDecided => {
                run.decision = payload["decision"].as_str().unwrap_or_default().to_owned();
                run.reason = payload["reason"].as_str().unwrap_or_default().to_owned();
                run.result_branch = payload["result_branch"]
                    .as_str()
                    .filter(|branch| {
                        run.decision == "Deliver"
                            && branch
                                .strip_prefix("kaisa/result/")
                                .is_some_and(|id| kaisa_domain::RunId::try_new(id).is_ok())
                    })
                    .map(str::to_owned);
                if run.needs_person() {
                    run.waiting_order = chained.recorded.sequence.value();
                }
            }
            _ => {}
        }
        for field in ["developer_attempt", "reviewer_attempt", "attempt"] {
            if let Some(attempt) = payload[field].as_str() {
                links.insert(attempt.to_owned(), run_id.to_owned());
            }
        }
    }
    // Admissions, not last state changes, determine the attempt order.
    let admission_order: BTreeMap<&str, u64> = events
        .iter()
        .filter(|event| event.recorded.event.kind == EventKind::AttemptAdmitted)
        .map(|event| {
            (
                event.recorded.event.subject.as_str(),
                event.recorded.sequence.value(),
            )
        })
        .collect();
    for row in rows {
        let id = row.attempt.as_str();
        let directory = state.join("attempts").join(id);
        let role = kaisa_mcp::BridgeConfig::load(&directory)
            .map(|config| config.role.code().to_owned())
            .unwrap_or_else(|_| text(Message::RoleUnknown).to_owned());
        let admission = &admissions[id]["containment"];
        let missing = admission["missing"].as_array();
        let containment = if admission["admission"] == "reduced" {
            message(
                Message::Reduced,
                &[
                    &admission["basis"].as_str().unwrap_or("unknown"),
                    &missing
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", "),
                ],
            )
        } else if admission["admission"] == "full" {
            text(Message::Confined).to_owned()
        } else {
            message(Message::Containment, &[&admission["admission"]])
        };
        let usage = ledger.usage_for(&row.attempt)?;
        let goal = row
            .context_pack
            .as_ref()
            .map(|digest| ledger.context_pack(digest))
            .transpose()?
            .flatten()
            .and_then(|pack| spec(&pack))
            .unwrap_or_else(|| text(Message::GoalUnknown).to_owned());
        let tail = transcript_tail(&directory.join("transcript.log"))
            .unwrap_or_else(|_| text(Message::TranscriptUnreadable).to_owned());
        let findings = review_trees
            .get(id)
            .and_then(|tree| review_findings(&directory.join("transcript.log"), tree))
            .unwrap_or_default();
        let order = admission_order.get(id).copied().unwrap_or_default();
        let run_id = links
            .get(id)
            .cloned()
            .unwrap_or_else(|| format!("unlinked:{id}"));
        let run = runs.entry(run_id.clone()).or_insert_with(|| RunCard {
            id: run_id,
            goal: goal.clone(),
            order,
            reason: text(Message::Unlinked).to_owned(),
            ..RunCard::default()
        });
        run.attempts.push(Attempt {
            row,
            role,
            containment,
            usage,
            tail,
            findings,
        });
    }
    let mut runs: Vec<RunCard> = runs.into_values().collect();
    for run in &mut runs {
        run.attempts.sort_by_key(|attempt| {
            admission_order
                .get(attempt.row.attempt.as_str())
                .copied()
                .unwrap_or_default()
        });
        let mut developers = 0;
        for attempt in &mut run.attempts {
            if attempt.role == "developer" {
                developers += 1;
                if developers > 1 {
                    attempt.role = message(Message::Revision, &[&(developers - 1)]);
                }
            }
        }
    }
    runs.sort_by_key(|run| {
        (
            !run.needs_person(),
            if run.needs_person() {
                run.waiting_order
            } else {
                run.order
            },
        )
    });
    Ok(Snapshot { runs })
}

/// Only interpret the two documented leading fields of the context-pack v1 encoding.
fn spec(bytes: &[u8]) -> Option<String> {
    fn field(bytes: &mut &[u8]) -> Option<Vec<u8>> {
        let (length, rest) = bytes.split_at_checked(std::mem::size_of::<u64>())?;
        let length = usize::try_from(u64::from_be_bytes(length.try_into().ok()?)).ok()?;
        let (value, rest) = rest.split_at_checked(length)?;
        *bytes = rest;
        Some(value.to_vec())
    }
    let mut bytes = bytes.strip_prefix(b"kaisa-context-pack v1\n")?;
    if field(&mut bytes)? != b"spec" {
        return None;
    }
    String::from_utf8(field(&mut bytes)?).ok()
}

fn review_findings(path: &Path, tree: &str) -> Option<Vec<String>> {
    let file = File::open(path).ok()?;
    if file.metadata().ok()?.len() > VERDICT_BYTES {
        return None;
    }
    let mut text = String::new();
    file.take(VERDICT_BYTES).read_to_string(&mut text).ok()?;
    let answer = kaisa_agents::final_answer(&text)?;
    let verdict = kaisa_daemon::verdict_in_answer(&answer, tree).ok()?;
    Some(
        verdict
            .findings
            .iter()
            .map(|finding| {
                format!(
                    "{:?}: {}:{} — {}",
                    finding.severity,
                    finding.path.as_str(),
                    finding
                        .line
                        .map(|line| line.to_string())
                        .unwrap_or_default(),
                    finding.message
                )
            })
            .collect(),
    )
}

fn transcript_tail(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let start = size.saturating_sub(TRANSCRIPT_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(TRANSCRIPT_BYTES).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if start > 0 {
        text.split_once('\n')
            .map_or(text.as_ref(), |(_, tail)| tail)
    } else {
        &text
    };
    let lines: Vec<&str> = text.lines().collect();
    Ok(lines[lines.len().saturating_sub(8)..].join("\n"))
}

/// Strip control characters from untrusted ledger/transcript text before either renderer.
pub(super) fn safe(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect()
}

pub(super) fn usage_text(attempt: &Attempt) -> String {
    attempt.usage.as_ref().map_or_else(
        || text(Message::UsageUnknown).to_owned(),
        |usage| {
            let cost = usage
                .cost_usd
                .map(|cost| format!("${cost:.4}"))
                .unwrap_or_else(|| text(Message::ProviderNoCost).to_owned());
            message(
                Message::Usage,
                &[&usage.input_tokens, &usage.output_tokens, &cost],
            )
        },
    )
}

pub(super) fn details(run: &RunCard, selected_attempt: Option<usize>) -> String {
    let mut output = message(
        Message::GoalDetails,
        &[&run.goal, &run.id, &run.status(), &run.reason],
    );
    if let Some(repository) = &run.repository {
        let identity: Option<Value> = serde_json::from_str(repository).ok();
        let root = identity
            .as_ref()
            .and_then(|identity| identity["root"].as_str())
            .unwrap_or(repository);
        output.push_str(&message(Message::Repository, &[&root]));
    }
    if let Some(branch) = &run.result_branch {
        output.push_str(&format!("Result: {branch}\ngit merge --ff-only {branch}\n"));
    }
    if let Some(attempt) = &run.candidate_attempt {
        output.push_str(&message(Message::Branch, &[&attempt]));
        if run.decision == "Deliver" {
            output.push_str(&format!("git merge --ff-only kaisa/{attempt}\n"));
        } else {
            output.push_str(text(Message::NoMerge));
        }
    }
    for review in &run.reviews {
        output.push_str(&message(
            Message::Review,
            &[
                &review["reviewer_attempt"].as_str().unwrap_or("unknown"),
                &review["valid"],
                &review["approve"],
                &review["finding_count"],
            ],
        ));
    }
    for evidence in &run.evidence {
        // Preserve argv boundaries: a check command is evidence, not an invented shell line.
        let command = evidence["command"].to_string();
        output.push_str(&message(
            Message::Check,
            &[
                &command,
                &evidence["exit"],
                &evidence["candidate_tree"].as_str().unwrap_or("unknown"),
            ],
        ));
    }
    let selected: BTreeSet<usize> = selected_attempt
        .map(|index| BTreeSet::from([index]))
        .unwrap_or_else(|| (0..run.attempts.len()).collect());
    for (index, attempt) in run.attempts.iter().enumerate() {
        // Findings remain visible even when the developer row is selected.
        for finding in &attempt.findings {
            output.push_str(&message(Message::Finding, &[&finding]));
        }
        if !selected.contains(&index) {
            continue;
        }
        output.push_str(&message(
            Message::AttemptDetails,
            &[
                &attempt.role,
                &attempt.row.attempt.as_str(),
                &attempt.row.task.as_str(),
                &attempt.row.execution.code(),
                &attempt.row.cleanup.code(),
                &usage_text(attempt),
                &attempt.containment,
                &attempt.tail,
            ],
        ));
    }
    safe(&output)
}
