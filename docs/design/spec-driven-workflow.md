# Specification-driven, independently verified workflow (design, v0.1)

Status: design for implementation. It extends the verified run (`team.rs`: developer → trusted checks → reviewer → gate) without replacing it. Plain `kaisa "<goal>"` keeps working unchanged. The new workflow runs when a goal is bound to a specification: `kaisa build --spec <id>`.

The purpose is **separated ownership with independently verifiable evidence**, not more tests:

| Artifact | Owner | Who may change it |
|---|---|---|
| Specification and acceptance criteria | the human | only through a new spec version that a human approves |
| Implementation and development tests | Developer agent | the developer, inside its worktree and write set |
| Private acceptance and adversarial tests | QA agents | QA agents, inside the private QA store, outside every developer workspace |
| Test-quality findings | Domain Test Review agent and deterministic linters | — |
| Review findings | Review agent | — |
| The final PASS / FAIL / INCONCLUSIVE | the orchestrator (Kaisa) | nobody else; agents only contribute evidence |

## 1. Flow

```
requirements ─► Specification agent ─► spec draft (v1, provisional)
                                        │  human approves on a terminal  ─► spec v1 frozen (human-approved)
                                        │  or policy allows provisional  ─► spec v1 frozen (provisional, labelled)
                                        ▼
                   [legacy area without tests?] ─► characterization stage (baseline evidence on the base tree)
                                        ▼
                               Developer agent (spec, not tests) ─► candidate frozen
                                        ▼
        QA author agent (spec + public interface only) ─► private acceptance tests (AC-mapped)
        Blind QA agent  (spec + public interface only) ─► private adversarial tests
                                        ▼   (both written after the developer session ended, into the private QA store)
            run private tests on a fresh checkout of the candidate (trusted runner, sandboxed)
                                        ▼
            Domain Test Review: deterministic lint + semantic subagent (spec + tests only)
                                        ▼
            Context-aware Review agent: diff → tests → decisions/history → evaluation → judgment changes
                                        ▼
            Acceptance gate (orchestrator): PASS / FAIL / INCONCLUSIVE  ─► deliver only on PASS
                 FAIL with fixable findings ─► bounded revision (developer gets observable failures, never test source)
```

## 2. Data model (domain types in `kaisa-domain`, events in the ledger)

