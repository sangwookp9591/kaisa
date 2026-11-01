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

# Review follow-up 2

Addresses the blocking shared-repository worktree metadata race and the small live-status
gap identified in `target/kaisa-review/REVIEW-RRWP8.md` against `f0727ea`.

## Shared worktree metadata serialization

Both workspace Git runners now serialize every `worktree` subcommand, including add,
remove, prune, move, repair and listing. They discover the canonical common Git directory
using the same Git configuration and pinned metadata as the command, take its in-process
mutex, then take an advisory OS file lock at `<common-git-dir>/kaisa-worktree.lock`.
The lock file is permanent, outside `worktrees/`, and is never unlinked; closing its handle
releases ownership on success, error, unwind or process exit. No dependency or unsafe code
was added. Linked worktree entry points and independent processes converge on the same
lock, while unrelated repositories remain independent.

The production mutation audit found only the workspace attempt add/remove and detached
`CandidateCheckout` add/drop paths. Trusted verification (`verify.rs`), reviewer and
alternative checkouts, integration, scheduling and recovery all reach these APIs; recovery's
remaining direct `worktree list` is read-only. The critical section contains one Git
worktree command, never agent/check execution, preservation inventory or ledger work.
Lock order is documented in the implementation and architecture: attempt owner lock when
present, repository mutex, advisory file lock. The registry mutex is released before
waiting, and no operation under the repository lock acquires an attempt owner lock.

`crates/kaisa-workspace/tests/worktree_metadata.rs` retains the reviewer's isolated mixed
case through the real public APIs: two threads perform 150 detached and 150 branch
creations/removals, with metadata integrity and complete cleanup assertions. An initial
held metadata lock makes contention observable without relying only on the rare Git
failure. Two child test processes additionally perform 100 detached and 100 branch
creations/removals; they are killed and reaped on failure and have bounded waits.
Separate regressions cover both add APIs through a linked worktree, both removal paths,
cross-process exclusion, and progress in an unrelated repository.

## Task settlement precedes eligibility

Planned-task verification returns its pending Eligible event to the owning mission worker.
The worker completes the ownership check, records `TaskFinished`, then publishes eligibility
only for a successful task. Alternative-candidate and plain-goal gate behavior is retained.
The status/TUI regression now samples progress immediately when eligibility appears and
asserts settlement precedes that gate in ledger order, with no one-second grace period.
The outside-write-set regression additionally refuses an Eligible event for failed ownership.

## Validation

- The advisory-lock regression failed against the original implementation with
  `worktree metadata changed while another owner held its advisory lock`
  (`target/wp8c-lock-before.log`).
- The final committed mixed-thread test was copied into an isolated, dependency-free
  workspace crate exported from pre-fix HEAD and run with
  `CARGO_NET_OFFLINE=true CARGO_INCREMENTAL=0 cargo test --manifest-path
  target/wp8c-before/Cargo.toml --test worktree_metadata mixed_two_thread_worktree_stress
  -- --nocapture`. It failed in 12.89 seconds with `could not create directory of
  '.git/worktrees/check-2': No such file or directory`, reproducing the actual metadata race
  (`target/wp8c-stress-before-final.log`). An earlier unguarded stress run passed, as expected
  for an intermittent race; it is not claimed as red evidence.
- The strict status test failed before its fix with `fast settlement precedes eligibility`
  (`target/wp8c-status-before.log`) and passed afterward (`target/wp8c-status-after.log`).
- The initial six metadata regressions passed together after a clean rebuild
  (`target/wp8c-metadata-after.log`). The first full default-parallel script run, also
  including the added removal regression, passed (`target/wp8c-check-1.log`).
- Two full runs of `CARGO_NET_OFFLINE=true CARGO_INCREMENTAL=0 scripts/check.sh`
  passed with default parallelism (no `RUST_TEST_THREADS` override), including formatting,
  workspace all-target clippy/tests, and the installed Windows/Linux cross-compilation
  checks (`target/wp8c-check-1.log`, `target/wp8c-check-2.log`, both exit 0 and ending
  `== all checks passed`). The second run includes the final contention assertion;
  all seven metadata regressions, all 18 mission tests and all 30 team tests passed.

Cargo used offline mode throughout. All agent execution used `kaisa-fake-agent`; no real
Claude/Codex or external network was used. Initial scratch stress validation encountered
disk exhaustion; generated review build caches were cleaned before the successful runs.
