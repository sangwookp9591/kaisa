# WP4 — specification workflow and orchestrator acceptance gate

## Defect and reproduction

The WP1–WP3 base exposed specification storage, independent QA, deterministic domain-test lint and semantic test review as disconnected APIs. `kaisa build --spec ...` did not select a specification workflow, no `gate.acceptance` event existed, and ordinary verified delivery did not require independent private QA or specification traceability. The design's §1 flow and §5 acceptance requirements therefore had no enforcing composition root.

Read the WP1 approval-hardening commit `f0f6ac7`, WP2 positive-evidence/isolation hardening commit `8c3664f`, and WP3 review fixes `17f92c5`/`ed10d6a`; retain their receipt validation, source privacy, QA influence labels and lexical-analysis limits. The original deep review reports describe the existing verified path and preservation/isolation limitations; these workflow-specific defects are additions relative to those reports. All new integration reproductions use `kaisa-fake-agent`, private explicit state paths, offline Cargo and disposable Git repositories.

## Changes and decisions

- `kaisa-daemon/src/acceptance.rs`: pure ten-line acceptance gate, final-tree freshness, evidence identifiers and FAIL-over-INCONCLUSIVE-over-PASS aggregation. Agent approval has no authority to change deterministic failures.
- `kaisa-daemon/src/spec_flow.rs`: pin and re-read the exact ledger-backed frozen specification; refuse uncovered requirements and disallowed provisional approval before development; protect parsed repository copies of that specification; run developer preservation/freeze, trusted checks, sealed independent QA, deterministic lint, semantic test review and read-only review in order. Revisions use only observable failures and source-filtered review findings, re-freeze, and rerun both already-sealed suites.
- `spec_flow::Characterize` and `spec_flow::StagedReview` are deliberately small WP6/WP5 seams. `NoCharacterization` explicitly records `not applicable (no characterization adapter configured)`; `ExistingReviewer` uses the current strict verdict protocol with diff, specification, labelled tests, policy decisions and evidence. WP5/WP6 adapters can consume the shared frozen context; no parallel worker's implementation was copied or merged.
- `kaisa-daemon/src/traceability.rs`: JSON and Markdown requirement → criterion → private scenario/result/final tree → lint-checked development mapping matrix, including explicit requirement and criterion gaps.
- `kaisa-cli/src/goal.rs`, `lib.rs`, `spec.rs`: `build --spec <id> [--version n]` reuses ordinary goal options and adds independent QA/domain-review fake-process arguments plus explicit exact-label `--accept-qa-isolation`. Results use exit 0/4/3 and retain provisional labels; `spec report <run>` verifies the preserved report against its gate ledger digest. GateDecided receipts carry the acceptance label and failing lines for existing status views.
- WP1 spec domain is the single criterion model (`CriterionRef` is a compatibility alias to `AcceptanceCriterion`). `TestMapping` and `QaManifestEntry` live beside it; context/workspace re-export them. Required small non-owned hunks add the workspace→domain dependency/lock entry, one GateAcceptance event kind, exports, and expose the existing reviewer prompt helper. Existing lint/review fixtures now construct the full domain criterion.

Publication uses a new `kaisa/result/<run>` branch exclusively after a recorded PASS. Existing `kaisa/att_*` WIP commits/branches remain recovery artifacts: removing those would contradict the required candidate freeze and no-loss preservation contract. FAIL/INCONCLUSIVE create no result branch, emit no merge hint, and never change or auto-commit the user's HEAD. This distinction was reported to the coordinator during implementation.

Rejected alternatives: wrapping `run_verified_goal` would run its delivery gate before private QA; letting reviewers supply the aggregate decision would override deterministic evidence; reauthoring QA after revisions would let candidate changes redefine the acceptance suite; copying private reviewer text directly into developer packs would leak test source.

## Regression evidence

`bin/kaisa/tests/spec_flow.rs` covers protected spec-state writes and repository criterion edits; failing private QA while developer checks/reviewer approve; blocking deterministic lint despite approval; missing R1 criteria before any developer; disallowed provisional policy and provenance labels; reduced observed isolation; result-branch/HEAD invariants for both blocked outcomes; successful publication; report tampering; and sealed-suite reuse/final-tree QA plus source-free feedback after revision.

`acceptance.rs` unit tests cover deterministic dominance, stale-tree private QA rejection, missing isolation evidence, failure dominance and provisional labeling/all requirement lines. `spec_flow.rs` adds a semantic-review-source redaction regression. An additional unreadable-source regression ensures deterministic lint cannot silently skip a nontext test source. Existing WP1–WP3 tests remain part of the full workspace check.

Guard-removal mutation logs are under `target/kaisa-review/mutation-*.log`; the mutation runner restores original sources in a `finally` block. Each of the eight required-behaviour mutations failed with an assertion in its intended integration regression: private QA, deterministic lint, repository criterion integrity, missing criterion classification, provisional policy, reduced isolation, and publication after FAIL/INCONCLUSIVE. Four further guard mutations (stale-tree evidence, semantic-review source redaction, report integrity, and development coverage) likewise failed their targeted regressions; all twelve returned test exit 101 rather than a compilation error. Sources were restored after every group.

Audit anchors: `crates/kaisa-daemon/src/acceptance.rs:129` (pure gate), `spec_flow.rs:334` (admission and revision loop), `spec_flow.rs:657` (independent evaluation), `spec_flow.rs:1193` (gate recording and PASS-only publication), `spec_flow.rs:1323` (report digest validation), `traceability.rs:105` (matrix construction), `crates/kaisa-cli/src/goal.rs:488` (spec CLI adapter), `goal.rs:707` (build dispatch), and `bin/kaisa/tests/spec_flow.rs:628` (development mapping regression).

## Remaining limits

- WP5/WP6 implementations are parallel work packages; the default hook implementations remain active until their adapters are wired after integration. Default characterization is explicitly labelled as no configured adapter, not a claim of baseline characterization.
- QA retains `candidate-influenceable-in-process`; named harness evidence and OS write denial cannot prove an adversarial candidate did not forge in-process output. Native Linux/Windows runtime isolation remains unverified, as WP2 documents. Reduced isolation requires explicit acceptance of the exact recorded boundary.
- Domain-test lint, development mapping checks, repository-spec-copy detection and feedback source-overlap filtering are lexical/structural checks, not proof against arbitrary encodings or semantically equivalent hidden copies. Development mappings are optional supplied domain mappings; absence is shown as an empty development column, while private acceptance coverage is mandatory.
- Real model CLIs were never run. Real-provider QA-author networking/authentication depends on the WP2 author execution boundary; no real-provider success is claimed.

## Final validation

`CARGO_NET_OFFLINE=true scripts/check.sh --fix` exited **0** on the final source tree (`target/kaisa-review/check-final.log`, ending `== all checks passed`). Stable/nightly formatting, workspace clippy with warnings denied, all workspace tests/doc tests, and configured Windows/Linux compile-only cross-checks passed. The twelve workflow integration tests passed in that run, alongside four pure-gate tests and two workflow unit tests; no real model agent or external network was used.

All twelve targeted guard-removal mutations were detected with test exit **101** and assertion failures, and the pristine sources were restored before the final successful full check. An earlier full check also exited 0, but the final log is authoritative because it includes the added development-coverage and unreadable-source regressions. Native Linux/Windows runtime tests and real-provider author execution were not performed.
