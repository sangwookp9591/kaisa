# Limits, execution costs and remaining risks

Recorded on macOS 27.0.1 arm64, 2026-11-01, source `f3a00c2` plus WP7.
The [regression table](isolation.md) and [CLI reports](spec-demo/traceability.md)
are the evidence for observed outcomes. Prior merged review reports under sibling
worktrees' `target/kaisa-review/` were read as background, not substituted for reruns.

| Label / limit | Consequence and evidence boundary |
|---|---|
| `qa_evidence: candidate-influenceable-in-process` | Candidate code shares the harness process. Positive named results and matching summaries reject the recorded exit-zero attacks, but a candidate anticipating all expected lines can forge output. PASS is not proof against arbitrary in-process forgery. See the demo gate reports and `review_repro_b_candidate_exit_zero_cannot_pass_a_private_criterion`. |
| Plain-goal path has no private QA | Plain `kaisa "<goal>"` now reuses the private-QA libtest/TAP completion parser and termination/configuration diff guards. Cargo test, cargo nextest run and direct Node test commands (including trusted base npm test scripts running node:test) require at least one executed test, no failures and consistent summaries; zero tests, incomplete output and truncation fail with `no positive test evidence`. The recorded exit-zero fixture is blocked; see [before/after](before-after.md). There are still no independent acceptance tests for plain goals. |
| Trusted-check evidence formats | Ordinary libtest checks require completed `running …` / result-summary pairs; nextest checks require named PASS lines and one matching text summary; Node checks require named TAP results and matching counters/plan. Node file-wrapper results cannot substitute for test completion. Unsupported output reporters fail closed for recognised runners; unrecognised commands retain exit-code semantics and receipts label them `exit-code only`. Parser evidence remains candidate-influenceable in-process and can be forged by a candidate anticipating the output. |
| Unknown-command UX risk | Older binaries without the `spec` subcommand treat `kaisa spec` as goal text and can start a real agent attempt. Current dispatch recognizes `spec` and reserves planned commands, but still sends other unknown first words to the goal parser (`crates/kaisa-cli/src/lib.rs:181`); a typo can therefore start a real attempt. This is a known risk; CLI behavior is unchanged. |
| Test harness binary replacement | `bin/kaisa/tests/mcp_serve.rs` relies on the `test-hooks` build to redirect agent launches to the fake. Rebuilding `target/debug/kaisa` without that feature while integration tests run can replace the binary and make those tests launch the real agent CLI found on PATH. Do not run concurrent builds against that target directory; offline validation should also put refusing `claude`/`codex` shims first on PATH. |
| macOS runtime only | Native isolation was exercised here only. The demo regression `bin/kaisa/tests/spec_demo.rs` is compiled only on macOS and is skipped entirely on Linux/Windows. Linux and Windows cross-clippy are compile checks; their filesystem, private-input write denial, networking and author isolation require native runtime validation. |
| Lexical detectors | Test detection, assertions, cfg wrappers, legacy coverage and public-interface extraction use bounded lexical analysis, not semantic proof. Aliasing, generated code and encodings can escape heuristics. The before/after report covers only its concrete fixtures. |
| Same-user trust | State belongs to the user's OS identity. Unconfined/observed agents or unrelated same-user processes may impersonate PTY input or rewrite user-owned state. Digest checks and append receipts do not form a cryptographic boundary against the state owner. |
| Reduced containment | Missing write/state-read/tcp/udp/descendants capabilities and observed mode are aggregated across all attempts. PASS is labelled only when shortfalls are admitted; an unadmitted shortfall is INCONCLUSIVE. macOS demo reports `reduced: descendants`. |
| Reduced private QA isolation | Temporal separation holds for the first developer. Revisions exist while private tests exist; unconfined revisions may read them. Explicit acceptance remains labelled. Author contexts exclude developer reasoning, but developer-controlled documentation/interface text remains prompt-injection input. |
| Private test integrity | `write-denied` reports an available OS mechanism, not native validation on every OS. Platforms without it report `reduced(write-deny unavailable; digests only)` and cannot treat matching bytes as write denial. Compiler/runtime access to private source remains necessary. |
| Approval assurance | The demo intentionally uses provisional policy, never human approval. Unix terminal confirmation verifies a digest and records OS identity; marker/ancestor checks are best effort. Unconfined history reduces assurance. Windows human approval is unsupported and refused, not silently substituted. |
| Ordered review, one session | The five structured stages arrive together. Parsing proves answer order and ledger evidence references, not stage-one reasoning blindness to later context. |
| Routing attribution | Legacy rows without agent/model/effort route identity remain unattributable. Explicit fake/model launches bypass automatic routing. Cold-start catalog order is a proxy; no real model quality, price advantage or latency advantage was measured. |
| Parallel scheduling | Explicit user-supplied DAGs, deterministic waves and write-set exclusion are implemented. Slow siblings delay later waves; admitted failures drain under existing deadlines. There is no immediate interactive sibling cancellation or automatic decomposition. |
| Shared Git metadata | The common-directory mutex and advisory lock serialize cooperating Kaisa worktree calls, including QA and recovery listings. They do not serialize arbitrary external Git processes that ignore the lock. Mixed-thread/process regressions ran here. |
| Production pre-exec lock inheritance window | `crates/kaisa-daemon/src/goal.rs:441-453` names/prepares the attempt before the owner lock; the owner handle is retained at `crates/kaisa-daemon/src/goal.rs:467`, through the process start at `crates/kaisa-daemon/src/goal.rs:716`. Unix fork-before-exec can inherit descriptors before close-on-exec. Concurrent process creation can transiently extend an owner lock's lifetime. This task does not establish elimination of that window; QA author attempts also lack normal owner-lock receipts, producing the preserved conservative “older Kaisa / no owner lock” recovery notices on a second demo build. |
| Runtime ownership | The engine/keeper runs in the CLI process; there is no independent daemon/keeper process or restart reattachment. Existing state may require manual preserved-worktree inspection. |
| Feedback redaction | Source overlap filters and bounded observable messages reject recorded disclosures; arbitrary encodings from hostile in-process code are not proven confidential. |

