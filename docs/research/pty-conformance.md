# PTY layer: conformance comparison (ADR 0005)

| | |
|---|---|
| Date | 2026-10-30 |
| Suite | [`crates/kaisa-sys/tests/pty_conformance.rs`](../../crates/kaisa-sys/tests/pty_conformance.rs) — five scenarios written once against a test-only `Backend` trait |
| Candidates | Kaisa's own wrapper (`kaisa-sys`: `openpty` + `setsid`/`TIOCSCTTY`, ConPTY + `CreateProcessW` in a Job Object) · `portable-pty` 0.9.0 (wezterm, MIT, last release 2025-02-11) |
| Evidence | CI run 41025652626 (commit `a1fa06c`), GitHub-hosted `ubuntu-latest`, `macos-latest`, `windows-latest` |

## Results

| Scenario | Kaisa macOS | Kaisa Linux | Kaisa Windows | portable-pty macOS | portable-pty Linux | portable-pty Windows |
|---|---|---|---|---|---|---|
| shows what the agent prints | pass | pass | pass | pass | pass | **hung** (> 60 s) |
| delivers typed input | pass | pass | pass | pass | pass | **fail** |
| Korean and emoji round trip | pass | pass | pass | pass | pass | **fail** |
| reports a resize to the agent | pass | pass | pass | pass | pass | **fail** |
| keeps output written just before exit (2000 lines, then exit) | pass | pass | pass | pass | pass | **hung** (> 60 s) |

Caveat: portable-pty ran through an adapter written for this suite. Its Windows failures may partly
come from that adapter rather than from the library; the comparison job keeps running them so the
result can change. Even so, the adapter had to implement the drain and close ordering itself — the
part both macOS and Windows punish when done in the wrong order — because portable-pty leaves it to
the caller.

## Decision

**Kaisa keeps its own wrapper** (ADR 0005 accepted), because:
1. It passes every scenario on all three OSes, including the drain ordering that macOS (an agent
   cannot finish exiting while its output is unread) and Windows (closing the pseudo console early
   loses the last output) each require.
2. It is part of process ownership: the agent in a terminal is still an `OwnedProcess` — a process
   group (and pidfd) on Unix, a Job Object created suspended on Windows. portable-pty spawns its own
   child and offers no tree containment, so Kaisa would have to give up `kill_tree` guarantees or
   duplicate the spawn path.
3. Dependency weight: portable-pty pulls in `nix`, `winapi`, `anyhow` and more for a layer that is a
   few hundred lines of `kaisa-sys`.

The comparison stays in CI as a non-blocking, time-boxed job (`pty comparison`) so the decision can be
revisited with data.
