# Isolation and authorization regressions

Recorded 2026-11-01, macOS 27.0.1 arm64, source `f3a00c2` plus WP7 docs/demo.
No real model sessions or external network; Cargo offline. Runtime results below
apply to this host only. Linux/Windows cross-checks are compilation, not runtime proof.

Command: `CARGO_NET_OFFLINE=true cargo test --workspace --locked --no-fail-fast`.
[Full output](workspace-tests.txt) is retained; every row below was matched against
its fresh `... ok` line rather than inferred from prior reviews.

| Regression | Test name | Source | Platform | Result |
|---|---|---|---|---|
| Repo-copy edits cannot change the contract; stored approved spec remains immutable | `a_developer_edit_to_a_repo_criterion_fails_and_preserves_the_approved_spec` | [bin/kaisa/tests/spec_flow.rs:395](../../bin/kaisa/tests/spec_flow.rs#L395) | macOS arm64 | PASS |
| Private state write denied | `a_developer_attempt_to_write_private_spec_state_is_refused_without_changing_it` | [bin/kaisa/tests/spec_flow.rs:592](../../bin/kaisa/tests/spec_flow.rs#L592) | macOS arm64 | PASS |
| Private QA inaccessible | `a_confined_developer_cannot_read_a_planted_private_qa_test` | [crates/kaisa-daemon/src/qa.rs:2378](../../crates/kaisa-daemon/src/qa.rs#L2378) | macOS arm64 | PASS |
| Missing criteria identified | `a_requirement_without_criteria_is_reported_before_any_developer_attempt` | [bin/kaisa/tests/spec_flow.rs:452](../../bin/kaisa/tests/spec_flow.rs#L452) | macOS arm64 | PASS |
| Weak assertions flagged | `an_approving_reviewer_cannot_override_a_lint_blocking_test` | [bin/kaisa/tests/spec_flow.rs:374](../../bin/kaisa/tests/spec_flow.rs#L374) | macOS arm64 | PASS |
| Failed QA blocks acceptance | `failing_private_qa_blocks_delivery_even_when_developer_tests_and_reviewer_pass` | [bin/kaisa/tests/spec_flow.rs:353](../../bin/kaisa/tests/spec_flow.rs#L353) | macOS arm64 | PASS |
| Reviewer cannot override gate | `production_staged_approval_cannot_override_a_failed_deterministic_line` | [bin/kaisa/tests/spec_flow.rs:1147](../../bin/kaisa/tests/spec_flow.rs#L1147) | macOS arm64 | PASS |
| Legacy characterized first | `production_legacy_baseline_precedes_real_developer_admission_and_blocks_unexplained_change` | [bin/kaisa/tests/spec_flow.rs:1031](../../bin/kaisa/tests/spec_flow.rs#L1031) | macOS arm64 | PASS |
| Injected ordering fault refuses comparison (not proof of production ordering) | `production_refused_comparison_is_a_fail_with_an_explicit_ordering_reason` | [bin/kaisa/tests/spec_flow.rs:1345](../../bin/kaisa/tests/spec_flow.rs#L1345) | macOS arm64 | PASS |
| No terminal means no human approval | `either_nonterminal_stream_refuses_human_approval` | [crates/kaisa-domain/src/spec.rs:601](../../crates/kaisa-domain/src/spec.rs#L601) | macOS arm64 | PASS |
| Actual PTY confirmation | `a_real_pty_accepts_only_a_persons_correct_digest_and_rejects_agent_markers` | [crates/kaisa-cli/src/spec.rs:590](../../crates/kaisa-cli/src/spec.rs#L590) | macOS arm64 | PASS |
| Provisional is never human approval | `provisional_cli_approval_requires_policy_and_never_displays_or_stores_human_approval` | [crates/kaisa-cli/src/spec.rs:399](../../crates/kaisa-cli/src/spec.rs#L399) | macOS arm64 | PASS |
| Parallel write sets cannot overlap | `a_plan_with_overlapping_parallel_write_sets_is_rejected` | [crates/kaisa-domain/src/plan.rs:209](../../crates/kaisa-domain/src/plan.rs#L209) | macOS arm64 | PASS |
| Parallel admission slot bound | `independent_ready_agents_overlap_but_never_exceed_the_slot_limit` | [bin/kaisa/tests/mission.rs:353](../../bin/kaisa/tests/mission.rs#L353) | macOS arm64 | PASS |
| Worktree metadata race: threads | `mixed_two_thread_worktree_stress` | [crates/kaisa-workspace/tests/worktree_metadata.rs:146](../../crates/kaisa-workspace/tests/worktree_metadata.rs#L146) | macOS arm64 | PASS |
| Process stress: no corruption observed in 100 rounds per worker | `mixed_two_process_worktree_stress` | [crates/kaisa-workspace/tests/worktree_metadata.rs:431](../../crates/kaisa-workspace/tests/worktree_metadata.rs#L431) | macOS arm64 | PASS |
| QA listing shares metadata lock | `qa_worktree_listing_waits_for_an_external_metadata_owner` | [crates/kaisa-workspace/tests/worktree_metadata.rs:214](../../crates/kaisa-workspace/tests/worktree_metadata.rs#L214) | macOS arm64 | PASS |
| Recovery listing shares metadata lock | `recovery_branch_listing_waits_for_metadata_and_recognises_unavailable_worktrees` | [crates/kaisa-workspace/tests/worktree_metadata.rs:302](../../crates/kaisa-workspace/tests/worktree_metadata.rs#L302) | macOS arm64 | PASS |
| Private build script swap blocked | `a_preexisting_build_script_cannot_write_the_external_private_source` | [crates/kaisa-daemon/src/qa.rs:3086](../../crates/kaisa-daemon/src/qa.rs#L3086) | macOS arm64 | PASS |
| Measured routing reliability vs price | `a_cheaper_reliable_model_wins_and_a_cheaper_unreliable_model_loses` | [crates/kaisa-daemon/src/route.rs:666](../../crates/kaisa-daemon/src/route.rs#L666) | macOS arm64 | PASS |
| Routing unknown price not zero | `an_unknown_cost_is_not_cheaper_than_a_complete_usd_receipt` | [crates/kaisa-daemon/src/route.rs:697](../../crates/kaisa-daemon/src/route.rs#L697) | macOS arm64 | PASS |
| Security routing cannot explore | `security_role_never_explores_even_without_caller_signals` | [crates/kaisa-daemon/src/route.rs:617](../../crates/kaisa-daemon/src/route.rs#L617) | macOS arm64 | PASS |

The injected-ordering-fault regression sets `bench.poison_admission = true`; production
baseline-before-developer ordering is checked separately by “Legacy characterized first.”
The process stress row records an observation, not lock compliance;
`another_process_waits_for_the_common_directory_lock`
(`crates/kaisa-workspace/tests/worktree_metadata.rs:441`) checks that a cooperating
process waits for the lock. The repo-copy regression does not prove state write denial;
the next row exercises that boundary.

The planted-private-test test exercises confined read denial. It does not prove
confidentiality in observed/unconfined modes. The PTY regression demonstrates
correct confirmation, wrong-digest rejection and attempt-marker refusal; it does
not establish cryptographic human identity. See [limitations](limitations.md).

Cargo summaries from this run (verbatim):

```text
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.25s
test result: ok. 34 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 7.63s
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.79s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.55s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.98s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 17.77s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.29s
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 10.05s
test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 28.45s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.24s
test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.00s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.26s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.87s
test result: ok. 56 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 85 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.70s
test result: ok. 67 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.69s
test result: ok. 181 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.86s
test result: ok. 132 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.82s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok. 74 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.90s
test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.26s
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 62 filtered out; finished in 0.00s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 62 filtered out; finished in 0.03s
test result: ok. 63 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.21s
test result: ok. 5 passed; 0 failed; 5 ignored; 0 measured; 0 filtered out; finished in 0.28s
test result: ok. 127 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 13.57s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 0.09s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 0.26s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 0.13s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 7.20s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 10.16s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 15.24s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.08s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

## Final local check

`CARGO_NET_OFFLINE=true RUST_TEST_THREADS=1 scripts/check.sh` exited **0**.
[Full output](check.txt) ends with `== all checks passed`: stable/nightly formatting,
workspace clippy, workspace tests and installed Windows/Linux cross-clippy passed.
The 55 emitted Cargo summaries total 1083 passed, 0 failed and 6 ignored; this
is a sum of emitted summaries, including nested fixture/doc-test summaries, not
a count of distinct security properties. Cross-checks are compile-only.

The newly added example regression
`documented_offline_spec_demo_delivers_only_the_passing_candidate` at
[bin/kaisa/tests/spec_demo.rs:5](../../bin/kaisa/tests/spec_demo.rs#L5) also passed
on macOS only (`#[cfg(target_os = "macos")]`; Linux/Windows do not run it),
both in the full check and in a fresh targeted run after strengthening the script's domain/staged-review assertions. [Targeted output](demo-test.txt).