## Measured offline costs

The demo ran with a zero revision budget. The fake answers are deterministic; IDs,
resource observations and Git digests vary. IDs, hashes and temporary paths have no
numeric variance bound. In the reviewer’s one-off rerun, build wall time differed
by about 3–6%, peak RSS by about 2% (roughly 119–123 MB), per-agent RSS by about
0.5–1.5%, and ledger `elapsed_ms` by about 15% (recorded 32–34 ms versus rerun
28–29 ms). These are observed differences, not guaranteed ranges.
`/usr/bin/time -l` on this macOS host
reports wall seconds at hundredth-second resolution and maximum resident set size
in bytes. A reported `0.00` means below that display resolution, not zero work.
Raw command measurements and per-agent measurements are linked below. RSS is a
maximum, not a sum, and should not be added across stages.

| CLI operation | Wall seconds | Peak RSS bytes | Raw `/usr/bin/time -l` |
|---|---:|---:|---|
| 01-missing-criteria | 0.40 | 9994240 | [01-missing-criteria.time.txt](spec-demo/01-missing-criteria.time.txt) |
| 02-draft | 0.19 | 9797632 | [02-draft.time.txt](spec-demo/02-draft.time.txt) |
| 03-show | 0.00 | 6406144 | [03-show.time.txt](spec-demo/03-show.time.txt) |
| 04-policy | 0.00 | 4063232 | [04-policy.time.txt](spec-demo/04-policy.time.txt) |
| 04-revision-policy | 0.00 | 3866624 | [04-revision-policy.time.txt](spec-demo/04-revision-policy.time.txt) |
| 05-approve | 0.00 | 7028736 | [05-approve.time.txt](spec-demo/05-approve.time.txt) |
| 06-build-fail | 5.28 | 119062528 | [06-build-fail.time.txt](spec-demo/06-build-fail.time.txt) |
| 06-build-pass | 5.17 | 123502592 | [06-build-pass.time.txt](spec-demo/06-build-pass.time.txt) |
| 07-report-fail | 0.01 | 6275072 | [07-report-fail.time.txt](spec-demo/07-report-fail.time.txt) |
| 07-report-pass | 0.00 | 6160384 | [07-report-pass.time.txt](spec-demo/07-report-pass.time.txt) |

Agent-stage costs below measure the fake process for each stage, including its child bridge where used. They exclude Kaisa orchestration and private-suite compilation/execution; the full build rows above include those costs.

