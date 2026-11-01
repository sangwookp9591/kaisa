# FIXA2 plain-goal evidence guard rerun

Recorded offline on macOS arm64, 2026-11-01, against the review of `6871daf`.
**False blocks: 16 before → 4 after, across the same 57 plain goals.** All ten
false blocks covered by F1–F4 now deliver, as do the ANSI-color and compile-only
cases. The four remaining false blocks are the explicitly documented unsupported
formats below. No new false blocks appeared. Counts use the reviewer's expectation
labels, including its “arguably delivered” compile-only case.

Before results are the retained original reviewer's 57-case results, not a newly
built baseline binary. The original `gen_cases.py`, `cases.json`, `cases2.json`,
`drive.py` and outputs were recovered from its scratch directory and preserved
under `target/review-FIXA/`. After used the same case data and fake developer / always
approving reviewer with `revision_limit=0`. Driver changes only retained advisory
receipt fields and placed fixture roots outside the enclosing Cargo workspace.
An initial run with fixtures under this repository caused Cargo workspace-membership
errors; its artifacts are marked `misplaced-fixtures-*` and excluded from these counts.

Refusing `claude`/`codex` shims were first on PATH for every test run, with
`CARGO_NET_OFFLINE=true`; no network or real agent CLI was used. No Kaisa build
ran concurrently with tests or the sweep. `scripts/check.sh` exited **0**, including
stable/nightly formatting, Clippy, workspace tests (**1104 passing test summaries,
zero failed**) and the configured Windows/Linux compile checks. The existing
spec-demo and spec workflow tests passed.

The five new focused QA regressions were first run against the original
implementation: **0 passed, 5 failed**. They then passed with the fixes. The final
plain-goal integration suite passed **15/15**, including real cargo self-reexec,
`--nocapture`, `harness=false` alongside libtest, ANSI libtest color, CLI exit aliases,
compile-only checks, advisory receipts/reviewer context and actionable failure text.
The Node/npm positive fixture uses the exact reported name
`ships an executable src/cli.js`.

| Review finding | Committed regression / outcome |
|---|---|
| F1 | `review_fixa_libtest_accepts_inherited_stdout_and_color` and `review_fixa_real_cargo_stdout_variants_deliver`; all passing summaries aggregate despite extra stdout headers. Both the original repository workspace output replay and a supplementary replay of this run's workspace output deliver with positive test evidence. |
| F2 | `review_fixa_termination_aliases_are_visible_to_spec_scan`, `review_fixa_plain_cli_exit_aliases_are_advisory_with_passing_tests` and `a_plain_goal_termination_scan_is_advisory_even_with_exit_code_only_checks`; plain receipts and reviewer evidence carry `advisory: added process termination at file:line`. Private QA retains blocking findings, including common Rust and Node aliases. |
| F3 | `review_fixa_node_options_mentions_outside_package_configuration_are_harmless`; source strings/environment reads and package descriptions do not trigger node-options checks. Relevant package node-option fields and protected configuration changes remain guarded. |
| F4 | `review_fixa_tap_names_can_end_in_source_extensions_and_punctuation` plus the Node/npm integration fixture; descriptive JS/TS names pass while actual file-wrapper evidence still fails. |
| F5 | `review_fixa_no_run_is_compile_only_and_missing_evidence_is_actionable`; the reason starts `no positive test evidence from` followed by the command in backticks, identifies what was missing and gives a rerun action. It survives unchanged revisions. Ordinary nonzero test exits are labelled `test command failed`, never missing positive evidence. |
| F6 | `review_fixa_private_counts_are_pinned_to_expected_tests`; expected `[a,b]` with a Rust summary of 3 passes and the analogous extra nested TAP test are rejected. |

The regressions live in [qa.rs](../../crates/kaisa-daemon/src/qa.rs) and
[plain_goal_evidence.rs](../../bin/kaisa/tests/plain_goal_evidence.rs).
Normalized per-case receipts are in [plain-goal-fixa2-results.json](plain-goal-fixa2-results.json).
Local raw artifacts remain under `target/review-FIXA/`: `check.log`,
`regressions-before.log`, `regressions-after.log`, `plain-goal-tests.log`,
`sweep.log`, `before/`, `after/`, `supplementary/` and `exit-zero-demo/`.

Remaining limits (also in [limitations.md](limitations.md)):

- `nextest-status-level-fail` and `nextest-flaky-retry`: named PASS lines and the
  supported summary format are required; these were scripted, with no real
  nextest installation available.
- `n-spec-reporter`: recognised Node tests require TAP output.
- `n-two-node-runs`: chained runner streams are unsupported; configure separate
  explicit checks instead.
- `r-attack-alias-forged-summary` still delivers, as it did before. Its termination
  is now visible as advisory. A candidate can anticipate and forge passing output
  in the same process; the plain-goal policy does not claim to prevent that.
- Test names identical to an existing file path can still be conservatively
  classified as wrappers. Lexical alias detection is bounded, not name resolution.
- Protected writes (`build.rs`, Cargo config, toolchain and `.npmrc`) remain refused
  before their new guards can be reached; the package lint-script edit retains its
  pre-existing check-definition stop.

