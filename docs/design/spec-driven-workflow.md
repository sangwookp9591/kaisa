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

| Boundary | Mechanism | Where it is weaker (always labelled, never silent) |
|---|---|---|
| Developer cannot change the approved spec | Spec lives only in the state directory; the developer writes only through the broker write set (Claude) or its own workspace sandbox (Codex). The gate re-hashes the spec and compares with the `spec.approved` digest; any mismatch is FAIL. | — (the digest check holds even if a write slipped through) |
| Developer cannot read private QA tests | **Temporal**: QA tests are authored after the developer session has ended. **Spatial**: they live in the state directory, which confined agents cannot read (macOS Seatbelt state-read deny). | Revision rounds run while QA tests exist: an unconfined developer (Claude reduced mode, Codex observed, Linux state-read gap) could read them. The run is then labelled `qa-isolation: reduced` and the gate returns at best INCONCLUSIVE unless the policy key `allow_reduced_qa_isolation` is set (still labelled). Revision feedback carries observable failures (criterion id, input, expected vs actual), never test source. |
| QA does not see developer reasoning | QA prompts are built from the frozen spec and the public interface only (the candidate's public files listed by an interface extractor, with development tests and transcripts excluded). QA runs in a fresh checkout, not the developer worktree. | QA could still read source of the candidate it executes; "black-box" means no developer reasoning, history or developer test results — documented. |
| Developer cannot approve its own work | The developer role has no tool or command that records a verdict or approval; the gate is computed by the orchestrator from recorded evidence. | — |
| Human spec approval | The binding control against agents is the confinement sandbox: state-directory write isolation plus PTY denial. `kaisa spec approve` displays the selected version and criteria, requires terminal stdin/stdout and a digest prefix, and records the OS user. It refuses while any recorded attempt is live in that state directory. Marker and ancestor checks are best effort. | Unconfined agents can detach/reparent, allocate PTYs, or rewrite user-owned state. Receipts record `attempts_unconfined_since_draft`; affected approval is shown as `human-approved (reduced assurance)`. A same-user process outside an attempt can also impersonate terminal input. This is shell-level trust, not cryptographic human identity. Windows human approval is unsupported and refused; provisional approval remains explicitly labelled and policy controlled. |

## 4. Deterministic checks (no model involved)

- **Traceability**: every requirement → ≥1 criterion; every criterion → ≥1 private acceptance test that ran on the final candidate tree; reported as a matrix.
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
