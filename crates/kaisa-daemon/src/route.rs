//! Explainable spawn routing (T27c, ADR-0032): measured cost per delivered result within
//! grade and floor constraints, with the original catalog rule at cold start. Model escalation
//! precedes a constrained step to high effort; every choice is recorded as `route.decided`.

use kaisa_domain::{Catalog, Event, EventKind, Grade, GradeDecision, GradeReason, TaskId};
use kaisa_ledger::{EventStore, StoreError};

/// Cold-start and ordinary measured effort. Escalation tries the next model before high.
pub const ROUTE_EFFORT: &str = "medium";

/// Payload schema version of `route.decided` events.
const ROUTE_PAYLOAD_VERSION: u16 = 2;

/// Where a task's attempt runs, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision {
    /// How the choice was made.
    pub strategy: &'static str,
    /// Grade plus task kind used to join measured outcomes.
    pub task_class: String,
    /// Stable task identity across revisions.
    pub routing_task: String,
    /// Every candidate and the constraint that excludes it.
    pub candidates: Vec<serde_json::Value>,
    /// Effective policy and cost comparison unit for replay.
    pub policy: serde_json::Value,
    /// The catalog agent.
    pub agent: String,
    /// The model the agent runs.
    pub model: String,
    /// Medium, or one measured escalation step to high; never max.
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
        strategy: "cold_start",
        task_class: task_class(decision, "unknown"),
        routing_task: String::new(),
        candidates: catalog.entries().iter().flat_map(|agent| agent.models.iter().map(move |model| serde_json::json!({"agent":agent.name, "model":model.id, "effort":ROUTE_EFFORT, "stats":null, "excluded_by": if model.grade < decision.grade { "below_grade" } else if decision.floor.is_some_and(|f| f.admit(model.grade).is_err()) { "below_floor" } else { "insufficient_samples" }}))).collect(),
        policy: serde_json::Value::Null,
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
        "strategy": route.strategy,
        "policy": route.policy,
        "task_class": route.task_class,
        "routing_task": route.routing_task,
        "candidates": route.candidates,
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

/// Measured route inputs. Explicit pins are selected only as requested; an ineligible pin
/// returns no route rather than silently substituting an automatic choice.
pub struct RoutingRequest<'a> {
    /// Stable task identity and exploration seed input.
    pub task: &'a str,
    /// Deterministic grader output.
    pub decision: &'a GradeDecision,
    /// Existing role/kind, without invented language or size buckets.
    pub kind: &'a str,
    /// Security-sensitive tasks never explore.
    pub security_sensitive: bool,
    /// Explicit agent/model selection, if any.
    pub pinned: Option<(&'a str, &'a str)>,
}

fn task_class(decision: &GradeDecision, kind: &str) -> String {
    format!("{:?}:{kind}", decision.grade)
}