- `Specification { id: SpecId, version: u32, title, requirements: Vec<Requirement>, criteria: Vec<AcceptanceCriterion>, ambiguities: Vec<Ambiguity>, invariants: Vec<String>, error_behaviour: Vec<String> }`
- `Requirement { id: "R1", text }` (verbatim lines of the original request; nothing is paraphrased away)
- `AcceptanceCriterion { id: "AC-1", requirement_ids: Vec<"R1">, given, when, then, kind: Normal|Edge|Error|Invariant }` — traceability is the `requirement_ids` link. Every requirement must be covered by ≥1 criterion.
- `SpecApproval = Provisional { policy } | HumanApproved { user: String, method: "terminal-confirmation", at, digest }` — the type has no constructor for `HumanApproved` outside the approval path.
- Spec bytes are canonical JSON, content-addressed (`blake3`) in the state directory (`state/specs/<id>/v<n>.json`, read-only after freeze), mirrored by ledger events: `spec.drafted`, `spec.approved` (human or provisional, with digest), `spec.frozen`.
- QA artifacts: `state/qa/<run>/acceptance/…`, `state/qa/<run>/adversarial/…`, each with a manifest `{ test_id, criterion_ids, file, digest }`; events `qa.suite_recorded`, `qa.result` (per test: pass/fail, output digest, ran on candidate tree X).
- Review records: `review.stage` (stage 1..5 with concerns and judgment changes), `domain_test_review.result`, `gate.acceptance` (PASS/FAIL/INCONCLUSIVE with each check's status and evidence ids).

## 3. Isolation — enforced by mechanisms, labelled where it cannot be

Runtime isolation was exercised only on macOS: native Linux and Windows guarantees for
private-QA write denial and author isolation remain **unverified** (NFR-04/NFR-08).
The Linux/Windows cross-clippy checks in `scripts/check.sh` are compile-only evidence;
this macOS host's Windows cross-check excludes the SQLite-dependent daemon.

| Boundary | Mechanism | Where it is weaker (always labelled, never silent) |
|---|---|---|
| Developer cannot change the approved spec | Spec lives only in the state directory; the developer writes only through the broker write set (Claude) or its own workspace sandbox (Codex). The gate re-hashes the spec and compares with the `spec.approved` digest; any mismatch is FAIL. | — (the digest check holds even if a write slipped through) |
| Developer cannot read private QA tests | **Temporal**: QA tests are authored after the developer session has ended. **Spatial**: they live in the state directory, which confined agents cannot read (macOS Seatbelt state-read deny). | Revision rounds run while QA tests exist: an unconfined developer (Claude reduced mode, Codex observed, Linux state-read gap) could read them. The run is then labelled `qa-isolation: reduced` and the gate returns at best INCONCLUSIVE unless the policy key `allow_reduced_qa_isolation` is set (still labelled). Revision feedback carries Kaisa-computed criterion/scenario ids and pass/fail, plus a bounded assertion-message excerpt scanned against the entire private suite; self-declared runtime observations are discarded. Each revision supplies its own attempt receipt for `qa_isolation`. |
| QA does not see developer reasoning | QA prompts are built from the frozen spec and the public interface only (the candidate's public files listed by an interface extractor, with development tests and transcripts excluded). QA authors run in a freshly initialized repository containing only the context pack and its own initial commit. macOS denies reads of the developer repository and common Git directory; Linux authors use a read allow-list, and Windows grants only the context repository/runtime. Candidate execution is a separate phase. | Candidate code executes in the private harness process and can read the private source that the compiler/runtime needs. README/docs, exported Node JSDoc and CLI help are developer-controlled contract data and remain a prompt-injection risk; authors must treat them as untrusted. Rust doc comments are currently omitted, unlike Node JSDoc. |
| Developer cannot approve its own work | The developer role has no tool or command that records a verdict or approval; the gate is computed by the orchestrator from recorded evidence. | — |
| Nobody but a human can approve a spec | `kaisa spec approve` requires an interactive terminal (stdin and stdout are TTYs), refuses inside an attempt (marker environment and an ancestor-process check), and requires typing the first 8 characters of the spec digest. It records the OS user. | A determined local process that fakes a TTY outside any attempt can impersonate the user; this is the same trust level as the user's shell and is documented. |

Private QA evidence always carries `qa_evidence: candidate-influenceable-in-process` (NFR-04).
Private tests execute in the same process as candidate code. Named libtest/TAP results and a
matching zero-failure summary reject premature `exit(0)` (including Node's successful file
wrapper), but a candidate that anticipates the harness can still forge in-process output.
Neither a successful test report nor OS confinement proves that a hostile candidate could not
influence the verdict. The gate must preserve this label. Message overlap filtering removes
complete normalized fields that occur in source, embedded source fragments (at least
16 characters), and 8-token shingles; it does not prove
confidentiality against arbitrary encoding of source by hostile in-process code.

Tests are installed outside the checkout and writable scratch. Seatbelt explicitly denies
`file-write*` to inputs, generated manifests and compiled artifacts; Linux excludes them from
Landlock grants. Every scenario verifies all private input digests before and after execution,
and compiled executables are pinned and read-only during execution. A platform without the
write-deny records `private_test_integrity: reduced(write-deny unavailable; digests only)` and
`private_test_unchanged: false`; matching bytes alone are never presented as integrity evidence.
The `write-denied` label reports an available OS mechanism, not native platform validation.

Cargo compiles all private targets once per suite using a Kaisa-owned `CARGO_HOME`, outside
candidate Cargo/toolchain configuration discovery, with explicit wrapper/environment overrides.
Cargo has no null runner value: exact-host and cfg runners are overridden with a fail-closed
sentinel, compilation uses `--no-run`, and Kaisa executes each artifact directly with no Cargo
runner. Node uses the trusted `node --test --test-reporter=tap` with empty `NODE_OPTIONS` and
statically named tests. Dynamic, ignored or nested test names cannot satisfy the expected-name
set. Added/changed `.cargo/config*`, `build.rs`, `rust-toolchain*`, package scripts/test configuration
and Node options produce blocking **QA-influencing configuration** findings; newly added
termination calls in library sources also block and require reviewer justification with evidence.
The findings do not have an automatic approval escape hatch.

The former per-scenario checkout and cold build multiplied compilation cost by the scenario
count. The runner now uses one cold build per suite and per-scenario input/artifact integrity
checks, without a shared writable dependency cache. Acceptance and adversarial suites still
build separately. On Unix, existing trusted offline registry/git sources are linked read-only
into the fresh Cargo home; config, credentials and executables are not inherited. On Windows,
dependency provisioning for the fresh offline home remains the trusted launcher's responsibility.
This does not estimate production-project timing; fixture measurements belong in the review
report, with the measured command and scope.

Public context includes root README files, allow-listed text in `docs/**`, lexical Rust public
signatures/types/modules and sanitized crate metadata, Node entry metadata/export names/JSDoc,
and bounded sandboxed CLI `--help` output. It excludes implementation bodies, Rust doc comments,
`#[cfg(test)]` items, `tests/`, test/spec filename patterns, dotfiles, transcripts, final answers,
developer test results and commit-message/history data. An executable author is confined as
well as instructed; its filesystem view must not be equated with prompt-pack cleanliness.

## 4. Deterministic checks (no model involved)

- **Traceability**: every requirement → ≥1 criterion; every criterion → ≥1 private acceptance test that ran on the final candidate tree; reported as a matrix. Missing acceptance mappings are reported in `QaOutcome.uncovered_criteria` and `qa.suite_recorded.uncovered_criteria` with `coverage_blocked: true`. A nonempty list is a blocking traceability failure, even when every recorded test passed; adversarial mappings cannot fill an acceptance gap.
- **Domain test lint** (per test, language-aware for Rust `#[test]`, Node `node:test`/`test()`/`it()`): name states condition + outcome (rejects `test_case_1`, `should_work`, `it works`, names under N words); setup uses named fixtures/constants (flags bare numeric/string literals in assertions); assertions present and not trivially true (`assert!(true)`, self-compare, `assert.ok(x)` on the value under test only); error behaviour asserted for `Error` criteria.
- **Protected assets**: approved spec digest unchanged; existing tests not weakened (current `candidate.rs` signals plus deletions/skip/todo/should_panic/cfg-gating); private QA tests unchanged between authoring and run (digest).
- **Legacy characterization**: when the change touches files with no covering tests, a characterization suite must exist and pass on the base tree before the developer starts; it is re-run on the candidate and every changed outcome must be explained by a criterion marked `correction`.

## 5. Acceptance gate

`PASS` only when all hold: spec approval verified (human, or provisional with the policy allowing it — the result then says `PASS (provisional spec)`); spec digest unchanged; traceability complete; deterministic checks green; every private acceptance and adversarial test passed on the final tree; domain test review has no blocking finding; context-aware review approved with no unresolved blocking finding; legacy characterization honoured; evidence preserved. `FAIL` when any check failed with evidence. `INCONCLUSIVE` when a check could not run or isolation was reduced without policy acceptance. A reviewer approval never overrides a failed deterministic check. Delivery (the result branch and its merge instructions) happens only on PASS; exit codes: PASS 0, FAIL 4, INCONCLUSIVE 3.

## 6. Implementation plan and ownership

| Work package | Priority | Owns (new files unless stated) |
|---|---|---|
| WP1 Spec core | P0 | `kaisa-domain/src/spec.rs`, ledger `store/spec.rs`, event kinds, `kaisa-daemon/src/spec_agent.rs`, `kaisa-cli/src/spec.rs` (`kaisa spec draft|show|approve|list`) |
| WP2 Private QA | P0 | `kaisa-daemon/src/qa.rs`, `kaisa-workspace/src/qa_store.rs`, public-interface extractor, QA runner on a fresh checkout |
| WP3 Domain test review | P1 | `kaisa-context/src/test_lint.rs` (deterministic), `kaisa-daemon/src/test_review.rs` (semantic subagent) |
| WP4 Workflow and acceptance gate | P0 | `kaisa-daemon/src/spec_flow.rs`, `kaisa-daemon/src/acceptance.rs`, `kaisa-cli` `build --spec`, traceability report |
| WP5 Context-aware review | P1 | `kaisa-daemon/src/staged_review.rs`, verdict schema extension (stages, judgment changes) |
| WP6 Legacy characterization | P1 | `kaisa-daemon/src/characterize.rs` |
| WP7 Reports, docs, demo, evidence | P2 | architecture docs, README, `kaisa spec report`, end-to-end demo, before/after comparison |

Waves: WP1 + WP2 + WP3 (independent) → WP4 + WP5 + WP6 (integration) → WP7 (real runs, reports).
