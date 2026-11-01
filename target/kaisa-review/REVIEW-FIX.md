# WP5 — Context-aware staged review

## Reproduction and original gap

Read design `docs/design/spec-driven-workflow.md` §1 and §2, the architecture/PRD/user-flow constraints, and `docs/testing.md`. Main-checkout evidence `target/kaisa-review/deep/D2/REPORT.md` §8.28 describes the existing incomplete reviewer context; §8.26 and §8.24 describe the existing final-verdict parser and failure handling. The baseline at this worktree starts from `feat/spec-workflow` plus merged WP2 (`afc25d8`). No staged review module or stage-aware verdict validation existed.

Reproduced the protocol gap using a plain `{"candidate_tree":"frozen-session-tree","approve":true,"findings":[]}` approval: the legacy parser accepts it with no stages, context analysis or evidence of judgment changes. Regression `a_plain_verdict_without_review_stages_cannot_approve_a_spec_bound_candidate` preserves legacy acceptance while rejecting it for specification-bound review. The old plain-goal reviewer is unchanged.

## Implementation and interfaces

- `crates/kaisa-daemon/src/staged_review.rs:56`: owned, review-only input splits bounded diff, tests/spec, developer final answer, ledger revisions/interventions, evaluation context and trusted evidence. Nothing writes the bundle into a candidate, public-interface tree or QA suite.
- `crates/kaisa-daemon/src/staged_review.rs:167`: JSON-escaped context sections in the required five-step order; stage one has only bounded diff. Stage three carries decisions and rejected alternatives from the supplied developer final answer plus ledger history and interventions. Evaluation explicitly covers architecture, domain invariants, error handling, security and external integrations.
- `crates/kaisa-daemon/src/staged_review.rs:189`: serde strict tagged schema rejects absent, duplicate or reordered stages and unknown fields. Stable concern ids reconcile initial concerns with final findings; an initial concern cannot silently disappear or change severity. Every judgment change has a nonempty reason and known candidate-bound evidence ids. Resolutions and blocking downgrades require passing Check/QA evidence; explanations, source references, failed results and stale results cannot resolve blockers.
- `crates/kaisa-daemon/src/staged_review.rs:303`: final dedicated unquoted `kaisa-review` fence parser rejects quoted examples, ambiguous, unfinished and non-final answers.
- `crates/kaisa-daemon/src/staged_review.rs:348`: `run_staged_review(GoalRequest, &StagedReviewInput)` runs one Reviewer attempt with an empty write set, overriding caller role/write settings. Failed/truncated/tool-denied/writing attempts cannot yield a validated review. Actual attempt containment evidence is retained for the orchestrator.
- Minimal additive public module export in `crates/kaisa-daemon/src/lib.rs`. The validated wrapper has private construction and exposes verdict/stages for the acceptance gate and ledger. No domain verdict changes were needed.

The coordinator explicitly reassigned hook trait ownership to WP4 (`spec_flow::StagedReview`, message `msg_55e137730746`), directing WP5 to expose plain functions/types instead of defining a conflicting trait. Followed that instruction and sent the concrete interface to the coordinator. WP4 supplies the thin adapter and records stage/gate evidence.

Small ownership exceptions: one negative test in existing `qa.rs` at its private context-building seam, plus `bin/kaisa/tests/staged_review.rs` to use the existing real fake-agent binary/test launcher. No QA production behavior or fake-agent fixture changes.

## Tests and mutation checks

Required regressions passed, and their guards were deliberately weakened to observe failures before restoring them:

| Regression | Removed/weakened behavior | Observed result |
| --- | --- | --- |
| Missing/duplicate/unordered stages | Skip exact stage sequence rejection | FAIL; `mutation-stage-order.log` |
| Judgment change without evidence | Permit empty references and bypass execution requirement | FAIL; `mutation-missing-evidence.log` |
| Developer-explanation-only resolution | Disable execution evidence gate | FAIL; `mutation-persuasion.log` |
| Silent omission/downgrade | Skip final concern reconciliation | FAIL; `mutation-silent-omission.log` |
| Context contents per stage | Inject tests into diff stage | FAIL; `mutation-context-partition.log` |
| Dedicated final review region | Permit text after review fence | FAIL; `mutation-final-fence.log` |
| Fake-agent read-only attempt | Use Developer role and caller write set | FAIL after a forbidden write creates a commit; `mutation-read-only-attempt.log` |
| Blind QA context exclusion | Contaminate QA pack with developer reasoning | FAIL; `mutation-qa-boundary.log` |

All mutation logs are in `target/kaisa-review/`; each captured a real `test result: FAILED`, not a compile failure. Restored production guards afterwards. Positive tests cover both trusted check and QA result resolutions, preserved stage records, stale/unknown/failed evidence rejection, schema/fence rejection, and a real fake-agent attempt receiving ordered context while its brokered write is refused. State is isolated under per-test temporary roots; no real agent, credentials or network were used.

## Design choices and limits

Used an ordered stages array rather than JSON object key order, which JSON does not give semantic significance. Reused the plain verdict validation only after strict staged validation, preserving plain goals. Kept trusted evidence supplied by the orchestrator rather than accepting self-declared reviewer evidence; the caller must obtain it from trusted execution/ledger records and select the frozen candidate, bounded diff and stage inputs.

One attempt receives all five ordered context sections in one instruction. The parser deterministically enforces answer order and evidence transitions; it cannot prove a model did not read a later section early or enforce private thought order. This is ordered-context review, with temporal blindness between review stages unverified. Private QA still receives only specification/public-interface context, never this bundle. The gate must inspect actual containment receipts; the fake-agent integration intentionally uses an explicit reduced-mode decision, so it does not claim full OS confinement. WP4 wiring/ledger recording is outside this package.

## WP4 adapter mapping after integration

`spec_flow::StagedReviewInput.context` provides repository/state/run/base/candidate and the frozen specification. Build a `GoalRequest` with that repository, the frozen candidate commit as base, workflow reviewer launcher/containment settings, and the hook state directory; the staged function itself replaces goal/role/write set. Serialize `specification` plus labelled `tests` into `tests_and_spec`; pass `diff` directly as already bounded context. Split `decisions` into the developer final answer, revision ledger and human interventions; never hand this data to QA. Convert authenticated execution entries in `evidence` into `ReviewEvidence` with exact ids, candidate tree and pass/fail status; do not convert arbitrary narrative strings into Check/Qa evidence.

Map a successful, adequately confined attempt with `review: Ok` to Pass only when `verdict().approves(candidate.tree)`, otherwise Fail for blocking findings. Map invalid/failed attempts and unaccepted reduced containment to Inconclusive. Record all `stages()` in the ledger and return a candidate-bound `GateCheck::recorded` citing the actual review event id. Keep deterministic checks and private QA independent: a review approval cannot override their failures. The WP4 trait files are absent from this worktree, so this dependency adapter is deliberately left to WP4/coordinator after package integration rather than adding a second conflicting trait.

## Full repository check

`scripts/check.sh` exited **0**: stable/nightly rustfmt, workspace clippy with warnings denied, the entire workspace test/doctest suite, Windows compile-only clippy checks and Linux compile-only clippy checks all passed. Complete output: `target/kaisa-review/check.log`. Final assertion-only cleanup replaces a success-code literal with a named constant and locates the history stage by name; final targeted format, clippy, unit and fake-agent integration verification is also captured under `target/kaisa-review/final-*.log`.