/// Chooses using complete measured outcomes, retaining the old rule at cold start.
/// Unknown cost is excluded from measured minimization. USD and tokens are never compared:
/// USD wins as the comparison unit when any feasible USD cell exists, otherwise tokens.
#[must_use]
pub fn decide_measured_route(
    catalog: &Catalog,
    request: &RoutingRequest<'_>,
    policy: &kaisa_domain::RoutingPolicy,
    observations: &[kaisa_ledger::RoutingObservation],
) -> Option<RouteDecision> {
    use serde_json::json;

    use crate::route_stats::routing_cells;

    let mut route = decide_route(catalog, request.decision)?;
    route.task_class = task_class(request.decision, request.kind);
    route.routing_task = request.task.to_owned();
    route.candidates.clear();
    route.policy = json!({"window":policy.window, "min_samples":policy.min_samples, "target_delivery_bps":policy.target_delivery_bps, "exploration_bps":policy.exploration_bps, "latency_budget_ms":policy.latency_budget_ms, "seed":policy.seed});
    let cells = routing_cells(observations, policy.window);
    let eligible = catalog
        .models_at_or_above(request.decision.grade)
        .into_iter()
        .filter(|(_, m)| {
            request
                .decision
                .floor
                .is_none_or(|floor| floor.admit(m.grade).is_ok())
        })
        .collect::<Vec<_>>();
    let prior_failure = observations.iter().rev().find(|o| {
        o.task == request.task
            && o.delivered == Some(false)
            && matches!(
                o.route["strategy"].as_str(),
                Some("measured" | "escalated" | "explore")
            )
    });
    let mut choices = Vec::new();
    for (agent, model) in catalog
        .entries()
        .iter()
        .flat_map(|a| a.models.iter().map(move |m| (a, m)))
    {
        for effort in [ROUTE_EFFORT, "high"] {
            let cell = cells.iter().find(|c| {
                c.agent == agent.name
                    && c.model == model.id
                    && c.effort == effort
                    && c.task_class == route.task_class
            });
            let eligible_index = eligible
                .iter()
                .position(|(a, m)| a.name == agent.name && m.id == model.id);
            let exclusion = if model.grade < request.decision.grade {
                Some("below_grade")
            } else if request
                .decision
                .floor
                .is_some_and(|f| f.admit(model.grade).is_err())
            {
                Some("below_floor")
            } else if request
                .pinned
                .is_some_and(|(a, m)| a != agent.name || m != model.id)
            {
                Some("user_pin")
            } else if cell.is_none_or(|c| c.samples < policy.min_samples) {
                Some("insufficient_samples")
            } else if cell
                .is_some_and(|c| c.delivered_rate < f64::from(policy.target_delivery_bps) / 10000.0)
            {
                Some("delivered_rate")
            } else if policy
                .latency_budget_ms
                .is_some_and(|budget| cell.is_none_or(|c| c.p90_ms.is_none_or(|v| v > budget)))
            {
                Some("latency_budget_or_unknown")
            } else if cell.is_none_or(|c| c.mean_cost_usd.is_none() && c.mean_tokens.is_none()) {
                Some("unknown_cost")
            } else {
                None
            };
            route.candidates.push(json!({"agent":agent.name, "model":model.id, "effort":effort,
                "model_grade":format!("{:?}",model.grade), "stats":cell.map(|c| c.evidence()), "excluded_by":exclusion}));
            choices.push((agent, model, effort, cell, eligible_index, exclusion));
        }
    }
    if let Some((agent, model)) = request.pinned {
        let (a, m) = eligible
            .iter()
            .find(|(a, m)| a.name == agent && m.id == model)?;
        route.agent.clone_from(&a.name);
        route.model.clone_from(&m.id);
        route.model_grade = m.grade;
        route.policy["user_pinned"] = json!(true);
        for candidate in &mut route.candidates {
            candidate["selected"] = json!(
                candidate["agent"] == a.name
                    && candidate["model"] == m.id
                    && candidate["effort"] == ROUTE_EFFORT
            );
        }
        return Some(route);
    }
    let measured = choices.iter().any(|c| {
        c.2 == ROUTE_EFFORT && c.4.is_some() && c.3.is_some_and(|s| s.samples >= policy.min_samples)
    });
    // Security roles are protected here even when a caller omitted sensitivity signals.
    let security = request.kind.eq_ignore_ascii_case("Security")
        || request.kind.eq_ignore_ascii_case("security-review")
        || request.security_sensitive
        || request
            .decision
            .reasons
            .iter()
            .any(|r| matches!(r, GradeReason::SecurityFloor | GradeReason::SecurityPaths));
    let hash = blake3::hash(format!("{}:{}", policy.seed, request.task).as_bytes());
    let draw = u64::from_le_bytes(hash.as_bytes()[..8].try_into().expect("eight hash bytes"));
    let under_sampled = choices
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            c.2 == ROUTE_EFFORT
                && c.4.is_some()
                && c.3.is_none_or(|s| s.samples < policy.min_samples)
        })
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    let explored = if measured
        && prior_failure.is_none()
        && !security
        && draw % 10000 < u64::from(policy.exploration_bps)
        && !under_sampled.is_empty()
    {
        Some(under_sampled[(draw / 10000 % under_sampled.len() as u64) as usize])
    } else {
        None
    };
    let feasible = choices
        .iter()
        .enumerate()
        .filter(|(_, c)| c.2 == ROUTE_EFFORT && c.5.is_none())
        .collect::<Vec<_>>();
    let usd = feasible
        .iter()
        .any(|(_, c)| c.3.is_some_and(|s| s.expected_cost(true).is_some()));
    route.policy["cost_unit"] = json!(if usd { "USD" } else { "tokens" });
    let best = feasible
        .iter()
        .filter_map(|(i, c)| Some((*i, c.3?.expected_cost(usd)?)))
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(i, _)| i);
    let chosen = if prior_failure.is_some() || (measured && explored.is_none() && best.is_none()) {
        route.strategy = "escalated";
        let prior_index = prior_failure.and_then(|previous| {
            eligible.iter().position(|(a, m)| {
                previous.route["agent"] == a.name && previous.route["model"] == m.id
            })
        });
        let base_index = prior_index.unwrap_or(0);
        let next_index = if prior_failure.is_some() && prior_index.is_none() {
            0
        } else {
            base_index + 1
        };
        let next = choices
            .iter()
            .position(|c| c.2 == ROUTE_EFFORT && c.4 == Some(next_index));
        let high = choices.iter().position(|c| {
            c.2 == "high"
                && (prior_failure.is_none() || prior_index.is_some())
                && c.4 == Some(base_index)
                && c.5.is_none()
                && prior_failure.is_none_or(|p| p.route["effort"] != "high")
        });
        // Model first. High is allowed only when no stronger option exists, or measured
        // evidence shows its feasible result costs less than the next model at medium.
        let high_better = high.zip(next).is_some_and(|(h, n)| {
            let higher = choices[h].3;
            let stronger = choices[n].3;
            stronger.is_some_and(|s| {
                s.samples >= policy.min_samples
                    && (choices[n].5.is_some()
                        || higher
                            .and_then(|h| h.expected_cost(usd))
                            .zip(s.expected_cost(usd))
                            .is_some_and(|(h, n)| h < n))
            })
        });
        if next.is_none() || high_better {
            high.or(next)?
        } else {
            next?
        }
    } else if let Some(index) = explored {
        route.strategy = "explore";
        index
    } else if let Some(index) = best {
        route.strategy = "measured";
        index
    } else {
        route.strategy = "cold_start";
        choices
            .iter()
            .position(|c| c.2 == ROUTE_EFFORT && c.4 == Some(0))?
    };
    let (agent, model, effort, _, _, _) = choices[chosen];
    route.agent.clone_from(&agent.name);
    route.model.clone_from(&model.id);
    route.effort = effort;
    route.model_grade = model.grade;
    // Explain policy gating even for otherwise feasible but unselected high-effort cells.
    for (index, candidate) in route.candidates.iter_mut().enumerate() {
        candidate["selected"] = json!(index == chosen);
        if index != chosen && candidate["excluded_by"].is_null() {
            candidate["excluded_by"] = json!(if candidate["effort"] == "high" {
                "model_before_effort"
            } else {
                "higher_expected_cost_or_escalation"
            });
        }
    }
    Some(route)
}

