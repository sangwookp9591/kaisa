//! Windowed task outcomes, derived only from attributable ledger receipts.

use std::collections::BTreeMap;

use kaisa_ledger::RoutingObservation;
use serde_json::{Value, json};

/// A measured cell: agent, model, effort, grade and existing role/kind.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingCell {
    /// Catalog agent name.
    pub agent: String,
    /// Exact catalog model identifier.
    pub model: String,
    /// Recorded effort.
    pub effort: String,
    /// Grade plus existing task role.
    pub task_class: String,
    /// Completed task episodes in the window.
    pub samples: usize,
    /// Attempts including revisions in those episodes.
    pub attempts: usize,
    /// Delivered without any failed verification or revision.
    pub first_pass_rate: f64,
    /// Episodes finally delivered by this cell.
    pub delivered_rate: f64,
    /// Attempts which failed verification, execution or delivery.
    pub failure_rate: f64,
    /// Additional attempts within this cell's task episodes.
    pub revisions: usize,
    /// Nearest-rank p50 attempt wall time, only when every attempt reports it.
    pub p50_ms: Option<u64>,
    /// Nearest-rank p90 attempt wall time, only when every attempt reports it.
    pub p90_ms: Option<u64>,
    /// Mean USD per attempt, only when every receipt reports USD.
    pub mean_cost_usd: Option<f64>,
    /// Mean tokens per attempt, only when every receipt reports tokens.
    pub mean_tokens: Option<f64>,
}

impl RoutingCell {
    /// Expected cost per delivered episode, including revision attempts. Zero delivery and
    /// incomplete cost coverage are unknown, never cheap. USD and tokens stay separate units.
    #[must_use]
    pub fn expected_cost(&self, usd: bool) -> Option<f64> {
        if self.samples == 0 || self.delivered_rate <= 0.0 {
            return None;
        }
        let cost = if usd {
            self.mean_cost_usd
        } else {
            self.mean_tokens
        }?;
        Some(cost * self.attempts as f64 / self.samples as f64 / self.delivered_rate)
    }

    /// Serializable evidence for explanations and the stats command.
    #[must_use]
    pub fn evidence(&self) -> Value {
        json!({"samples": self.samples, "attempts": self.attempts,
            "verified_first_pass_rate": self.first_pass_rate, "delivered_rate": self.delivered_rate,
            "refusal_failure_rate": self.failure_rate, "revisions": self.revisions,
            "p50_ms": self.p50_ms, "p90_ms": self.p90_ms,
            "mean_cost_usd": self.mean_cost_usd, "mean_tokens": self.mean_tokens,
            "expected_cost_usd": self.expected_cost(true), "expected_cost_tokens": self.expected_cost(false)})
    }
}