| Build | Agent stage | Wall seconds | Peak RSS bytes | Raw measurement |
|---|---|---:|---:|---|
| PASS | characterization author | 0.00 | 2359296 | [att_dmjvasp6kn940000czb0000002](spec-demo/agent-times/att_dmjvasp6kn940000czb0000002.time.txt) |
| PASS | developer | 0.00 | 3325952 | [att_dmjvat4alrao0000czb0000007](spec-demo/agent-times/att_dmjvat4alrao0000czb0000007.time.txt) |
| PASS | acceptance QA author | 0.00 | 2310144 | [att_dmjvatlmtt4g0000czb000000b](spec-demo/agent-times/att_dmjvatlmtt4g0000czb000000b.time.txt) |
| PASS | blind QA author | 0.00 | 2310144 | [att_dmjvatnmsxzk0000czb000000c](spec-demo/agent-times/att_dmjvatnmsxzk0000czb000000c.time.txt) |
| PASS | domain test reviewer | 0.00 | 2342912 | [att_dmjvauh65bco0000czb000000h](spec-demo/agent-times/att_dmjvauh65bco0000czb000000h.time.txt) |
| PASS | staged reviewer | 0.00 | 2473984 | [att_dmjvaukhumds0000czb000000i](spec-demo/agent-times/att_dmjvaukhumds0000czb000000i.time.txt) |
| FAIL | characterization author | 0.00 | 2359296 | [att_dmjvav3gkie80000igt0000002](spec-demo/agent-times/att_dmjvav3gkie80000igt0000002.time.txt) |
| FAIL | developer | 0.00 | 3325952 | [att_dmjvavisujhc0000igt0000007](spec-demo/agent-times/att_dmjvavisujhc0000igt0000007.time.txt) |
| FAIL | acceptance QA author | 0.00 | 2310144 | [att_dmjvaw050els0000igt000000b](spec-demo/agent-times/att_dmjvaw050els0000igt000000b.time.txt) |
| FAIL | blind QA author | 0.00 | 2310144 | [att_dmjvaw226mr40000igt000000c](spec-demo/agent-times/att_dmjvaw226mr40000igt000000c.time.txt) |
| FAIL | domain test reviewer | 0.00 | 2342912 | [att_dmjvawx7jwqg0000igt000000h](spec-demo/agent-times/att_dmjvawx7jwqg0000igt000000h.time.txt) |
| FAIL | staged reviewer | 0.00 | 2473984 | [att_dmjvax0c2emw0000igt000000i](spec-demo/agent-times/att_dmjvax0c2emw0000igt000000i.time.txt) |

Each demo spec build admitted **6 agent sessions**, measured by run-bound `attempt.admitted`
events in [sessions.json](spec-demo/sessions.json) and the corresponding ledger exports.
The two independent draft commands admitted 2 additional sessions outside those builds.
The gate and report commands do not launch agents. Both QA authors use the same
minimal empty-input scenario in this small demo; distinct adversarial reasoning quality
is not being measured by a fake answer.

The private test runner, baseline execution and pure acceptance-gate overhead run
inside the build CLI and are included in its total. Separate peak RSS for those
in-process stages was **not measured**: attributing the build's process maximum
to one of them would be false precision. The per-stage table records actual agent
process costs; it is not a measurement of each internal Rust function.
The exported event schema contains sequence, kind, subject, payload version,
payload and hash; it has no event timestamp column. Private-QA start/finish and gate
events in these runs contain no timestamps either, so internal durations cannot be
derived from ledger timestamps. The existing `elapsed_ms` receipt does record the
captured process interval for normal developer/reviewer attempts (`goal.rs:715-729`),
as shown below; it is distinct from `/usr/bin/time`'s fake-process interval.

| Build | Stage | Ledger-recorded process capture milliseconds |
|---|---|---:|
| PASS | developer | 34 |
| PASS | domain test reviewer | 33 |
| PASS | staged reviewer | 32 |
| FAIL | developer | 30 |
| FAIL | domain test reviewer | 29 |
| FAIL | staged reviewer | 27 |

Real-model tokens, dollar costs and provider latency: **not measured**.

Reproduce costs with:

```sh
python3 examples/spec-demo/run.py --measure-agent --output target/spec-demo-costs
```

The optional observer copies timing files from agent scratch while they exist; the
wrapper invokes only the fake executable under `/usr/bin/time -l`. It redirects
timing output to scratch so it cannot contaminate structured review answers.
No build or performance multiplier is inferred from these tiny offline fixtures.