pub(crate) fn routing_task_id(run: &str, role: &str) -> String {
    let digest = blake3::hash(run.as_bytes()).to_hex();
    format!("task_{}{role}", &digest[..16])
}

/// Prepares a production route from the current ledger. Callers opt in only for unpinned
/// launches; the supplied catalog must match the launch adapter.
pub(crate) fn route_for_attempt(
    ledger: &EventStore,
    catalog: Option<&Catalog>,
    policy: &kaisa_domain::RoutingPolicy,
    task: &str,
    role: kaisa_domain::Role,
    write_set: &kaisa_domain::WriteSet,
    prior_failures: u32,
) -> Result<Option<RouteDecision>, crate::GoalError> {
    let Some(catalog) = catalog else {
        return Ok(None);
    };
    let security = write_set.entries().iter().any(|p| {
        p.as_str()
            .split('/')
            .any(|part| matches!(part, "auth" | "security" | "crypto" | "credentials"))
    });
    let planned = kaisa_domain::PlannedTask {
        id: TaskId::try_new(task).map_err(crate::goal::at("naming the routing task"))?,
        role,
        write_set: write_set.clone(),
        depends_on: Vec::new(),
    };
    let decision = kaisa_domain::grade_task(
        &planned,
        &kaisa_domain::TaskSignals {
            paths_in_write_set: write_set.entries().len(),
            impacted_files: write_set.entries().len(),
            touches_security_paths: security,
            is_revision: prior_failures > 0,
            prior_failures,
        },
    );
    let observations = ledger
        .routing_observations()
        .map_err(crate::goal::at("reading routing outcomes"))?;
    decide_measured_route(
        catalog,
        &RoutingRequest {
            task,
            decision: &decision,
            kind: &format!("{role:?}"),
            security_sensitive: security,
            pinned: None,
        },
        policy,
        &observations,
    )
    .map(Some)
    .ok_or_else(|| crate::GoalError {
        step: "routing the attempt",
        detail: "no eligible route remains at or above the required grade and floor".to_owned(),
    })
}