/// Builds the last `window` completed task episodes per cell in ledger order. Revisions of a
/// task in the same cell count once for rate denominators, but all their costs count.
#[must_use]
pub fn routing_cells(observations: &[RoutingObservation], window: usize) -> Vec<RoutingCell> {
    type CellKey = (String, String, String, String);
    let mut cells: BTreeMap<CellKey, Vec<(String, Vec<&RoutingObservation>)>> = BTreeMap::new();
    for observation in observations.iter().filter(|o| o.delivered.is_some()) {
        let route = &observation.route;
        let (Some(agent), Some(model), Some(effort), Some(class)) = (
            route["agent"].as_str(),
            route["model"].as_str(),
            route["effort"].as_str(),
            route["task_class"].as_str(),
        ) else {
            continue;
        };
        let episodes = cells
            .entry((
                agent.to_owned(),
                model.to_owned(),
                effort.to_owned(),
                class.to_owned(),
            ))
            .or_default();
        if let Some(index) = episodes
            .iter()
            .position(|(task, _)| *task == observation.task)
        {
            let mut episode = episodes.remove(index);
            episode.1.push(observation);
            episodes.push(episode);
        } else {
            episodes.push((observation.task.clone(), vec![observation]));
        }
    }
    cells
        .into_iter()
        .filter_map(|((agent, model, effort, task_class), episodes)| {
            let selected: Vec<_> = episodes.iter().rev().take(window).collect();
            let samples = selected.len();
            if samples == 0 {
                return None;
            }
            let all: Vec<_> = selected
                .iter()
                .flat_map(|(_, attempts)| attempts.iter().copied())
                .collect();
            let attempts = all.len();
            let delivered = selected
                .iter()
                .filter(|(_, a)| a.last().is_some_and(|a| a.delivered == Some(true)))
                .count();
            let first_pass = selected
                .iter()
                .filter(|(_, a)| a.len() == 1 && a[0].delivered == Some(true) && !a[0].failed)
                .count();
            let failed = all
                .iter()
                .filter(|a| a.failed || a.delivered == Some(false))
                .count();
            let mut latency = all.iter().map(|a| a.latency_ms).collect::<Option<Vec<_>>>();
            if let Some(latency) = &mut latency {
                latency.sort_unstable();
            }
            let percentile = |percent: usize| {
                latency
                    .as_ref()
                    .map(|v| v[(v.len() * percent).div_ceil(100) - 1])
            };
            let mean_cost_usd = all
                .iter()
                .map(|a| a.usage.as_ref()?.cost_usd)
                .collect::<Option<Vec<_>>>()
                .map(|v| v.iter().sum::<f64>() / attempts as f64);
            let mean_tokens = all
                .iter()
                .map(|a| {
                    let u = a.usage.as_ref()?;
                    Some(u.input_tokens.saturating_add(u.output_tokens) as f64)
                })
                .collect::<Option<Vec<_>>>()
                .map(|v| v.iter().sum::<f64>() / attempts as f64);
            Some(RoutingCell {
                agent,
                model,
                effort,
                task_class,
                samples,
                attempts,
                first_pass_rate: first_pass as f64 / samples as f64,
                delivered_rate: delivered as f64 / samples as f64,
                failure_rate: failed as f64 / attempts as f64,
                revisions: attempts - samples,
                p50_ms: percentile(50),
                p90_ms: percentile(90),
                mean_cost_usd,
                mean_tokens,
            })
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use kaisa_ledger::{AttemptUsage, RoutingObservation};
    use serde_json::json;

    use super::routing_cells;

    pub(crate) const HISTORY: usize = 5;
    pub(crate) const CHEAP_USD: f64 = 1.0;
    pub(crate) const STRONG_USD: f64 = 4.0;
    pub(crate) const ATTEMPT_MS: u64 = 100;
    pub(crate) const TOKENS: u64 = 1000;
    const NO_FIRST_PASS_DELIVERIES: f64 = 0.0;
    const ALL_EPISODES_DELIVERED: f64 = 1.0;

    pub(crate) fn outcome(
        task: &str,
        model: &str,
        delivered: bool,
        cost: Option<f64>,
    ) -> RoutingObservation {
        RoutingObservation {
            attempt: format!("att_{task}_{model}"),
            task: format!("task_{task}"),
            route: json!({"agent":"claude", "model":model, "effort":"medium", "task_class":"T1:Developer", "strategy":"measured"}),
            delivered: Some(delivered),
            failed: !delivered,
            latency_ms: Some(ATTEMPT_MS),
            usage: Some(AttemptUsage {
                input_tokens: TOKENS,
                output_tokens: TOKENS,
                cost_usd: cost,
            }),
        }
    }

    pub(crate) fn reliable_history() -> Vec<RoutingObservation> {
        (0..HISTORY)
            .flat_map(|index| {
                [
                    outcome(&format!("cheap_{index}"), "haiku", true, Some(CHEAP_USD)),
                    outcome(&format!("strong_{index}"), "sonnet", true, Some(STRONG_USD)),
                ]
            })
            .collect()
    }

    #[test]
    fn revisions_increase_expected_delivered_cost_and_unknown_usage_never_becomes_zero() {
        let mut first = outcome("revision", "haiku", false, Some(CHEAP_USD));
        let mut revision = outcome("revision", "haiku", true, Some(STRONG_USD));
        revision.attempt.push_str("_retry");
        let cells = routing_cells(&[first.clone(), revision.clone()], HISTORY);
        let cell = &cells[0];
        assert_eq!(
            cell.expected_cost(true),
            Some(CHEAP_USD + STRONG_USD),
            "one delivered task pays both attempt receipts"
        );
        assert_eq!(
            cell.revisions,
            cell.attempts - cell.samples,
            "revisions are additional attempts in an episode"
        );
        assert_eq!(
            cell.first_pass_rate, NO_FIRST_PASS_DELIVERIES,
            "a failed first attempt cannot earn first-pass credit"
        );
        first.usage = None;
        let unknown = routing_cells(&[first, revision], HISTORY);
        assert_eq!(
            unknown
                .first()
                .expect("one unknown-cost cell")
                .expected_cost(true),
            None,
            "missing USD is never a free revision"
        );
        assert_eq!(
            unknown
                .first()
                .expect("one unknown-cost cell")
                .expected_cost(false),
            None,
            "missing usage also prevents token costing"
        );
    }

    #[test]
    fn a_window_discards_old_task_episodes_and_preserves_unknown_latency() {
        let mut old = outcome("old", "haiku", false, Some(STRONG_USD));
        old.latency_ms = None;
        let fresh = outcome("fresh", "haiku", true, Some(CHEAP_USD));
        let one_episode = 1;
        let cells = routing_cells(&[old.clone(), fresh.clone()], one_episode);
        assert_eq!(
            cells.first().expect("one selected cell").delivered_rate,
            ALL_EPISODES_DELIVERED,
            "an old failure is outside the selected window"
        );
        assert_eq!(
            cells.first().expect("one selected cell").p90_ms,
            Some(ATTEMPT_MS),
            "latency uses observed wall time only"
        );
        let all = routing_cells(&[old, fresh], HISTORY);
        assert_eq!(
            all.first().expect("one full-history cell").p90_ms,
            None,
            "partial latency coverage is unknown rather than biased low"
        );
    }

    #[test]
    fn tokens_remain_a_distinct_cost_unit_when_usd_is_unreported() {
        let cell = routing_cells(&[outcome("tokens", "haiku", true, None)], HISTORY).remove(0);
        assert_eq!(
            cell.expected_cost(true),
            None,
            "tokens have no invented exchange rate to USD"
        );
        assert_eq!(
            cell.expected_cost(false),
            Some((TOKENS + TOKENS) as f64),
            "reported tokens remain usable without pricing guesses"
        );
    }
}