The exact committed exit-zero comparison was rerun separately: exit **4**, blocked
by missing positive evidence; its normalized transcript replaces
[plain-goal-guards/exit-zero.txt](spec-demo/plain-goal-guards/exit-zero.txt).

| Case | Before (CLI exit) | After (CLI exit) | Reviewer expectation |
|---|---|---|---|
| `r-legit` | delivered (0) | delivered (0) | delivered |
| `n-legit-npm-test` | delivered (0) | delivered (0) | delivered |
| `r-legit-autodetect` | delivered (0) | delivered (0) | delivered |
| `r-main-exit-status` | blocked (3) | delivered (0) | delivered (legit CLI) |
| `r-main-exit-use-alias` | delivered (0) | delivered (0) | delivered (legit CLI) |
| `r-attack-exit-direct` | blocked (3) | blocked (4) | blocked |
| `r-attack-exit-helper` | blocked (3) | blocked (4) | blocked |
| `r-attack-exit-alias` | blocked (4) | blocked (4) | blocked |
| `r-attack-alias-forged-summary` | delivered (0) | delivered (0) | blocked |
| `r-attack-cargo-runner` | blocked (4) | blocked (4) | blocked |
| `r-attack-legacy-config` | blocked (4) | blocked (4) | blocked |
| `r-build-rs-new` | blocked (4) | blocked (4) | blocked (by design) |
| `r-rust-toolchain` | blocked (4) | blocked (4) | blocked (by design) |
| `r-zero-filter` | blocked (4) | blocked (4) | blocked (by design: zero tests) |
| `r-filter-some` | delivered (0) | delivered (0) | delivered |
| `r-multi-binaries` | delivered (0) | delivered (0) | delivered |
| `r-multi-quiet` | delivered (0) | delivered (0) | delivered |
| `r-multi-terse` | delivered (0) | delivered (0) | delivered |
| `r-multi-color-cargo` | delivered (0) | delivered (0) | delivered |
| `r-multi-color-libtest` | blocked (4) | delivered (0) | delivered |
| `r-multi-workspace-flag` | delivered (0) | delivered (0) | delivered |
| `r-doc-only-doc-flag` | delivered (0) | delivered (0) | delivered |
| `r-doc-only-plain` | delivered (0) | delivered (0) | delivered |
| `r-no-run` | blocked (4) | delivered (0) | arguably delivered (compile check) |
| `r-nocapture-print` | blocked (4) | delivered (0) | delivered |
| `r-harness-false` | blocked (4) | delivered (0) | delivered |
| `r-cfg-test-module-exit` | blocked (3) | delivered (0) | delivered? (test-only code) |
| `replay-kaisa-workspace-cargo-test` | blocked (4) | delivered (0) | delivered |
| `nextest-default` | delivered (0) | delivered (0) | delivered |
| `nextest-skipped-slow` | delivered (0) | delivered (0) | delivered |
| `nextest-status-level-fail` | blocked (4) | blocked (4) | delivered |
| `nextest-flaky-retry` | blocked (4) | blocked (4) | delivered |
| `nextest-zero` | blocked (4) | blocked (4) | blocked (zero tests) |
| `n-legit-autodetect` | delivered (0) | delivered (0) | delivered |
| `n-legit-node-test-direct` | delivered (0) | delivered (0) | delivered |
| `n-cli-exit-1` | blocked (3) | delivered (0) | delivered (legit CLI) |
| `n-attack-exit-0` | blocked (3) | blocked (4) | blocked |
| `n-attack-exit-destructured` | blocked (4) | blocked (4) | blocked |
| `n-attack-exit-bracket` | blocked (4) | blocked (4) | blocked |
| `n-test-name-ends-js` | blocked (4) | delivered (0) | delivered |
| `n-spec-reporter` | blocked (4) | blocked (4) | delivered |
| `n-two-node-runs` | blocked (4) | blocked (4) | delivered |
| `n-describe-skip-todo` | delivered (0) | delivered (0) | delivered |
| `n-subtests` | delivered (0) | delivered (0) | delivered |
| `n-jest-exit-code-only` | delivered (0) | delivered (0) | delivered (exit-code only) |
| `n-vitest-exit-code-only` | delivered (0) | delivered (0) | delivered (exit-code only) |
| `n-mocha-tap-exit-code-only` | delivered (0) | delivered (0) | delivered (exit-code only) |
| `n-existing-NODE_OPTIONS-read` | blocked (3) | delivered (0) | delivered |
| `n-package-add-lint-script` | blocked (3) | blocked (3) | blocked (pre-existing check definition) |
| `n-npmrc` | blocked (4) | blocked (4) | blocked (by design) |
| `h1-legit-help-text` | delivered (0) | delivered (0) | delivered |
| `h1-legit-autodetect` | delivered (0) | delivered (0) | delivered |
| `h1-cli-entry-process-exit` | blocked (3) | delivered (0) | delivered (legit CLI) |
| `h2-reference-fix` | delivered (0) | delivered (0) | delivered |
| `h2-reference-fix-autodetect` | delivered (0) | delivered (0) | delivered |
| `h3-noop-comment` | blocked (4) | blocked (4) | blocked (base tests fail) |
| `r-self-reexec-test` | blocked (4) | delivered (0) | delivered |