/// Applies the selected model and effort to the known Claude launch adapter.
pub(crate) fn apply_route(
    spec: &mut kaisa_sys::ProcessSpec,
    route: &RouteDecision,
) -> Result<(), crate::GoalError> {
    let invalid = || crate::GoalError {
        step: "applying the route",
        detail: "the routed launch must expose --model and --effort values".to_owned(),
    };
    let model_index = spec
        .args
        .iter()
        .position(|arg| arg == "--model")
        .filter(|i| *i + 1 < spec.args.len())
        .ok_or_else(invalid)?;
    let effort_index = spec
        .args
        .iter()
        .position(|arg| arg == "--effort")
        .filter(|i| *i + 1 < spec.args.len())
        .ok_or_else(invalid)?;
    spec.args[model_index + 1].clone_from(&route.model);
    spec.args[effort_index + 1] = route.effort.to_owned();
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

    pub(super) fn a_catalog_of_three_strengths() -> Catalog {
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

    pub(super) fn a_login_fix() -> PlannedTask {
        PlannedTask {
            id: TaskId::try_new("task_login").expect("valid"),
            role: Role::Developer,
            write_set: WriteSet::new(vec![RepoPath::try_new("src/auth").expect("valid")]),
            depends_on: Vec::new(),
        }
    }

    pub(super) fn signals(prior_failures: u32) -> TaskSignals {
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

#[cfg(test)]
mod measured_tests {
    use kaisa_domain::{GradeFloor, RoutingPolicy};

    use super::tests::{a_catalog_of_three_strengths, a_login_fix, signals};
    use super::{RoutingRequest, decide_measured_route, decide_route, record_route};
    use crate::route_stats::tests::{
        ATTEMPT_MS, CHEAP_USD, HISTORY, STRONG_USD, outcome, reliable_history,
    };

    fn policy_without_exploration() -> RoutingPolicy {
        RoutingPolicy {
            exploration_bps: 0,
            ..RoutingPolicy::default()
        }
    }

    fn choose(
        history: &[kaisa_ledger::RoutingObservation],
        policy: &RoutingPolicy,
        security: bool,
    ) -> super::RouteDecision {
        let decision = kaisa_domain::grade_task(&a_login_fix(), &signals(0));
        decide_measured_route(
            &a_catalog_of_three_strengths(),
            &RoutingRequest {
                task: "task_routing",
                decision: &decision,
                kind: "Developer",
                security_sensitive: security,
                pinned: None,
            },
            policy,
            history,
        )
        .expect("eligible routes")
    }

    #[test]
    fn security_role_never_explores_even_without_caller_signals() {
        let decision = kaisa_domain::GradeDecision {
            grade: kaisa_domain::Grade::T1,
            floor: None,
            reasons: Vec::new(),
        };
        let mut history = reliable_history();
        for row in &mut history {
            row.route["task_class"] = serde_json::json!("T1:Security");
        }
        let route = decide_measured_route(
            &a_catalog_of_three_strengths(),
            &RoutingRequest {
                task: "task_security_review",
                decision: &decision,
                kind: "Security",
                security_sensitive: false,
                pinned: None,
            },
            &RoutingPolicy {
                exploration_bps: 10000,
                ..RoutingPolicy::default()
            },
            &history,
        )
        .expect("eligible security route");
        assert_ne!(
            route.strategy, "explore",
            "the decision boundary protects security roles independently of caller signals"
        );
    }

    #[test]
    fn an_empty_history_keeps_the_old_cold_start_model_and_effort() {
        let decision = kaisa_domain::grade_task(&a_login_fix(), &signals(0));
        let old = decide_route(&a_catalog_of_three_strengths(), &decision).expect("old route");
        let cold = choose(&[], &RoutingPolicy::default(), false);
        assert_eq!(
            (cold.agent, cold.model, cold.effort),
            (old.agent, old.model, old.effort),
            "cold start preserves catalog ordering exactly"
        );
        assert_eq!(
            cold.strategy, "cold_start",
            "no measured accuracy is invented"
        );
    }

    #[test]
    fn a_cheaper_reliable_model_wins_and_a_cheaper_unreliable_model_loses() {
        let reliable = reliable_history();
        let selected = choose(&reliable, &policy_without_exploration(), false);
        assert_eq!(
            (selected.model.as_str(), selected.strategy),
            ("haiku", "measured"),
            "expected cost rewards the cheaper delivered result"
        );
        let mut unreliable = reliable;
        for observation in unreliable
            .iter_mut()
            .filter(|o| o.route["model"] == "haiku")
        {
            observation.delivered = Some(false);
            observation.failed = true;
        }
        let selected = choose(&unreliable, &policy_without_exploration(), false);
        assert_eq!(
            selected.model, "sonnet",
            "a cheap failed attempt cannot beat the delivery constraint"
        );
        assert!(
            selected
                .candidates
                .iter()
                .any(|c| c["model"] == "haiku" && c["excluded_by"] == "delivered_rate"),
            "the rejected candidate states its violated constraint"
        );
    }

    #[test]
    fn an_unknown_cost_is_not_cheaper_than_a_complete_usd_receipt() {
        let mut history = reliable_history();
        for observation in history.iter_mut().filter(|o| o.route["model"] == "haiku") {
            observation.usage = None;
        }
        let selected = choose(&history, &policy_without_exploration(), false);
        assert_eq!(
            selected.model, "sonnet",
            "unreported cost is excluded from measured minimization"
        );
    }

    #[test]
    fn a_latency_budget_excludes_slow_or_unmeasured_candidates() {
        let mut history = reliable_history();
        for observation in history.iter_mut().filter(|o| o.route["model"] == "haiku") {
            observation.latency_ms = None;
        }
        let policy = RoutingPolicy {
            latency_budget_ms: Some(ATTEMPT_MS),
            ..policy_without_exploration()
        };
        assert_eq!(
            choose(&history, &policy, false).model,
            "sonnet",
            "missing latency cannot satisfy a hard budget"
        );
    }

    #[test]
    fn exploration_replays_the_seed_and_never_runs_for_security_or_below_grade() {
        let history = reliable_history();
        let policy = RoutingPolicy {
            exploration_bps: 10000,
            ..RoutingPolicy::default()
        };
        let first = choose(&history, &policy, false);
        assert_eq!(
            first,
            choose(&history, &policy, false),
            "the task and seed reproduce the same exploration decision"
        );
        assert_eq!(
            (first.model.as_str(), first.strategy),
            ("opus", "explore"),
            "only the under-sampled eligible option is explored"
        );
        let secure = choose(&history, &policy, true);
        assert_ne!(
            secure.strategy, "explore",
            "security-sensitive work never explores even at full epsilon"
        );
        let decision = kaisa_domain::GradeDecision {
            grade: kaisa_domain::Grade::T2,
            floor: Some(GradeFloor::at(kaisa_domain::Grade::T3)),
            reasons: Vec::new(),
        };
        let selected = decide_measured_route(
            &a_catalog_of_three_strengths(),
            &RoutingRequest {
                task: "task_floor",
                decision: &decision,
                kind: "Developer",
                security_sensitive: false,
                pinned: None,
            },
            &policy,
            &history,
        )
        .expect("top grade exists");
        assert_eq!(
            selected.model_grade,
            kaisa_domain::Grade::T3,
            "both grade and the stricter declared floor apply during exploration and cold start"
        );
    }

    #[test]
    fn a_failed_measured_task_escalates_its_next_attempt_before_exploring() {
        let mut history = reliable_history();
        history.push(outcome("routing", "haiku", false, Some(CHEAP_USD)));
        let route = choose(
            &history,
            &RoutingPolicy {
                exploration_bps: 10000,
                ..RoutingPolicy::default()
            },
            false,
        );
        assert_eq!(
            (route.model.as_str(), route.effort, route.strategy),
            ("sonnet", "medium", "escalated"),
            "failed measured verification forces the next model before effort or exploration"
        );
    }

    #[test]
    fn a_user_pin_wins_over_measured_cost_and_exploration() {
        let decision = kaisa_domain::grade_task(&a_login_fix(), &signals(0));
        let selected = decide_measured_route(
            &a_catalog_of_three_strengths(),
            &RoutingRequest {
                task: "task_pin",
                decision: &decision,
                kind: "Developer",
                security_sensitive: false,
                pinned: Some(("claude", "opus")),
            },
            &RoutingPolicy {
                exploration_bps: 10000,
                ..RoutingPolicy::default()
            },
            &reliable_history(),
        )
        .expect("eligible pin");
        assert_eq!(
            selected.model, "opus",
            "an explicit model cannot be silently replaced with a cheaper measured option"
        );
    }

    #[test]
    fn an_exhausted_delivery_constraint_escalates_model_before_measured_high_effort() {
        let mut history = reliable_history();
        for observation in &mut history {
            observation.delivered = Some(false);
        }
        let route = choose(&history, &policy_without_exploration(), false);
        assert_eq!(
            (route.model.as_str(), route.effort, route.strategy),
            ("sonnet", "medium", "escalated"),
            "no qualifying medium candidate escalates to the next catalog model"
        );
        let mut high = (0..HISTORY)
            .map(|i| {
                let mut o = outcome(&format!("high_{i}"), "haiku", true, Some(STRONG_USD));
                o.route["effort"] = serde_json::json!("high");
                o
            })
            .collect::<Vec<_>>();
        history.append(&mut high);
        let route = choose(&history, &policy_without_exploration(), false);
        assert_eq!(
            route.effort, "high",
            "high is allowed only after measured stronger-medium failure or absence"
        );
    }

    #[test]
    fn a_recorded_measured_route_contains_the_candidate_table_and_strategy() {
        let directory =
            std::env::temp_dir().join(format!("kaisa-measured-route-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("temp directory");
        let database = directory.join("ledger.db");
        let _ = std::fs::remove_file(&database);
        let mut ledger = kaisa_ledger::EventStore::open(&database).expect("ledger");
        let route = choose(&reliable_history(), &policy_without_exploration(), false);
        record_route(&mut ledger, &a_login_fix().id, &route).expect("recorded route");
        let explanation = ledger
            .route_explanation("last")
            .expect("readable")
            .expect("recorded");
        assert_eq!(
            explanation["strategy"], "measured",
            "the strategy is persisted rather than recomputed for explain"
        );
        assert!(
            explanation["candidates"]
                .as_array()
                .is_some_and(|c| !c.is_empty()),
            "every candidate and its constraints remain inspectable"
        );
        drop(ledger);
        let _ = std::fs::remove_dir_all(directory);
    }
}

#[cfg(test)]
mod launch_tests {
    use super::tests::{a_catalog_of_three_strengths, a_login_fix, signals};
    use super::{apply_route, decide_route};

    #[test]
    fn the_spawn_adapter_uses_the_selected_model_and_effort_and_refuses_missing_flags() {
        const OPTION_AND_VALUE: usize = 2;
        let mut route = decide_route(
            &a_catalog_of_three_strengths(),
            &kaisa_domain::grade_task(&a_login_fix(), &signals(0)),
        )
        .expect("routable");
        route.model = "opus".to_owned();
        route.effort = "high";
        let mut spec = kaisa_sys::ProcessSpec {
            program: "claude".into(),
            args: vec![
                "--model".to_owned(),
                "haiku".to_owned(),
                "--effort".to_owned(),
                "medium".to_owned(),
            ],
            working_directory: std::env::temp_dir(),
            env: Vec::new(),
        };
        apply_route(&mut spec, &route).expect("known adapter");
        assert!(
            spec.args
                .windows(OPTION_AND_VALUE)
                .any(|pair| pair == ["--model", "opus"])
                && spec
                    .args
                    .windows(OPTION_AND_VALUE)
                    .any(|pair| pair == ["--effort", "high"]),
            "the process receives the exact recorded route"
        );
        spec.args.clear();
        assert!(
            apply_route(&mut spec, &route).is_err(),
            "an incompatible launch cannot silently ignore its recorded model"
        );
    }
}

#[cfg(test)]
mod invariant_tests {
    use kaisa_domain::{Grade, GradeDecision, RoutingPolicy};

    use super::tests::{a_catalog_of_three_strengths, a_login_fix, signals};
    use super::{RoutingRequest, decide_measured_route};
    use crate::route_stats::tests::reliable_history;

    #[test]
    fn a_grade_requirement_rejects_weaker_models_even_without_a_floor_or_an_eligible_pin() {
        let decision = GradeDecision {
            grade: Grade::T2,
            floor: None,
            reasons: Vec::new(),
        };
        let catalog = a_catalog_of_three_strengths();
        let mut request = RoutingRequest {
            task: "task_grade",
            decision: &decision,
            kind: "Developer",
            security_sensitive: false,
            pinned: None,
        };
        let route = decide_measured_route(
            &catalog,
            &request,
            &RoutingPolicy::default(),
            &reliable_history(),
        )
        .expect("strong models available");
        assert!(
            route.model_grade >= decision.grade,
            "the grade applies independently of a declared floor"
        );
        request.pinned = Some(("claude", "haiku"));
        assert!(
            decide_measured_route(
                &catalog,
                &request,
                &RoutingPolicy::default(),
                &reliable_history()
            )
            .is_none(),
            "an ineligible pin is refused instead of silently replaced or admitted below grade"
        );
    }

    #[test]
    fn multiple_under_sampled_options_replay_each_seed_and_receive_exploration() {
        let history: Vec<_> = reliable_history()
            .into_iter()
            .filter(|o| o.route["model"] == "haiku")
            .collect();
        let decision = kaisa_domain::grade_task(&a_login_fix(), &signals(0));
        let request = RoutingRequest {
            task: "task_seed",
            decision: &decision,
            kind: "Developer",
            security_sensitive: false,
            pinned: None,
        };
        let catalog = a_catalog_of_three_strengths();
        let seed_trials = 64;
        let mut explored = std::collections::BTreeSet::new();
        for seed in 0..seed_trials {
            let policy = RoutingPolicy {
                seed,
                exploration_bps: 10000,
                ..RoutingPolicy::default()
            };
            let first =
                decide_measured_route(&catalog, &request, &policy, &history).expect("explore");
            assert_eq!(
                Some(first.clone()),
                decide_measured_route(&catalog, &request, &policy, &history),
                "each seed replays the same route and explanation"
            );
            explored.insert(first.model);
        }
        assert_eq!(
            explored,
            ["sonnet".to_owned(), "opus".to_owned()]
                .into_iter()
                .collect(),
            "exploration can reach either eligible under-sampled medium cell"
        );
    }
    #[test]
    fn a_higher_required_grade_uses_the_first_stronger_model_after_a_failed_lower_grade() {
        let decision = GradeDecision {
            grade: Grade::T2,
            floor: None,
            reasons: Vec::new(),
        };
        let mut history = reliable_history();
        let mut failed = crate::route_stats::tests::outcome(
            "highergrade",
            "haiku",
            false,
            Some(crate::route_stats::tests::CHEAP_USD),
        );
        failed.task = "task_highergrade".to_owned();
        history.push(failed);
        let route = decide_measured_route(
            &a_catalog_of_three_strengths(),
            &RoutingRequest {
                task: "task_highergrade",
                decision: &decision,
                kind: "Developer",
                security_sensitive: false,
                pinned: None,
            },
            &RoutingPolicy {
                exploration_bps: 0,
                ..RoutingPolicy::default()
            },
            &history,
        )
        .expect("next grade available");
        assert_eq!(
            (route.model.as_str(), route.strategy),
            ("sonnet", "escalated"),
            "a stronger grade already excludes the failed model, so escalation need not skip another model"
        );
    }
}
