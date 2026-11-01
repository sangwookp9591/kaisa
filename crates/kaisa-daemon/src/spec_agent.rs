//! Read-only specification synthesis; Kaisa owns source partitioning and validation.

use kaisa_domain::spec::{SpecFinding, split_requirements};
use kaisa_domain::{AcceptanceCriterion, Ambiguity, Role, SpecId, Specification, WriteSet};
use serde::Deserialize;

use crate::{GoalOutcome, GoalRequest, run_goal};

/// The only fields an agent is permitted to propose.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    title: String,
    criteria: Vec<AcceptanceCriterion>,
    ambiguities: Vec<Ambiguity>,
    invariants: Vec<String>,
    error_behaviour: Vec<String>,
}
/// Draft content, deterministic findings and the attempt's actual containment evidence.
#[derive(Debug)]
pub struct SpecDraft {
    /// Kaisa-bound specification, not yet approved.
    pub specification: Specification,
    /// Missing coverage and other validation findings.
    pub findings: Vec<SpecFinding>,
    /// Attempt evidence, including reduced containment when applicable.
    pub attempt: GoalOutcome,
}
/// Parses exactly one final, unquoted dedicated fence, ignoring fenced examples.
pub fn spec_in_answer(
    answer: &str,
    requirements: &str,
    id: SpecId,
) -> Result<Specification, String> {
    const OPEN: &str = "```kaisa-spec";
    let lines: Vec<_> = answer.trim_end().lines().collect();
    let mut starts = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let Some(marker) = trimmed.chars().next().filter(|c| matches!(c, '`' | '~')) else {
            continue;
        };
        let width = trimmed.chars().take_while(|c| *c == marker).count();
        if width < 3 {
            continue;
        }
        let tail = &trimmed[width..];
        if let Some((opened, length)) = fence {
            if marker == opened && width >= length && tail.trim().is_empty() {
                fence = None;
            }
        } else {
            if *line == OPEN {
                starts.push(index);
            }
            fence = Some((marker, width));
        }
    }
    let start = match starts.as_slice() {
        [] => return Err("missing unquoted kaisa-spec fence".into()),
        [start] => *start,
        _ => return Err("multiple kaisa-spec fences".into()),
    };
    let body = &lines[start + 1..];
    let end = body
        .iter()
        .position(|line| *line == "```")
        .ok_or("unfinished kaisa-spec fence")?;
    if end + 1 != body.len() {
        return Err("kaisa-spec fence is not final".into());
    }
    let proposal: Proposal = serde_json::from_str(&body[..end].join("\n"))
        .map_err(|e| format!("invalid kaisa-spec JSON: {e}"))?;
    let spec = Specification {
        id,
        version: 1,
        title: proposal.title,
        requirements: split_requirements(requirements),
        criteria: proposal.criteria,
        ambiguities: proposal.ambiguities,
        invariants: proposal.invariants,
        error_behaviour: proposal.error_behaviour,
    };
    if spec.requirements.is_empty() {
        return Err("requirements are empty".into());
    }
    if spec
        .validate()
        .iter()
        .any(|finding| !matches!(finding, SpecFinding::MissingAcceptanceCriteria { .. }))
    {
        return Err("invalid criterion identifiers or requirement links".into());
    }
    Ok(spec)
}
/// Runs the supplied launcher as a reviewer with an empty write set, using normal repository context.
/// Failed, truncated or tool-denied output never supplies a draft.
pub fn draft_spec(
    request: GoalRequest<'_>,
    requirements: &str,
    id: SpecId,
) -> Result<SpecDraft, String> {
    let source =
        serde_json::to_string(&split_requirements(requirements)).map_err(|e| e.to_string())?;
    let prompt = format!(
        "Draft a specification from these original requirements and the repository context. You are read-only. Kaisa owns the R identifiers and verbatim text: {source}\nPropose observable criteria (Normal, Edge, Error, Invariant), ambiguities, invariants and error behaviour. Mark correction=true only for explicit legacy behaviour changes. Do not claim approval. Finish with exactly one unquoted ```kaisa-spec JSON fence containing {{\"title\":string,\"criteria\":[{{\"id\":\"AC-1\",\"requirement_ids\":[\"R1\"],\"given\":string,\"when\":string,\"then\":string,\"kind\":\"Normal\",\"correction\":false}}],\"ambiguities\":[{{\"id\":\"A-1\",\"requirement_ids\":[\"R1\"],\"question\":string}}],\"invariants\":[],\"error_behaviour\":[]}}. Never invent, omit or reassign source requirements."
    );
    let write_set = WriteSet::default();
    let attempt = run_goal(&GoalRequest {
        goal: &prompt,
        role: Role::Reviewer,
        write_set: &write_set,
        ..request
    })
    .map_err(|e| e.to_string())?;
    if attempt.exit != Some(kaisa_sys::ProcessExit::Code(0))
        || attempt.truncated
        || !attempt.permission_denials.is_empty()
        || !attempt.kept_back.is_empty()
    {
        return Err(
            "specification attempt failed, was truncated, denied tools or changed protected paths"
                .into(),
        );
    }
    let transcript = std::fs::read_to_string(
        request
            .state_directory
            .join("attempts")
            .join(attempt.attempt.as_str())
            .join("transcript.log"),
    )
    .map_err(|e| e.to_string())?;
    let answer = kaisa_agents::final_answer(&transcript)
        .ok_or("specification attempt has no final answer")?;
    let specification = spec_in_answer(&answer, requirements, id)?;
    let findings = specification.validate();
    Ok(SpecDraft {
        specification,
        findings,
        attempt,
    })
}

