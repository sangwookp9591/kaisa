# Review follow-up

Addresses both findings in `target/kaisa-review/REVIEW-RWP8.md` against WP8 commit
`3d769727df0803bb1178ed55c40b96d4e756e626`.

## P1 — cancellation owns the complete shutdown

Planned missions and best-of-N CLI runs install SIGINT/SIGTERM handlers that only set a
shared atomic cancellation token. The owner explicitly passes the scoped token into every
worker and attempt broker. Captured processes poll it and stop through the existing
`OwnedProcess::kill_tree` supervision (descendant discovery and process groups on Unix,
Job Objects on Windows), then reap the root and drain captured output.

Running attempts retain the existing terminated exit record, ledger state transition,
preservation and reclamation path, with a cancellation reason in the ledger. Cancellation
blocks new attempts, subsequent waves and final delivery; admitted workers join before
the CLI returns failure. Signal registration and token scope apply only to planned missions
and best-of-N; plain single-goal execution retains its previous signal/deadline behaviour.

Four Unix integration regressions send SIGINT or SIGTERM to the real CLI during a planned
mission or candidate run. Each waits for two actual fake agents and their children in `ps`,
then asserts no live owned processes remain after owner exit, each attempt has terminated
and reclaimed ledger events, pending work was never launched and no delivery was recorded.
Fixture cleanup also runs on assertion failure so a red regression does not leave orphans.

## P2 — record task settlement independently of wave collection

Each mission worker appends `TaskFinished` after its full verification and ownership check
settles, including failure and panic outcomes. Its timestamp and elapsed time now describe
that task's settlement. Admission, scheduler updates and returned results retain their
deterministic wave order; ledger readers immediately remove settled tasks from the running
set.

The asymmetric-duration regression puts the slow task first, waits for its fast sibling's
eligible gate and asserts that the fast task has a successful `TaskFinished` receipt within
one second while the slow task remains running. It also checks both `status` and `tui
--plain` report only the slow task on their running-task line.

## Validation

- Before implementation, `CARGO_NET_OFFLINE=true cargo test -p kaisa --test mission
  cancellation:: -- --nocapture` failed all four signal regressions with surviving roots
  reparented to PID 1 and surviving child processes (`target/wp8b-cancel-before.log`).
- Before implementation, `CARGO_NET_OFFLINE=true cargo test -p kaisa --test mission
  a_finished_task_leaves_status_and_tui_before_its_slow_sibling -- --nocapture` failed with
  both tasks in the ledger running set after the fast task's verified gate.
- The four cancellation regressions passed after the fix; the corrected status/TUI
  regression passed (`target/wp8b-status-after.log`).
- Two default-harness full runs passed all five new regressions but encountered separate
  intermittent failures: the pre-agent recovery fixture observed its released owner lock
  still held (`target/wp8b-check.log`), and a diamond mission check checkout could not resolve
  the common `.git/worktrees` directory during concurrent worktree operations
  (`target/wp8b-check-final.log`). Both named tests passed isolated reruns
  (`target/wp8b-owner-rerun.log`, `target/wp8b-diamond-rerun.log`).
- Final full validation uses `CARGO_NET_OFFLINE=true RUST_TEST_THREADS=1 scripts/check.sh`
  to isolate unrelated test fixtures. Mission and candidate concurrency remains real inside
  each test; the overlap and slot-limit assertions still run. **Exit 0**, including all 18
  mission tests, formatting, workspace clippy/tests, and the installed Windows and Linux
  cross-compilation clippy checks (`target/wp8b-check-serial.log`, ending
  `== all checks passed`).

All agent execution uses `kaisa-fake-agent`; no real Claude/Codex or external network was used.
