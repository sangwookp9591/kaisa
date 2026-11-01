# ADR-0032 addendum: measured routing

Status: implemented for the unpinned Claude verified-goal path. This extends T27c;
`run_verified_goal` remains an explicit-launch API, and the routed entry point is
`run_verified_goal_with_routing`. Mission scheduling is unchanged.

A task class is its deterministic grade and existing role (`T1:Developer`, for
example). No language or size category is invented. Eligible models must meet
both grade and declared floor. Cold start retains the existing catalog ordering
(grade, agent, model identifier), at medium effort. This ordering is a proxy for
strength, not a measured price list.

## Measurements and scoring

The ledger joins `route.decided` to admitted attempts, usage receipts, recorded
process wall time, frozen candidates, check evidence, review verdicts and delivery
gates. Process success alone never earns delivery credit. Legacy attempts without
route identity are unattributable. Legacy latency remains unknown. Refusals,
failed processes and failed verification contribute failures, not rewards.

Cells are `(agent, exact model id, effort, grade + role)`. A sample is a completed
logical task episode in that cell; repeated attempts of that task in that cell
count as revisions. The last `routing_window` completed episodes are retained,
ordered by their last completed attempt in ledger order. Rebuilding projections
or reopening the ledger gives the same result. In-flight work is excluded.

The score is:

```
mean cost per attempt × (attempts / task episodes) / delivered task rate
```

Only cells with at least `routing_min_samples` episodes enter measured selection.
They must meet `routing_target_delivery_bps` and, if set, the p90 latency budget.
The lowest expected cost wins with stable catalog-order tie breaking. USD and
tokens are separate units: feasible complete USD measurements are compared first;
if none exists, complete token measurements are compared. An absent cost receipt
is never zero. A partially reported cost or latency window remains unknown;
latency uses nearest-rank quantiles only with complete wall-time coverage.

## Escalation, exploration and pins

When measured cells exist but none meets the constraints, choose the next catalog
model at medium. A failed measured, explored or escalated attempt similarly forces
the next model on that task's next attempt, ahead of exploration and cost ranking.
Model order is the eligible catalog order, including another option in the same
grade. At the ceiling, exhaustion returns an error instead of silently repeating
a failed high-effort route.

High effort is allowed for the preceding model only if its own measured cell
meets the constraints and either no stronger model exists, or the stronger model
at medium has enough samples and fails the constraints or has a worse expected
cost in the same unit. Unmeasured stronger-medium results do not justify high
effort. Maximum effort is never used. An escalated medium route may itself have
insufficient or unfavorable evidence; its recorded candidate table exposes this
rather than claiming the delivery target was achieved.

Exploration uses a BLAKE3 hash of the policy seed and logical task id. It selects
an under-sampled eligible medium cell only after at least one eligible medium
cell has measured history. The same task and seed reproduce the draw. Security
roles, security-path grader reasons and caller-declared sensitive tasks never
explore. No eligible model means no route.

Explicit `--agent`, `--model` and scripted `--agent-program` launches bypass the
automatic path. A model pin applies to the developer; the reviewer retains its
separate role model. The decision API also supports exact pins and refuses an
ineligible pin instead of substituting another model.

## Policy and explanations

These are policy defaults, not observed performance claims:

| Key | Default | Meaning |
| --- | --- | --- |
| `routing_window` | 100 | Completed episodes per cell |
| `routing_min_samples` | 5 | Episodes before measured selection |
| `routing_target_delivery_bps` | 8000 | Delivered-rate constraint in basis points |
| `routing_exploration_bps` | 500 | Exploration probability in basis points |
| `routing_latency_budget_ms` | none | Optional attempt p90 bound |
| `routing_seed` | 0 | Reproducible exploration seed |

The policy snapshot captures these keys. A route stores strategy (`cold_start`,
`measured`, `escalated`, `explore`), effort, task identity/class, effective policy,
comparison unit and the candidate table with measured values and exclusions.
`kaisa route explain <task|last>` prints that recorded explanation, rather than
rerouting with today's data. `kaisa stats routing` derives windowed cells read-only
and labels unknown measurements.

## Limits and rejected alternatives

No performance improvement is claimed without a measured benchmark. This is
empirical windowed selection, not a Bayesian prior, counterfactual estimator,
shadow benchmark, drift detector, rollback controller or policy learner. Exact
catalog model ids partition cells, but prompt/verifier versions are not currently
available as trustworthy receipt fields. Catalog aliases can change underneath
an id. Cross-model escalation episodes stay separate: the failed cell gets its
failure, the delivering cell gets its own result, and costs are not transferred
between cells. Same-cell revisions are fully costed. This limits causal claims
about the whole task cost of an initial model choice.

The built-in path currently estimates impacted files from declared writable paths;
its security-path detection uses named path components (`auth`, `security`,
`crypto`, `credentials`). It does not claim full repository impact analysis.
Explicit launches and tool-less advisory first passes are not measured routes.
Keeping unknowns rather than filling catalog prices avoids invented exchange
rates, invented wall times and free-looking missing receipts (NFR-08).