#[cfg(test)]
mod tests {
    use kaisa_domain::SpecId;

    use super::spec_in_answer;

    const SOURCE: &str = "- preserve sessions\n- reject expired tokens";
    fn identity() -> SpecId {
        SpecId::try_new("spec_session").expect("valid specification identity")
    }
    fn proposal() -> String {
        serde_json::json!({"title":"Sessions", "criteria":[{"id":"AC-1", "requirement_ids":["R1"], "given":"a session", "when":"refreshed", "then":"preserved", "kind":"Normal", "correction":false}], "ambiguities":[], "invariants":[], "error_behaviour":[]}).to_string()
    }
    fn answer() -> String {
        format!("```kaisa-spec\n{}\n```", proposal())
    }
    #[test]
    fn a_final_specification_fence_keeps_kaisa_owned_requirements_and_missing_coverage() {
        let spec =
            spec_in_answer(&answer(), SOURCE, identity()).expect("one final dedicated proposal");
        assert_eq!(
            spec.requirements,
            kaisa_domain::spec::split_requirements(SOURCE),
            "agent output cannot paraphrase away the source"
        );
        assert_eq!(
            spec.validate(),
            vec![kaisa_domain::spec::SpecFinding::MissingAcceptanceCriteria {
                requirement_ids: vec!["R2".into()]
            }],
            "deterministic coverage identifies the omitted source requirement"
        );
    }
    #[test]
    fn missing_multiple_quoted_nested_unfinished_or_nonfinal_spec_fences_are_refused() {
        let answer = answer();
        let quoted = answer
            .lines()
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let indented = answer
            .lines()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        for invalid in [
            proposal(),
            format!("{answer}\n{answer}"),
            quoted,
            indented,
            format!("~~~~example\n{answer}\n~~~~"),
            format!("```kaisa-spec\n{}", proposal()),
            format!("{answer}\nI retract this."),
            format!("```kaisa-spec\n{} {{}}\n```", proposal()),
        ] {
            assert!(
                spec_in_answer(&invalid, SOURCE, identity()).is_err(),
                "only one whole final unquoted JSON region is authoritative: {invalid}"
            );
        }
    }
    #[test]
    fn agent_owned_requirements_or_unknown_requirement_links_are_refused() {
        let mut value: serde_json::Value =
            serde_json::from_str(&proposal()).expect("valid proposal");
        value["requirements"] = serde_json::json!([]);
        assert!(
            spec_in_answer(&format!("```kaisa-spec\n{value}\n```"), SOURCE, identity()).is_err(),
            "source requirement ownership is not delegated to the agent"
        );
        value
            .as_object_mut()
            .expect("object")
            .remove("requirements");
        value["criteria"][0]["requirement_ids"] = serde_json::json!(["R999"]);
        assert!(
            spec_in_answer(&format!("```kaisa-spec\n{value}\n```"), SOURCE, identity()).is_err(),
            "links to invented requirements are invalid"
        );
    }
}
