# Review follow-up

Addresses REVIEW-RWP6 for commit `3956605` within WP6b's agreed scope.

- **F2 API support:** `Baseline` remains privately constructed by `LegacyCharacterizer::baseline` and now exposes its base tree and recorded baseline event sequence. `compare` requires an `EventStore` and returns `Result<Vec<Finding>, CompareError>`. It verifies the baseline receipt's sequence, kind, run, commit, and tree, requires a developer admission with `role: "developer"` and matching `run_id`, and refuses if any such admission preceded the baseline. A later retry cannot hide an early admission. Empty baselines also receive receipts and require ordered admission. Failed baseline construction yields no usable token.
- **F3:** missing/new-file hints are skipped deterministically; existing hinted legacy areas remain selected. `Baseline::notes()` and its ledger receipt explain skipped hints, including when every hint is new.
- **F4:** cross-file Rust coverage now requires a target module/type or library-crate-qualified function reference inside a real test body. Same-file tests can reference functions directly. The reviewer's `pub fn new()` plus unrelated `Vec::new()` fixture, extended with `default` and `from`, no longer hides legacy code. This remains lexical evidence: aliases/macros may be missed and identical module/type names can still be ambiguous.
- **F5:** preserve/unknown dispositions use strict empty struct variants so a `criterion_id` (or another unknown field) is rejected; only correct accepts it.

## Integration boundary

Per coordinator decision, this change does not define `trait Characterize` (F1) or wire WP4's production admission path (F2). The integration follow-up must await baseline success before developer admission, record run-scoped developer admission payloads, and use the checked comparison API. The module cannot prevent an external component from launching a developer without invoking it; it refuses comparison after that invalid order.

Existing limitations F6 (acceptance QA must establish the intended correction) and F7 (no dedicated comparison-summary event) remain unchanged.

## Validation

- Before the fixes, the three new F3/F4/F5 regressions failed against the original implementation (12 other characterization tests passed).
- Disabling the production ordering guard caused `comparison_refuses_a_developer_admitted_before_the_baseline` to fail with `Ok([])` instead of the typed ordering error; the guard was restored immediately.
- All 18 characterization tests passed in the final full check, including the additional qualified-reference regression.
- Full `scripts/check.sh`: exit 0, including stable/nightly formatting, workspace clippy and tests, Windows cross-checks, and Linux cross-checks.
- No real Claude/Codex agent or network operation was used; characterization execution used the local fake agent and fixture repositories.
