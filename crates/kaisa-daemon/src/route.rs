//! The router on the spawn path (T27c, ADR-0032): the cheapest catalog model at or above the
//! task's grade, at a fixed effort. A harder task gets a stronger model, never more effort on a
//! weaker one; every decision is recorded with its reasons as `route.decided`.

use kaisa_domain::{Catalog, Event, EventKind, Grade, GradeDecision, GradeReason, TaskId};
use kaisa_ledger::{EventStore, StoreError};

/// The effort every route uses. Escalation changes the model, not this (ADR-0032).
pub const ROUTE_EFFORT: &str = "medium";

/// Payload schema version of `route.decided` events.
const ROUTE_PAYLOAD_VERSION: u16 = 1;

/// Where a task's attempt runs, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision {
    /// The catalog agent.
    pub agent: String,
    /// The model the agent runs.
    pub model: String,
    /// The reasoning effort; always [`ROUTE_EFFORT`].
    pub effort: &'static str,
    /// The grade the task needed.
    pub grade: Grade,
    /// The model's own grade (at or above `grade`).
    pub model_grade: Grade,
    /// The grading rules that produced `grade`.
    pub reasons: Vec<GradeReason>,
}

/// The cheapest eligible model for `decision`, or `None` if the catalog has none at its grade
/// and floor: a task is never routed below what it needs.
#[must_use]
pub fn decide_route(catalog: &Catalog, decision: &GradeDecision) -> Option<RouteDecision> {
    let (agent, model) = catalog
        .models_at_or_above(decision.grade)
        .into_iter()
        .find(|(_, model)| {
            decision
                .floor
                .is_none_or(|floor| floor.admit(model.grade).is_ok())
        })?;
    Some(RouteDecision {
        agent: agent.name.clone(),
        model: model.id.clone(),
        effort: ROUTE_EFFORT,
        grade: decision.grade,
        model_grade: model.grade,
        reasons: decision.reasons.clone(),
    })
}

/// Records `route` for `task` as `route.decided`.
///
/// # Errors
///
/// Returns [`StoreError`] if the event cannot be appended.
pub fn record_route(
    ledger: &mut EventStore,
    task: &TaskId,
    route: &RouteDecision,
) -> Result<(), StoreError> {
    let payload = serde_json::json!({
        "agent": route.agent,
        "model": route.model,
        "effort": route.effort,
        "grade": format!("{:?}", route.grade),
        "model_grade": format!("{:?}", route.model_grade),
        "reasons": route.reasons.iter().map(|reason| format!("{reason:?}")).collect::<Vec<_>>(),
    });
    ledger.append(Event {
        kind: EventKind::RouteDecided,
        subject: task.as_str().to_owned(),
        payload_version: ROUTE_PAYLOAD_VERSION,
        payload: payload.to_string(),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{
        AgentEntry, AgentKind, Catalog, CatalogModel, Grade, PlannedTask, RepoPath, Role, TaskId,
        TaskSignals, WriteSet, grade_task,
    };

    use super::{ROUTE_EFFORT, decide_route, record_route};

    /// Failures that make the grader escalate a revision.
    const REPEATED_FAILURES: u32 = kaisa_domain::REVISION_FAILURES_THRESHOLD;

    fn a_catalog_of_three_strengths() -> Catalog {
        let mut catalog = Catalog::new();
        catalog
            .add(AgentEntry {
                name: "claude".to_owned(),
                kind: AgentKind::CliAgent {
                    program: "claude".to_owned(),
                },
                models: vec![
                    CatalogModel {
                        id: "haiku".to_owned(),
                        grade: Grade::T1,
                    },
                    CatalogModel {
                        id: "sonnet".to_owned(),
                        grade: Grade::T2,
                    },
                    CatalogModel {
                        id: "opus".to_owned(),
                        grade: Grade::T3,
                    },
                ],
                personal: false,
            })
            .expect("a valid built-in entry");
        catalog
    }

    fn a_login_fix() -> PlannedTask {
        PlannedTask {
            id: TaskId::try_new("task_login").expect("valid"),
            role: Role::Developer,
            write_set: WriteSet::new(vec![RepoPath::try_new("src/auth").expect("valid")]),
            depends_on: Vec::new(),
        }
    }

    fn signals(prior_failures: u32) -> TaskSignals {
        TaskSignals {
            paths_in_write_set: 1,
            impacted_files: 1,
            touches_security_paths: false,
            is_revision: prior_failures > 0,
            prior_failures,
        }
    }

    #[test]
    fn a_hard_task_escalates_to_a_stronger_model_not_more_effort() {
        let catalog = a_catalog_of_three_strengths();

        let first = decide_route(&catalog, &grade_task(&a_login_fix(), &signals(0)))
            .expect("a light model exists");
        let after_failures = decide_route(
            &catalog,
            &grade_task(&a_login_fix(), &signals(REPEATED_FAILURES)),
        )
        .expect("a stronger model exists");

        assert!(
            after_failures.model_grade > first.model_grade,
            "repeated failures move the task to a stronger model: {first:?} -> {after_failures:?}"
        );
        assert_eq!(
            (first.effort, after_failures.effort),
            (ROUTE_EFFORT, ROUTE_EFFORT),
            "effort stays the same; the model is what escalates"
        );
    }

    #[test]
    fn a_task_is_never_routed_below_what_it_needs() {
        let mut catalog = Catalog::new();
        catalog
            .add(AgentEntry {
                name: "local".to_owned(),
                kind: AgentKind::OpenAiCompatible {
                    endpoint: "http://127.0.0.1:11434/v1".to_owned(),
                },
                models: vec![CatalogModel {
                    id: "small".to_owned(),
                    grade: Grade::T1,
                }],
                personal: true,
            })
            .expect("a valid personal entry");
        let security_review = PlannedTask {
            role: Role::Security,
            ..a_login_fix()
        };

        let route = decide_route(&catalog, &grade_task(&security_review, &signals(0)));

        assert_eq!(
            route, None,
            "with no model strong enough, there is no route rather than a weaker one"
        );
    }

    #[test]
    fn the_route_decision_is_recorded_with_its_reasons() {
        let directory = std::env::temp_dir().join(format!("kaisa-route-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("writable temp dir");
        let mut ledger =
            kaisa_ledger::EventStore::open(&directory.join("kaisa.db")).expect("opens");
        let route = decide_route(
            &a_catalog_of_three_strengths(),
            &grade_task(&a_login_fix(), &signals(0)),
        )
        .expect("routable");

        record_route(&mut ledger, &a_login_fix().id, &route).expect("recorded");
        let recorded = ledger
            .read_from(kaisa_domain::Sequence::FIRST)
            .expect("readable");
        let _ = std::fs::remove_dir_all(&directory);

        let event = &recorded
            .last()
            .expect("the decision was appended")
            .recorded
            .event;
        assert!(
            event.kind == kaisa_domain::EventKind::RouteDecided
                && event.payload.contains(&route.model)
                && event.payload.contains("RoleBaseline"),
            "the ledger says which model was chosen and why: {}",
            event.payload
        );
    }
}
