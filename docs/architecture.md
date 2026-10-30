# Kaisa Architecture

| | |
|---|---|
| Status | Draft v0.6.1 — M0 substrate in progress (ledger, keeper, workspace, PTY); everything else is design |
| User flows | [`user-flows.md`](user-flows.md) — the contract this architecture serves |
| Date | 2026-10-30 |
| Platforms | macOS, Linux, Windows (all three are release blockers) |
| Core language | Rust (stable 1.99.1, edition 2024) |
| Evidence | [`research/prior-art-rea.md`](research/prior-art-rea.md) (REA analysis of Orca, LearnHarness, ruflo) · [`research/ruflo-control.md`](research/ruflo-control.md) (how ruflo enforces, or only records, its capabilities) |
| Reviews | [`reviews/2026-10-30-architecture-v0.2-codex.md`](reviews/2026-10-30-architecture-v0.2-codex.md) · [`reviews/2026-10-30-architecture-v0.6-codex.md`](reviews/2026-10-30-architecture-v0.6-codex.md) (findings B1–B3, M1–M12, N1; addressed in v0.6.1, Appendix C) |

> Every performance or memory number here is a **budget to be measured**, not a claim.
> Every security property is a **requirement to be proven by a negative test on each OS**
> before it is advertised.

**Changes in v0.6.1 — independent review of v0.6.** A Codex review found 3 blockers and 12 major
issues ([`reviews/2026-10-30-architecture-v0.6-codex.md`](reviews/2026-10-30-architecture-v0.6-codex.md));
all are addressed, mapped in Appendix C.

**Changes in v0.6 — redesign of the team-intelligence layer.** An audit against eight required
capabilities found four of them only bolted on (project memory, self-learning retrieval, context
scoping, routing judgement) and two missing (web console, large-scale teams). They are now part of
the core model instead of optional extras. The M0 substrate (ledger, keeper, PTY, workspace,
preservation) is unchanged: it is the execution path every new capability depends on.
- **Principle P17**: a capability exists only where Kaisa is on the execution path. P15 is restated:
  learning adapts inside bounds and never widens authority (§2).
- **Missions and hierarchical runs** with sub-coordinators; a `documentation` role; lightweight
  read-only attempts; write-set ownership as a scheduler invariant (§5.2, §5.13).
- **The verification loop** is a state machine with structured review verdicts, automatic bounded
  revision and one delivery gate on fresh evidence (§5.8).
- **Knowledge and the context engine** are first-class and event-sourced: an auto-derived project
  profile, lessons, decisions and trajectories; hybrid BM25 + vector retrieval on by default; context
  packs assembled by Kaisa, with every injection logged and scored against outcomes (§5.9).
- **The router sits on the spawn path**: a deterministic grader, per-(role, grade) learned priors
  from verified outcomes, local OpenAI-compatible models for the light tier, per-request usage (§5.10).
- **A local web console** beside the TUI, on loopback with a per-launch token (§5.11, §12).
- **Agent catalog** adds Hermes and generic OpenAI-compatible endpoints; user MCP servers are relayed
  through the broker (§5.6).
- ADR-0028 to ADR-0035 were added.

**Changes in v0.5.** These come from [`user-flows.md`](user-flows.md) v0.2 §8.
- **Answers and Decisions** are now typed differently. Decisions carry a scope and an expiry, and questions have an owner, a timeout and a fallback (§5.12).
- **An effect preview API** is used by approve, undo and multi-repo work (§5.12).
- **Second-person approval rules** and reviewer reassignment were added (§5.12).
- **A CI exit-code contract** and `run --resume` were added (§6).
- **Workspace creation** now handles a dirty tree, LFS and submodules (§5.5).
- **Admin lifecycle** was added: enrollment, staged policy-pack rollout and rollback, offboarding receipts, template tests and agent conformance (§5.11).
- ADR-0023 to ADR-0027 were added.

**Changes in v0.4.** These come from [`user-flows.md`](user-flows.md) §8.
- A third authority level, **Requester**, for runs started from inside agent sessions (§5.7).
- A new §5.11, **Experience services**:
  - notifications with presence-checked approval;
  - the issue-tracker connector;
  - human takeover and hand-back;
  - undo and restore;
  - plan templates and signed policy packs with approval delegation;
  - localization and the plain-language event renderer.
- The user command surface now matches the flows (§4).
- ADR-0017 to ADR-0022 added.

**Changes in v0.3**

- **Review findings addressed.** All findings of the v0.2 review are addressed:
  - B1–B6: authority split, containment matrix, execution broker, quiescent failover, per-attempt keeper, and preservation-safe reclamation.
  - M1–M10 and m1–m4 are also covered; Appendix A maps each finding to its section.
- **Adopted from ruflo** (Appendix B):
  - the role table and event-driven pipeline advance;
  - verified-outcome learning and its gaming defences;
  - the compaction snapshot and the gateway-enforced budget;
  - project-settings risk refusal, the context-footprint budget and the deterministic tier 0;
  - no self-promotion of learned changes.
- **Milestones reordered.** Minimum safety moves into M0. Knowledge, best-of-N, the desktop app and remote work are deferred.

---

## 1. Purpose

Kaisa is a company-internal control plane for coding agents such as Claude Code, Codex, Hermes and
other CLI agents. It turns one goal into the work of a verified team. It does eight things:

1. **Runs** agents, each in an isolated git worktree and terminal.
2. **Supervises** them with a lifecycle the system enforces. The agents do not have to remember it.
3. **Contains** them with an OS sandbox and a brokered tool path.
4. **Verifies** results against an immutable snapshot before a human sees them.
5. **Keeps work moving** when a provider's quota runs out, by handing the coordinator role or a
   task to another provider.
6. **Remembers** the project: its profile, decisions and lessons survive every session, and the right
   part of them reaches each agent without anyone re-explaining the project.
7. **Learns** only from verified outcomes, and routes each task to the cheapest agent and model that
   has proven it can do that kind of task.
8. **Shows** the whole team live — progress, cost and what needs a human — in the terminal and in a
   local web console, and **accounts** for cost per run, team and model.

### Goals

- One small core binary per platform. It needs no Node, Python or Electron runtime. OS frameworks and
  optional model assets are allowed.
- Equivalent behaviour on macOS, Linux and Windows. Where a guarantee differs between OSes, the
  difference is shown and the run fails closed.
- Secure by default. Humans and agents hold separate authority. Policy denies by default and the
  sandbox is enforced. Secrets are held in the keychain, and the audit log is tamper-evident.
- Vendor neutral. Agents are adapters described by data, and any provider can coordinate.
- Small surfaces:
  - Users get `run`, `watch` and `review`.
  - Agents get a few MCP tools.
  - The per-turn context footprint is budgeted and measured.

### Non-goals (v1)

- Hosting large language models. Kaisa calls them; it runs only small local models for embeddings,
  and it can call a local model server (for example Ollama) that the user runs.
- A general-purpose IDE.
- Multi-tenant SaaS. v1 runs locally for one user. Team server mode comes later.
- Distributed consensus. One daemon is the authority for its runs.

---

## 2. Design principles

| # | Principle | Learned from |
|---|---|---|
| P1 | **One daemon, one protocol.** | Orca has three server roles and 53 daemon protocol versions. |
| P2 | **Execute, don't just record.** Operations that only write to the ledger are labelled as such. | ruflo `swarm_init`, `agent_spawn` and `team_spawn` return records or plans and execute nothing. |
| P3 | **The system carries the obligations, not the LLM.** | Orca expects workers to send heartbeats and `worker_done`, and coordinators to `release`. In ruflo, team formation depends on the model following CLAUDE.md. |
| P4 | **Own the process durably, through a per-attempt keeper.** | Orca owns terminals and not agents. A daemon restart loses pidfd and kqueue registrations (review B5). |
| P5 | **Reclaim by default, preserve before deleting, retain by request.** | Orca leaves terminals open until they are released. Naive WIP commits lose work and leak secrets (review B6). |
| P6 | **The coordinator is a role with an epoch, not a session.** | Orca's `handoff` needs the old coordinator, which may have no tokens left. |
| P7 | **Mechanical coordination is code.** Scheduling, waiting, retry rules and cleanup cost no tokens, and pipelines advance on events. | Orca's LLM-driven supervision loop; ruflo's `SubagentStop` → `team_on_stop`, which only produces a hint. |
| P8 | **Authority comes from the current epoch.** Stale attempts and stale coordinators are fenced at every mutation. An ambiguous effect is never retried automatically. | Orca's fencing and `*_unknown` stages. |
| P9 | **Effects go through an outbox.** Intent, event and projection commit in one transaction, and effects are reconciled on recovery. | Review M2. |
| P10 | **Humans and agents never share authority.** | Review B1. |
| P11 | **Enforce at the broker and at the OS boundary. Hooks are advisory.** | Review B3. |
| P12 | **One writer per store.** | LearnHarness; Orca. |
| P13 | **No silent degradation.** If a guarantee is reduced, the reduction is labelled. | ruflo's hash-embedding fallback. |
| P14 | **Read-only commands have no side effects.** `status`, `--help`, `--version` and MCP `tools/list` do not start the daemon, write files or refresh anything. | ruflo's helper refresh and daemon autostart. |
| P15 | **Learning adapts inside bounds and never widens authority.** Knowledge and routing priors update automatically from verified outcomes, and can be rolled back. Anything that changes policy, prompts, tools, egress, secrets, spend or concurrency stays a proposal behind a human Gate. | ruflo's MetaHarness / Darwin rule; the v0.6 audit found that "proposals only" made Kaisa unable to get better by itself. |
| P16 | **Measured claims only.** | ruflo retracted its benchmark multipliers. |
| P17 | **A capability exists only where Kaisa is on the execution path.** If Kaisa cannot enforce it — because it only prints a recommendation or writes a record — it is not claimed. Model choice, context, ownership and delivery are all decided where Kaisa starts or finishes an attempt. | ruflo: recommendations the model may ignore, `maxAgents` never checked, exit 0 treated as success ([`research/ruflo-control.md`](research/ruflo-control.md)). |
| P18 | **Context is assembled, not remembered.** Every attempt starts from a context pack Kaisa builds: spec, owned files, related code, relevant knowledge. No agent is expected to search for what it needs or to recall earlier sessions. | Context collapse in long single-agent sessions; ruflo's keyword-matched 80-character injections. |
| P19 | **Every injection is accountable.** Each knowledge item given to an agent is logged with the attempt, and its usefulness is scored against verified outcomes. Items that do not help decay. | ruflo scores recall by "matched again", which is not an outcome. |

---

## 3. System context

```
 Human ── kaisa CLI / TUI / Desktop ──► CONTROL endpoint (human authority; agents cannot reach it)
                                              │
┌─────────────────────────────────────────────▼──────────────────────────────────────────────┐
│ kaisad                                                                                     │
│  Scheduler (missions, runs, write sets) · Coordinator epochs · Router/Budget/Quota          │
│  Verify loop + delivery gate · Knowledge + context engine · Event stream (TUI / web)       │
│  Outbox + Ledger (single writer, SQLite WAL, hash-chained events)                          │
│  Policy compiler ─► signed, immutable policy snapshots                                     │
│  AGENT endpoint (per-attempt stdio MCP bridge; scoped capability token; no human ops)      │
│  Egress broker (per-attempt HTTP(S) proxy with allow-list)                                 │
└───────┬───────────────────────────────────────────────────────────────────────┬────────────┘
        │ supervises (reconnects after restart)                                 │
┌───────▼──────────────┐   per attempt                                ┌─────────▼───────────┐
│ kaisa keeper         │── holds PTY master + process handle/job ───► │ Agent CLI           │
│ (tiny, survives      │   writes durable exit record                 │ in sandbox +        │
│  daemon restart)     │◄── execution broker requests (headless) ──── │ worktree            │
└──────────────────────┘                                              └─────────────────────┘
```

---

## 4. Process model

There is one core executable, `kaisa` (`kaisa.exe` on Windows). The desktop client is a separate GUI
package, delivered at M3.

| Role | Invocation | Lifetime | Notes |
|---|---|---|---|
| Daemon | `kaisa daemon` | Long-lived | Sole writer of the ledger. Exactly one instance per user, enforced by an OS lock (`flock` / named mutex). Exits when idle. |
| Keeper | `kaisa keeper` (spawned by the daemon) | One attempt | Holds the PTY master, the process handle or Job Object, and the sandbox. Writes a durable exit record. The daemon reconnects to keepers after it restarts. |
| CLI | `kaisa <command>` | One command | Client of the control endpoint. Starts the daemon only for mutating commands (`run`, `steer`, `handoff`, …). Read-only commands report "not running" (P14). |
| TUI | `kaisa tui` | Interactive | Control client built on ratatui. |
| Hook | `kaisa hook <event>` | Milliseconds | Advisory local check (§5.7). Reports to an authenticated spool. |
| MCP bridge | `kaisa mcp --attempt <id>` (stdio, launched by the agent from its MCP config) | One agent session | Agent endpoint only. Carries the attempt's capability token. No loopback HTTP listener is needed. |

**Discovery.** A file `runtime.json` sits in the user state directory: `~/Library/Application Support/kaisa`,
`$XDG_STATE_HOME/kaisa` or `%LOCALAPPDATA%\kaisa`. It holds the control endpoint, the pid, the start time and
the protocol version. It holds no secret, and the sandbox profile makes it unreadable to agents.

**User surface (v1).** The full list and the flows behind it are in [`user-flows.md`](user-flows.md) §7.

| Level | Commands |
|---|---|
| Everyone (three verbs) | `kaisa "<goal>"` · `kaisa watch` · `kaisa review` · `kaisa` (Home TUI) |
| Common | `ask`, `attach`, `steer`, `pause`, `resume`, `cancel`, `undo`, `restore`, `init`, `doctor`, `status` |
| Advanced | `run` flags (`--n`, `--agents`, `--template`, `--plan`, `--budget`, `--issue`), `retain`, `gc`, `handoff` (planner), `handback` (attempt), `wrap` |
| Team / admin | `team`, `admin publish`, `audit verify`, `audit export` |
| Automation | `run --headless --json`, `--issues <query>` |

- `kaisa init` previews every change: the hooks, the MCP config, and any risky project settings it found. Nothing is applied until the user confirms.
- Read-only commands never start the daemon (P14).
- Every command supports `--json`, `--help` and the `ko`/`en` locales.

---

## 5. Engine cores

### 5.1 Supervisor: keeper, processes, PTYs and leases

**Keeper (review B5).** Each attempt gets one `kaisa keeper` process. The pattern is the same as podman's `conmon` and the tmux server.
- **What it holds.** The keeper holds the PTY master and the OS handle:
  - Linux: pidfd plus a cgroup v2 leaf;
  - macOS: a process-group leader, a `kqueue` watch, the start time and a run token;
  - Windows: the Job Object handle, created with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` disabled, so the keeper decides when to kill.
- **Exit records.** When the agent exits, the keeper writes an **exit record** to the attempt directory with an fsync: the exit status, the time and the incarnation.
- **Daemon restarts.** If the daemon restarts, it finds keepers by their per-attempt sockets and reconnects.
- **Keeper loss.** If a keeper is lost, the process state stays `Unknown` until it can be proven otherwise. It is never converted into a retry or a destructive GC.

**Containment of descendants**

| OS | Mechanism | Escape risk |
|---|---|---|
| Linux | cgroup v2 leaf per attempt (`cgroup.kill`, `cgroup.freeze`) | Low, if cgroup v2 is delegated. Without delegation the attempt runs in a **reduced** mode (§5.7). |
| macOS | process group plus a periodic descendant scan (`proc_listchildpids`). Seatbelt denies `setsid` / `setpgid` where the profile allows it. | Medium. This is documented, and the descendant scan runs before every settle. |
| Windows | Job Object assigned at creation (`CREATE_SUSPENDED` → assign → resume). Breakaway is denied. | Low. |

**PTY layer**
- macOS and Linux use `openpty`. Windows uses ConPTY.
- The implementation is chosen by a **conformance suite** (§16) that compares our `kaisa-sys` wrapper with mature wrappers such as `portable-pty` (review M9). Release age alone does not decide.
  Result (ADR 0005, accepted): Kaisa's own wrapper — see [`research/pty-conformance.md`](research/pty-conformance.md).
- All `unsafe` code lives in `kaisa-sys`.

**State dimensions (review M1).** An attempt has four independent state axes. Overloading one enum is what made v0.2 ambiguous.

| Axis | Values |
|---|---|
| Execution | `Admitted, Starting, Running, Stalled, Quiescing, Quiesced, Exited{code,signal}, Unknown{stage}` |
| Outcome | `Pending, Completed, Cancelled, Failed{reason: auth\|crash\|policy\|timeout\|agent}` |
| Verification | `NotStarted, Running, Passed{snapshot}, Failed{snapshot}, Tampered{snapshot}` |
| Cleanup | `Held, Preserving, Preserved{receipt}, Reclaiming, Reclaimed, Retained` |

The rules that connect the axes:
- **Exit is not success.** An exit with code 0 moves Outcome to `Completed`, but the result still has to pass verification. It does not bypass verification.
- **Stalls are bounded.** `Stalled` is entered when there is no observed activity for `stall_after`. The run policy then escalates in fixed steps: wait, then steer, then restart on another provider, then a Gate. The number of steps is fixed.
- **Leases cannot be renewed forever.** Activity renews a lease only up to `max_attempt_duration`. Deadlines use a monotonic clock. After a reboot, a missed deadline is evaluated against wall time and recorded as such.

**Output.**
- Each attempt keeps a 256 KiB ring buffer.
- Logs are written as zstd segments with a blake3 digest, under a retention policy, and secrets are redacted on write.
- Terminal state is held in `alacritty_terminal`, with a versioned screen-snapshot format. The format covers cells, attributes and wide characters, and scrollback is bounded (review m4).
- Kaisa drains post-exit output up to a configurable limit. If output is truncated, it is labelled as truncated.

**Agent state signals, in priority order:** hook events, then structured stream events, then process and cgroup or job activity, then terminal heuristics (labelled as a fallback).

### 5.2 Scheduler: deterministic orchestration

| Concept | Meaning |
|---|---|
| **Mission** | What the user asked for (`kaisa "<goal>"`). It owns the budget, the policy snapshot and the delivery: a Mission is done only when its result passes the delivery gate (§5.8). |
| **Run** | A plan under one coordinator epoch. A Mission has one root Run; a large Mission splits into **sub-runs**, each with its own coordinator and a slice of the budget (§5.13). |
| **Task** | A unit of work with a self-contained spec (target, change, constraints, acceptance), a **role**, dependencies, a **write set** (paths it may change) and a **grade** (§5.10). |
| **Attempt** | One execution of a Task by one agent or model in one workspace. It carries a `generation`. |
| **Gate** | A decision that needs **human authority** or policy. It is required at every permission boundary and for every uncertain effect. |

**Roles decide containment and tools.** This extends ruflo's team role table.

| Role | Workspace | Sandbox profile | Tool set | Default model grade |
|---|---|---|---|---|
| researcher | none (read-only mount) | read-only, egress allow-list | read, search, knowledge | T0–T1 |
| architect | none | read-only | read, search, plan proposal | T2 |
| developer | own worktree | write in worktree, exec via broker | read, write, edit, exec, search | T1–T2 |
| tester | own worktree | write in worktree, exec via broker | read, write, exec | T1 |
| reviewer | clean snapshot checkout | read-only, no egress | read, diff, report | T1–T2, different model from the implementer |
| security | clean snapshot checkout | read-only, no egress | read, diff, report | T3 floor (`never_downgrade`) |
| documentation | own worktree, write set limited to docs paths | write in worktree, no exec | read, write, edit, search | T1 |
| coordinator | none | no filesystem access; no exec | coordinator MCP tools only | T2 |

This table is the **single source** of each role's default grade and floor (v0.6.1, review M11). The
router (§5.10), templates and flows read it; a template or policy that sets a role below its floor
fails validation. Final user-facing output (the Mission summary and delivered answers) has a T3 floor
as well.

**Lightweight attempts.** Read-only roles (researcher, architect, reviewer, security) need no
worktree of their own: they share one read-only snapshot checkout per revision, and their keeper
has no worktree, write scratch or egress beyond review needs. Every attempt still has its own
keeper (ADR-0013), because ownership and exit records are per attempt. A heavy attempt (developer,
tester, documentation) gets a worktree, a keeper and a full sandbox. This is what makes large teams affordable (§5.13); the cost of each kind is a measured
budget (§15).

**Write-set ownership (scheduler invariant).** Every writing Task declares its write set as path
globs, proposed by the coordinator and checked by Kaisa against the repository index (§5.9).
- Two Tasks whose write sets overlap are never in progress at the same time. The scheduler orders
  them; it does not ask the LLM to remember.
- Writes outside the write set are impossible for **every** process of the attempt, not only for the
  broker's `fs.write`: the OS sandbox grants write access to the write-set paths and to the
  attempt's scratch area only (§5.7, "Write-set confinement"). A refused write is reported to the
  coordinator as a question ("this change also needs `src/auth/session.rs` — widen the write set?").
- The **impact set** — files that import or test what the write set changes, from the repository
  index — is added to the Task's verification (§5.8). This is how "fixing A broke B" is caught
  before delivery instead of after.

**The daemon does the mechanical work.**
- **Dispatch.** Ready Tasks are dispatched in waves under the concurrency, budget and admission limits (§5.10).
- **Pipelines advance on events, not on hints.** When an Attempt settles and passes its gate, the Scheduler starts the dependent Tasks itself. This takes ruflo's `SubagentStop` → `team_on_stop` idea and removes the step where "the lead must spawn the next one".
- **Retries and failover follow policy.** A start or effect in `Unknown` is never retried automatically.

**Correctness mechanisms (internal)**
- **Epoch fencing at every mutation.** Every mutating call carries `(run, coordinator_epoch)` or `(attempt, generation)`. A call with a stale epoch is rejected and recorded.
- **Outbox (review M2).** A mutation that causes an external effect commits its intent, its event and the projection in **one SQLite transaction**. The effect types are spawn, merge, push, branch delete and file archive. An effect executor then performs the effect and records an **effect receipt**.
  - **Recovery.** On recovery, pending intents are reconciled: an idempotent effect is re-run, a probeable effect is probed, and anything else opens a Gate.
  - **Request ids.** Request-id receipts reject a reused id that comes with a different payload.
- **Epoch-scoped cursor (review M3).** Cursors are keyed by `(consumer, run, epoch)`.
  - A consumer can confirm only up to the highest **contiguous sequence it was actually delivered**.
  - A decision tool call carries the cursor position it was based on, and the daemon rejects decisions made from a stale view.
- **Steer states (review M3).** A steer moves through `Queued → Injected → Acknowledged`, and `Injected` can become `Uncertain`. PTY injection has an unavoidable ambiguous window. Kaisa exposes `Uncertain` instead of promising exactly-once execution.

**Worker obligations: none required.** The optional worker tools are:

| Tool | Purpose |
|---|---|
| `kaisa_report` | A structured outcome summary. |
| `kaisa_ask` | A blocking question to the coordinator or a human. |
| `kaisa_knowledge` | Verified knowledge search (when Knowledge is enabled). |

### 5.3 Coordinator role and failover

The coordinator is the LLM part of a run: it decomposes the work, judges and answers. It holds the role under
an **epoch** and has no filesystem or exec access. It can only call coordinator tools.

| Coordinator tool | Effect |
|---|---|
| `kaisa_plan` | Propose Tasks or dependencies, transactionally, under the epoch. |
| `kaisa_decide` | Record a decision, its rationale and the cursor position it was based on. This is the journal. |
| `kaisa_events` | Read events from the epoch-scoped cursor. |
| `kaisa_answer` / `kaisa_steer` | Answer questions and redirect attempts. |
| `kaisa_gate` | Request a human decision. |

**Handoff packet (review M4).** The daemon assembles the packet from structured state. It is versioned and
content-addressed, and it contains:
- the snapshot sequence number, the epochs and the policy version;
- the objective, the constraints and the budget state;
- the Task DAG with status and base revisions;
- the decision journal and consumed cursors;
- open questions and Gates;
- active attempts with their artifact digests and latest verification receipts;
- pending outbox effects and unresolved tool calls;
- the compaction snapshot of the outgoing coordinator's last context, if one is available (ruflo ADR-505);
- the remaining next actions.

Unjournaled reasoning is lost by design. Making meaningful decisions go through `kaisa_decide` is what
makes the coordinator's work survive a handoff.

**Coordinator failover (review B4)**

```
quota/failure signal (classified, §5.10)
 1. revoke admission: coordinator epoch e → e+1 (all calls tagged e are now rejected)
 2. drain: cancel the old coordinator's in-flight tool calls; stop its process (keeper)
 3. reconcile: pending outbox effects from epoch e → completed / probed / Gate
 4. snapshot: assemble packet at sequence N
 5. grant: start successor with packet + coordinator tools under epoch e+1
 6. successor reconciliation step (read packet, confirm or amend plan) before any new dispatch
 7. ledger: coordinator_handoff{from, to, reason, packet_digest}
```

Workers keep running during a coordinator handoff, because the daemon and their keepers own them.

**Worker failover**

```
 1. revoke: attempt generation g → g+1; broker rejects calls tagged g
 2. quiesce: freeze (cgroup.freeze / SuspendThread via job / SIGSTOP group), then stop the process
 3. prove quiescence: keeper exit record or frozen state confirmed; else Unknown → Gate
 4. preserve: §5.5 preservation protocol (inventory + receipt)
 5. start superseding attempt on next provider with spec, preserved diff, verification receipts,
    attempt event digest and compaction snapshot
```

**Rules**
- Fallback is **capability-based**, not "weaker tier". Each step declares the capabilities it requires, and only providers that meet them are eligible. Steps marked `never_downgrade` pause and open a Gate.
- If no provider is available, the run pauses. Mechanical work continues: running attempts keep running and verification keeps going.

### 5.4 Ledger: storage engine

- **Database.** SQLite through `rusqlite` 0.41, bundled, in WAL mode with `synchronous=FULL` for the
  ledger. The file is `kaisa.db`, with one single-writer actor.
- **Event log.** The `events` table is the source of truth, with a blake3 hash chain.
  - Kaisa writes **signed checkpoints** every N events. The key is held in the OS keychain.
  - Checkpoints can be exported to an external anchor, a git notes ref or a remote store (review m1).
  - The limit is documented: a same-user attacker who can read the keychain can forge history.
- **Schema evolution (review Q2).**
  - Events carry a version and are **upcast** when read.
  - Projections can be rebuilt from events.
  - Upgrade and recovery tests replay recorded event logs from earlier versions.
- **Disk full.** The writer refuses new runs, keeps the effects of running ones journaled, and opens a Gate.

### 5.5 Workspace: isolation and preservation

**Worktrees.**
- Mutations go through the `git` CLI. Reads go through `gix` 0.90.
- A conformance suite checks that `gix` agrees with `git` on submodules, symlinks, long paths and sparse checkouts (review M9).
- On Windows, worktrees use `\\?\` paths and `core.longpaths=true`.
- Shared `.git` metadata is mounted read-only into sandboxes, except for the attempt's own worktree refs.

**Source state (user-flows F31).** Worktrees are created from **committed** state by default.
- **Dirty tree.** If the user's tree is dirty, the run request records one of three choices:
  - `committed-only` (the default);
  - `include-uncommitted`: Kaisa snapshots the tracked and untracked changes into a temporary commit object, without touching the
    user's branch. The digest of that snapshot becomes the attempt's base revision;
  - `commit-first`.
- **Submodules** are checked out at their recorded commits.
- **Missing LFS objects** are reported. They are then fetched through the egress broker or skipped, according to policy, and the decision is recorded.
- **Sparse cones** are preserved.
- **Preview.** The user sees what the copy will contain before the attempt starts (preview API, §5.12).

**Copy-on-write warm-up.**
- Large directories are cloned with APFS `clonefile`, `FICLONE` or ReFS/Dev Drive block cloning. If none is available, Kaisa copies or installs the directory and records which method was used.
- Cached directories are per attempt. They are never shared writable between attempts.

**Preservation protocol (review B6).** The protocol runs before any worktree is removed.
1. **Quiesce.** No writer may be active. This requires a keeper exit record or a frozen attempt.
2. **Claim.** Set the cleanup state from `Held` to `Preserving` with a compare-and-swap on the lease row, so only one reclaimer can win.
3. **Inventory.** Record tracked changes, untracked files, and ignored files as policy allows. The policy lists
   which ignored paths are build output and which are user data. Submodule states are included, and the
   inventory has a digest.
4. **Secret scan.** Scan everything the inventory covers. Clean changes become a WIP commit on the attempt branch. Anything flagged
   goes into an **encrypted archive** with the key in the keychain, and is never committed.
5. **Receipt.** Write the archive or commit digest, sync it to disk with `fsync`, and verify that it reads back.
6. **Reclaim.** Delete only resources that the receipt covers.

**Retention.**
- Review candidates are pinned until a decision is made.
- Attempt branches and archives have quotas and an explicit expiry. Expiry needs a Gate, or a policy that a human has approved.

### 5.6 Agent adapters: data-driven

Agents are catalog rows, following Orca's 44-agent pattern.

```toml
[agent.claude]
detect        = ["claude", "--version"]
modes         = ["observed", "brokered"]
prompt_mode   = "stdin-after-ready"
ready_signal  = "hook:SessionStart"
model_flag    = ["--model", "{model}"]
effort_flag   = ["--effort", "{effort}"]
brokered      = ["-p", "--output-format", "stream-json", "--input-format", "stream-json",
                 "--tools", "", "--mcp-config", "{kaisa_mcp_config}"]
hooks         = "claude-settings"
quota_signals = { throttle = ["http:429+retry-after"], exhausted = ["stream:error.usage_limit"] }
compaction    = "hook:PreCompact"
```

There are two trust modes, and they replace the v0.2 "interactive / headless" split (review B3).

| Mode | How | Guarantee |
|---|---|---|
| **Brokered** | The agent CLI runs as a bare transport with its built-in tools disabled. Every read, write, edit, exec and search goes through the **execution broker** in the keeper. The broker checks the policy, runs the action inside the sandbox and records it. | Kaisa sees and authorizes every effect. This is the default for automated runs. |
| **Observed** | The agent's own TUI runs in a PTY with its own tools. Kaisa hooks are installed, but they are **advisory**. Enforcement comes only from the OS sandbox and the egress broker. | Weaker. The run carries the label "observed". It is used for human-watched sessions. |

**Broker tool contracts (v1).**
- `fs.read`, `fs.write`, `fs.edit`: each operation carries the path, a content digest and a dry-run diff.
- `exec`: runs with argv, cwd and an env allow-list, under the sandbox. It has deadlines and an output cap.
- `search`: ripgrep semantics.
- `git.status` / `git.diff`.

Any other tool comes from the catalog and is policy-gated.

**Catalog kinds (v0.6).**

| Kind | Examples | Can edit files | Used for |
|---|---|---|---|
| `cli-agent` | Claude Code, Codex, Hermes, any agent CLI that passes the conformance check (`admin agent add`, §5.11) | yes, through the broker | every role |
| `openai-compatible` | a local Ollama, vLLM or LM Studio server; a company gateway | no (tool-less) | T1 work Kaisa can hand over as text: classification, summaries, first-pass review of a supplied diff (§5.10) |

A new agent is a catalog row plus a passing conformance run, never new engine code. Hermes and other
agents join the same way as Claude Code and Codex did.

**User MCP servers (v0.6.1, review B2).** A project or policy pack may list MCP servers (GitHub, a
database, a browser). They are a way to reach the outside world, so they get the same boundaries as
agents:
- **Isolation.** Each server runs under its own sandbox profile and egress allow-list, with only the
  credentials its row names, injected from the keychain by Kaisa; the agent never sees them.
- **Pinning.** A server row pins its version and the digest of its tool list and schemas. A server
  that advertises a different tool set is not started until an admin re-approves it.
- **Effect classes.** Every tool is classified in its row: `read`, `write-local` (inside the attempt's
  write set) or `external` (changes something outside Kaisa: a database row, a ticket, a deployment, a
  browser click on a live site). A tool without a class is refused.
- **External effects go through the outbox.** An `external` call is an outbox effect with an effect
  preview (§5.12) and, where policy says so, a Decision before it runs; it cannot be used to deliver
  around the delivery gate (§5.8). Effects that cannot be probed after a crash open a Gate (§5.2).
- **Untrusted output.** Tool results and resources are untrusted data, size- and time-limited.
  Sampling, elicitation and calls from one server to another are off unless the row enables them.
- A server that is not listed does not exist for the agent, and every call passes the same
  chokepoint and receipt as built-in tools.

**Compaction snapshot.** This is ruflo ADR-505. Where the agent exposes a pre-compaction hook, the keeper keeps
a bounded snapshot of the context about to be compacted. That snapshot feeds a re-injection after compaction and
the handoff packet.

### 5.7 Guard: policy, sandbox and egress

**Authority split (review B1, P10)**

| Authority | Who | Can | Cannot |
|---|---|---|---|
| **Human** (control endpoint) | human clients (CLI, TUI, desktop, presence-checked notifications) | everything: resolve Gates, approve, merge, retain, change policy, manage secrets | — |
| **Requester** (requester bridge) | an agent session the user works in, e.g. `/kaisa` inside Claude Code (user-flows F05) | `kaisa_request` (start runs within the user's budget and policy), `kaisa_status`, read results, cancel runs it started | approve, merge, resolve Gates, widen policy or egress, exceed budget, read secrets |
| **Agent** (per-attempt bridge) | workers and coordinators started by Kaisa | only the tools of the attempt's role | any requester or human operation |

- **Requester tokens** are issued per agent session when `kaisa init` installs the `/kaisa` command.
- **Scope.** A token is bound to the repository and the user, and expires after a period of inactivity.
- **Confused-deputy guard.** A request whose text came from untrusted content, such as an issue or a fetched page, is
  labelled. The policy can require a human to confirm it before the run starts.

- **Unreachable control endpoint.** Every sandbox profile denies access to the control socket or named pipe, the state directory, `kaisa.db`, the keychain items and the
  keeper sockets of other attempts.
- **Windows pipe checks.** The control pipe is created first (the flag `FILE_FLAG_FIRST_PIPE_INSTANCE` blocks squatting). Remote clients are rejected, and the client token's SID and integrity level are checked.
- **Human presence.** Resolving a Gate from the CLI can require human presence, for example Touch ID, Windows Hello or a password prompt. This is policy-configurable, and it is required when a run is `unsandboxed`.
- **Residual risk.** In an `unsandboxed` run, the agent can reach anything its user can. Such runs need a human
  Gate and are labelled for the whole run.

**Containment matrix (review B2).** Minimum versions are release requirements, and negative tests prove each
row on CI.

| Capability | Linux | macOS | Windows |
|---|---|---|---|
| Filesystem confinement | Landlock ABI ≥ 3 (kernel ≥ 6.2) | Seatbelt profile (macOS ≥ 14) | AppContainer + ACLs on the worktree (Windows 11 / Server 2022) |
| Write-set confinement | Landlock write rules on the write-set paths + scratch only | Seatbelt `file-write*` on the write-set paths + scratch only | ACLs granting write on the write-set paths + scratch only |
| Network to broker only | Landlock ABI ≥ 4 TCP connect rules (kernel ≥ 6.7), or a network namespace when user namespaces are allowed | Seatbelt network rules (localhost:broker only) | AppContainer without network capabilities + loopback exemption for the broker only |
| Descendant containment | cgroup v2 delegated leaf | process group + descendant scan | Job Object, breakaway denied |
| Syscall reduction | seccomp | n/a | n/a |

If a capability is missing, the run either refuses to start or runs in a **reduced** mode. Reduced mode is labelled on the run and needs
a human Gate. It is never silent (P13).

**Write-set confinement (v0.6.1, review B1).** The write set is enforced by the OS for every process
the attempt starts — the agent, `exec`, interpreters, build scripts and test runners alike:
- Write access is granted to the write-set paths and to the attempt's **scratch area** only. The
  scratch area holds build outputs and caches (`target/`, `node_modules/`, `.venv/`, tool caches),
  is declared per profile, is never part of a candidate and is discarded at reclamation.
- Rename, hard link and symbolic-link creation are allowed only when source and destination are
  both inside the write set or both inside scratch; a link pointing out of the write set is refused
  at creation (Landlock `REFER`, Seatbelt path rules, Windows ACL checks on the target).
- A post-attempt diff check compares the worktree with its base and refuses a candidate with any
  change outside the write set. It is a second line, not the enforcement.
- A tool that cannot run under these rules fails with a fix option ("add `generated/` to the write
  set"), never with a silently widened sandbox.

**Egress broker.** Each attempt gets an HTTP(S) proxy with a domain allow-list, for example package registries and git remotes. This is needed
because the OS layers alone cannot filter by domain. All direct network access is denied by the sandbox. Verification and
`exec` run under the same profile.
- **No way back to the host (v0.6.1, review M4).** After DNS resolution, and again on every redirect,
  the broker refuses destinations that are loopback, private, link-local or the host's own addresses,
  unless the policy names that exact address and port for this role. The control endpoint, the web
  console and other attempts' keepers are never nameable. MCP servers (§5.6) and model endpoints use
  the same check.

**Policy snapshots (review M5).**
- **Snapshots.** Policy is compiled into immutable files named by their content hash. Each one carries a MAC with a daemon key, an epoch and an expiry, and is published with an atomic rename.
- **Hooks.** A hook validates the schema, length, MAC and expiry. If any check fails, it **fails closed**.
- **Broker re-check.** The broker re-checks authority at the moment an effect executes, which closes the window where a snapshot has gone stale. File operations open paths with
  `openat2(RESOLVE_BENEATH)`, or the per-OS equivalent, to close TOCTOU gaps.
- **Spool.** Spool records carry a per-attempt MAC and have a size limit. Records with a bad MAC are dropped and counted.
- **WASM guards.** They run under `wasmtime` with limits on fuel, memory and host calls. They are off by default in v1.

**Rules (v1)**
- A shell tokenizer for dangerous commands, rewritten in Rust from ruflo's `pre-bash`. It is advisory in observed mode and enforced in the broker.
- A per-role path allow-list.
- Secret detection on writes and arguments.
- Permission-boundary actions open a Gate: push, deploy, delete, publish and egress outside the allow-list.

**Project-settings risk scanner.** Taken from ruflo ADR-509. Before an attempt starts, and during `kaisa init`, Kaisa scans the
agent's project configs. It **refuses** auto-approval keys such as `enableAllProjectMcpServers`,
blanket allow-lists, and hooks that call outside the repo, unless a human approves them.

### 5.8 Verification loop and delivery gate (review M6, v0.6)

Kaisa, not the agent, decides when work is good enough. The human receives only results that passed
this loop; nothing that was merely "exited 0" or "reported success" reaches them.

**The loop, per writing Task**

```
Candidate ──► Checks ──► Review ──► Security review ──► Delivery gate ──► Delivered
   ▲            │          │              │
   │            ▼          ▼              ▼
   └──── Revision (findings in its context pack; at most `revision_budget`, default 2)
                          │ budget spent, or the same finding repeats
                          ▼
                     Human Gate (with the findings, the diffs and the evidence)
```

| Stage | Who | Output | Blocking when |
|---|---|---|---|
| Checks | Kaisa runs the trusted commands (step 3 below), including the impact set's tests (§5.2) | `CheckReport` per command: exit, duration, log digest | any command fails, or tampering is detected |
| Review | a reviewer agent, a different model from the implementer, on a clean snapshot | `ReviewVerdict` (schema below) | any finding with `blocking = true` |
| Security review | the `security` role, `never_downgrade`, only when the diff touches policy-listed paths or patterns (auth, crypto, deserialization, process execution, dependency changes, CI config) | `ReviewVerdict` with security categories | any blocking finding, or a new dependency without a licence and advisory check |
| Revision | a new Attempt of the same Task, usually the same agent | a new candidate | — |
| Delivery gate | Kaisa | a delivery receipt | see below |

**`ReviewVerdict`** is the only output Kaisa accepts from a reviewer (a structured `kaisa_report`
call, validated against a JSON schema; free text is attached but never parsed for the decision):
`{ verdict: approve | request_changes | reject, findings: [{ id, severity: info | minor | major |
critical, category, location: path:line, blocking, rationale, suggested_fix? }] }`. A finding is
blocking when it says so or when its severity is `major` or `critical`. A reviewer that does not
produce a valid verdict counts as "no review", never as approval.

**Revision is automatic and bounded.** Blocking findings become the revision Task's acceptance
criteria and enter its context pack (§5.9). The revision budget is per Task; a finding that repeats
with the same id and location after a revision goes straight to a human, because the loop is not
converging. Revisions are charged to the Mission budget like any attempt.

**Eligibility is computed once (v0.6.1, review M7).** The daemon derives each candidate's state from
its evidence and every screen shows that state; no client recomputes it:

| State | Meaning | Can be approved |
|---|---|---|
| `verified` | all required evidence present and fresh, no open blocking finding | yes |
| `needs-revision` | a blocking finding is open (revision running or budget spent) | no |
| `unverified` | the project has no checks, or a required stage could not run | no; only a presence-checked override, recorded as such |
| `tampered` | tamper detection fired | no |

An override produces an `overridden` delivery, shown differently from `verified` everywhere,
including in PRs and reports. Live progress and raw terminal output are always labelled as
in-progress and never look like a delivered result.

**Answers are candidates too.** A non-code result (`kaisa ask`, F09) goes through a smaller loop:
every claim must cite a file and line, a log or a knowledge item; the daemon checks that each
citation exists at the cited revision; and an independent reviewer of a different model checks that
the citations support the claims. Unsupported claims are removed or marked before the answer is shown.

**What counts as a candidate.** A Task's result, every integration merge, every conflict revision
and the final Mission result are all candidates. Each needs its own evidence on its own source
digest; evidence is never inherited from the parts (§5.13).

**Delivery gate.** One check, at the single exit to the user (approve, merge, PR, `kaisa review`):
- every required evidence item exists for the **exact source digest** being delivered: checks
  passed, review approved, security review approved when required;
- the evidence is fresh: the base has not moved since, or the loop re-ran;
- the evidence was written by the daemon (§ verdict files below), not by any agent.
If any item is missing the action is refused with the missing item named — there is no "approve
anyway" for an agent; a human can override only through a presence-checked Decision (§5.12), which
is recorded as an override.

**Running verification safely (v0.6.1, review M8).**
- The source checkout is immutable; builds and tests write only to an ephemeral build area beside it,
  discarded after the pass.
- A candidate that changes tests on purpose (a real behaviour change) is not `Tampered` by default:
  test changes are shown side by side with the base, checked by a dedicated test-change review, and
  can pass only through that review or a recorded override.
- Failures are classified before anything is retried: `infra` (disk, network, toolchain), `flaky`
  (passes on rerun of the same digest), `timeout`, `malformed verdict`, `reviewer unavailable`.
  Each class has a retry cap; a flaky check is reported as flaky, never as a pass.
- If a reviewer cannot produce a valid verdict twice, the next eligible reviewer is used; if none
  is left, the Task waits for a human with the reason shown.
- A tool-less reviewer (an `openai-compatible` model, §5.10) answers with JSON in its reply; the
  daemon validates it against the `ReviewVerdict` schema and records it as the verdict. The model
  never writes the verdict itself.

**One verification pass** (used by the Checks and Review stages):

1. **Freeze.** Commit the candidate as an immutable snapshot, identified by its commit digest.
2. **Isolated checkout.** Check the snapshot out into a clean, read-only verification workspace. It is not the attempt's worktree.
3. **Trusted checks.** Take the verifier commands from the project config **at the base revision**, not from the candidate.
   Run them under the verification sandbox profile.
4. **Tamper detection.** Taken from ruflo ADR-503. Compare the candidate with its base and flag any of these:
   - deleted or weakened tests;
   - new `.skip` or `only` markers;
   - `--passWithNoTests`;
   - narrowed test commands;
   - edits to test scripts or CI config.

   A flagged candidate gets the verdict `Tampered`, which never counts as a pass.
5. **Independent review.** The reviewer is a different model from the implementer. It gets the requirements, the diff and the logs, and never the implementer's conclusions.
   Repository instructions and extracted facts count as untrusted input.
6. **Receipt.** A verification receipt binds the snapshot digest, the environment digest, the log digests and the verdicts.
7. **Approval.** A human approves through `kaisa review`. The approval is bound to the receipt.
8. **Merge.** Before merging, Kaisa checks the merge base again. If the base moved, it re-verifies or opens a Gate.

The verdict files and the receipts are written only by the daemon. The agent cannot write them, unlike ruflo's
project-local `verified-trajectories.jsonl`.

### 5.9 Knowledge and the context engine (v0.6: first-class, on by default)

Knowledge is how Kaisa keeps a project's memory across sessions; the context engine is how the right
part of that memory, and of the code, reaches each agent (P18). Both are event-sourced in the
ledger like every other state, so they survive restarts, can be audited and can be rolled back.

**Knowledge kinds**

| Kind | What it holds | Where it comes from | Becomes trusted when |
|---|---|---|---|
| Project profile | languages, build / test / lint commands, layout, entry points, conventions | derived automatically on the first run and whenever the base revision changes; repository instruction files are read as quoted data | derived by Kaisa (commands are confirmed by a successful check run) |
| Repository index | symbols, imports and tests per file, at a base revision | built by Kaisa (tree-sitter grammars where available; an import-line scanner otherwise, labelled) | always (it is computed, not claimed) |
| Lesson | "this failure signature was fixed by this kind of change", "reviewers reject X here" | extracted from attempts whose result passed the delivery gate, and from review findings a revision resolved | its source attempt passed the delivery gate |
| Decision | a scoped, project-level choice and its rationale | coordinator `kaisa_decide` entries and human Decisions with project scope (§5.12) | recorded by the daemon |
| Trajectory | task kind, role, grade, agent, model, effort, context items, revisions, cost, outcome | every settled attempt | always (it is a record of what happened) |
| Ingested note | agent-native memory, for example Claude auto-memory files | read-only ingestion with provenance (ruflo's memory-bridge idea) | it is confirmed by a verified use or by a human |

**Lifecycle (events).** `knowledge_proposed → promoted → injected → scored → decayed | retired`.
- Promotion of lessons and trajectories is **automatic** once the delivery gate passes (P15: it adapts
  inside bounds). Nothing in knowledge can change policy, tools, egress, secrets, spend or
  concurrency; at most it changes what an agent reads.
- Before storing, a secret scanner redacts credentials; an item that still matches is dropped.
- Knowledge is always given to agents as quoted data, never as instructions. Quoting is a format, not
  a defence, so the rules below carry the safety (v0.6.1, review M1).

**Provenance is not truth (review M1).**
- Every item records where each statement came from. A *lesson* must cite machine evidence: the
  failing check, the diff that fixed it and the passing check on the delivered digest. Kaisa writes
  the lesson from that evidence; free text written by an agent is attached as a note, never promoted.
- Agent-authored and ingested text stays **quarantined**: it can be shown to a human, and it can be
  injected only with a `tainted` mark that the context pack carries through compaction and handoff.
- A knowledge item can never contain a command to run, a path to widen, a tool to enable or a
  permission request. Items that look like one are dropped at proposal.
- Coordinator journal entries are *recorded* decisions, not verified facts, and are labelled so.
- An adversarial retrieval corpus (planted instructions, poisoned lessons, look-alike paths) runs in
  CI; none of its items may reach an agent unmarked or change a route, a write set or a check.

**Retrieval.** Hybrid by default: SQLite FTS5 (BM25) plus a local vector index, merged with maximal
marginal relevance, then weighted by recency and by measured usefulness (below).
- Embeddings come from a small local model fetched once at `kaisa init` (checksum-verified, from a
  company mirror when the policy pack names one). Inference runs in-process in Rust.
- If the model is not available, retrieval runs BM25-only and every result and the status line say
  "vector search off" (P13). It never silently degrades.
- A Mojo backend for embeddings and vector search may be added later as an accelerator on macOS and
  Linux; the Rust path stays the default and the only one on Windows (ADR-0033).

**Context packs.** At the start of every attempt Kaisa assembles a pack, within the role's token
budget, in this order:
1. the Task spec and acceptance criteria (for a revision: the blocking findings);
2. the write set and the impact set (§5.2);
3. related code from the repository index — definitions and call sites of what the write set touches;
4. the project profile summary;
5. the top relevant lessons and decisions, each with its provenance;
6. for a failover or handoff: the compaction snapshot and the handoff packet (§5.3).

**Overflow and freshness (v0.6.1, review M6).**
- Sections 1 and 2 are never cut. If they alone exceed the role's budget, the Task is too big: the
  pack is refused with the fix option "split this Task", and the coordinator is asked to split it.
  Lower sections are cut from the bottom up, and the cut is recorded in the pack.
- The context budget does not end at the first message. Per role, the broker enforces a read budget
  (bytes per read, total bytes per attempt) and caps tool output; an attempt that reaches its
  cumulative budget is compacted with a snapshot or ended with a handoff, not left to drift.
- When the repository index cannot tell what a change affects (dynamic imports, code generation,
  an unsupported language), the impact set is "unknown" and verification widens to the full check
  suite instead of guessing small.
- The profile, the index and every knowledge item carry the base revision, toolchain version and,
  for vectors, the embedding model version they were built from. A change to any of them invalidates
  what depended on it.

The pack is content-addressed and its digest is recorded on the attempt, so any run can be replayed
and audited. Agents receive it as their first message and as the MCP resource
`kaisa://context/<attempt>`; `kaisa_knowledge` is there for anything else they want to look up.

**Accountability (P19, v0.6.1 review M2).** Every injected item is recorded with the attempt.
Usefulness is not credited by giving every injected item the attempt's outcome — that rewards
bystanders and blames the wrong item. Instead:
- a small, policy-capped share of attempts runs with one eligible item held out (an ablation), and an
  item's usefulness is the outcome difference between attempts that had it and comparable attempts
  that did not;
- updates need a minimum sample, are capped per period, and near-duplicate tasks count once, so a
  burst of trivial tasks cannot dominate;
- delivered results that needed an override, and anything `Tampered`, never count as positive.
Items that keep failing to help decay; items unused for a policy-set period retire.

**Forgetting (v0.6.1, review M12).** `kaisa memory` forget writes a tombstone event. Replays never
resurrect a tombstoned item; its derived data — FTS rows, vectors, cached context packs not yet used —
is purged, and packs already recorded keep only the item's id and a "forgotten" marker. The vector index
and FTS tables are projections with a sequence watermark: after a crash, disk full or index corruption
they are rebuilt from the event log up to the watermark, so they can never disagree with it.

**Scope.** Per repository by default. Sharing knowledge across a team is a policy-pack setting
(M3), because shared knowledge is shared influence over agents.

### 5.10 Router, budget, quota and admission (review M7)

The router decides which agent, model and effort run each attempt. It sits on the spawn path (P17):
the keeper starts exactly what the router chose, so a routing decision is never a recommendation.

**Grades**

| Grade | Typical work | Default target (catalog data, per company) |
|---|---|---|
| T0 | formatting, renames, known codemods, lockfile updates | no model: a script or codemod (ruflo ADR-143 idea) |
| T1 | small, checkable edits; summaries; classification; first-pass review | a light model: Haiku-class, or a local model server such as Ollama |
| T2 | ordinary feature and bug work; reviews; documentation | a standard model: Sonnet-class or the Codex default |
| T3 | architecture, hard debugging, security review, final user-facing output | a heavy model: Opus-class |

**The grader is deterministic.** Kaisa sets a Task's grade from features it can measure — role,
task kind from the template, write-set size and spread, files in the impact set, whether security
paths are touched, earlier failures of the same Task — with the coordinator's suggestion as one
input. The grade and its reasons are recorded, so routing can be explained and replayed.

**Learned choice inside the grade.** For each `(role, task kind, grade)` Kaisa keeps a Beta prior per
eligible `(agent, model, effort)` option, updated from **verified** outcomes only (delivered without
revision, delivered after revision, rejected) and weighted by actual cost from usage events. The
router picks by expected cost per verified success, with a small, policy-capped exploration share.
Priors are per category, so one task type's failures cannot poison another's (lesson from ruflo
ADR-142). Priors change automatically (P15); the set of eligible options does not.

**Guarding the priors (v0.6.1, review M2).**
- The reward of an option is computed from the whole Task: the original attempt plus its revisions,
  with their total cost; a defect found later (a reverted delivery) is charged back to it.
- Overrides and `Tampered` verdicts are never positive rewards.
- Priors are keyed by model version and by prompt and verifier version, so an upgrade starts from a
  fresh, conservative prior instead of inheriting the old one.
- Updates need a minimum sample and are capped per period; near-duplicate Tasks count once.
- A recorded benchmark set is replayed in shadow on a schedule; if cost per verified success gets
  worse, priors roll back to the last good snapshot and an admin is notified.

**Escalation rules.**
- A failed attempt is retried on the next option in the grade before the grade goes up; the effort
  of the same model is raised at most one step, because a stronger model at medium effort usually
  beats a weaker one at maximum effort.
- Steps marked `never_downgrade` (security review, release gate, permission boundary, final
  user-facing output) are never routed below their declared grade, whatever the budget says.
- A reviewer is always a different model from the implementer (§5.8).

**Local and OpenAI-compatible models.** An agent catalog row may point at an OpenAI-compatible
endpoint (for example a local Ollama server). Such rows are tool-less: they serve T1 work that needs
no file access — classification, summaries, first-pass review of a diff Kaisa supplies. Their usage
is recorded in tokens even when it costs nothing.

**Trusting an endpoint (v0.6.1, review M5).** A model endpoint receives code, so it is configured,
not discovered:
- the row pins the endpoint — a loopback address checked by peer identity, or an HTTPS URL with a
  pinned certificate — and the model name and digest the server must report; redirects are refused;
- the row states the **data classification** it may receive; policy decides which repositories and
  which kinds of content (diffs, secrets-scanned excerpts, summaries only) may go to it;
- requests have time, size, token and concurrency limits, and a failing endpoint is isolated: its
  Tasks fall back to the next option in the grade, and the endpoint is marked unhealthy;
- usage reported by a local server is labelled "reported by endpoint", never treated as billing truth.
The local embedding model follows the same rule: its checksum comes from Kaisa's signed release
manifest, and its inference runs in a separate worker so a native crash cannot take the daemon down.

- **Routing policy.** Policy is data. A task's grade and its required capabilities map to the
  eligible agents, models, efforts and a candidate count N; the learned choice picks among them.
- **Fallback chains** exist for the coordinator and for workers, and each is filtered by capability.

```toml
[coordinator]
chain = ["claude:claude-opus-5-5:high", "codex:gpt-6.1-sol:medium", "claude:claude-sonnet-5-5:medium"]
requires = ["tool-use", "long-context"]
switch_at_usage = 0.85
handback = { on_reset = true, min_dwell = "30m" }      # hysteresis
never_downgrade = ["security-review", "release-gate", "permission-boundary"]

[worker]
chain = ["codex:gpt-6.1-sol:medium", "claude:claude-sonnet-5-5:medium"]
failover = "preserve"     # §5.5 protocol
```

**Signal classification**

| Class | Examples | Action |
|---|---|---|
| Throttle | 429 with retry-after, transient 5xx | Exponential backoff with jitter. No failover until `throttle_budget` is spent. |
| Exhausted | Usage-limit errors that match the catalog `quota_signals` | Failover with a cooldown. Hand back only after `min_dwell`. |
| Unknown | Unclassified errors | Retry once, then open a Gate. |

- Signals come from the adapter's structured channels: stream events and exit codes. Stderr matching is a labelled fallback, because an agent could spoof it.

**Admission control and enforcement**

- **Cost reservation (v0.6.1, review M3).** Before admitting an attempt, the Router reserves an
  estimated cost **atomically** along the whole chain — Mission, sub-run, user and team — in one ledger
  transaction, so concurrent admissions cannot each see room that only one of them has. Actual usage is
  reconciled afterwards.
- **Per-request ceilings.** Every model request carries a maximum output size (and, through a gateway,
  a maximum cost) so one request cannot overrun the remaining reservation by much.
- **Hard caps are a capability, not a promise.** An adapter or gateway that can refuse a request before
  it is billed declares `hard_cap`. Without it, the budget is enforced at the next admission and the UI
  shows the largest possible overrun ("may exceed by up to $0.40: Claude subscription reports usage after
  the turn"). Kaisa never calls such a budget hard.
- **Per-request usage.** Adapters report usage per model request from the agent's structured stream (or from the gateway), not per turn, so cost is attributed to the attempt, role and tool that caused it. Where an agent exposes only turn totals, the figure is labelled as estimated.
- **Separate caps.** Unpriced usage and subscription usage are capped separately.
- **Optional gateway enforcement.** This is ruflo ADR-508. Kaisa writes a budget file and can configure an LLM gateway, such as LiteLLM, with **per-attempt virtual keys**. The gateway enforces hard limits. Kaisa keeps attribution per attempt, role and tool.
- **Budget ladder.** Notifications fire at 50 / 75 / 90 / 100%. At 100% the run pauses, and steps marked `never_downgrade` stop and ask.
- **Proactive switching** happens only when a provider's usage can actually be read. Otherwise the provider is labelled `reactive-only`.

**Context-footprint budget.** This is ruflo ADR-507. CI measures the tokens taken by each role's tool catalog and its preamble, and holds them under a budget.

### 5.11 Experience services

These services exist because of [`user-flows.md`](user-flows.md). Each one is a daemon module behind the
same ledger, outbox and authority rules as the engine cores.

**Notifier.** Used by U4, F13, F14 and F19.
- **Channels.**
  - OS notifications: `UNUserNotificationCenter` on macOS, the freedesktop D-Bus spec on Linux, toast notifications on Windows.
  - The terminal bell.
  - A Claude Code / Codex status line, written as a status file that the agent's status-line hook reads.
  - Later: Slack and mobile.
- **Events.** Only events that need attention or report completion produce a notification. Notifications are de-duplicated and rate-limited, with quiet hours per user.
- **Actions from a notification.**
  - Answering a question and allow-once map to Human operations. They require the OS **presence check** (Touch ID, Windows Hello, polkit/password) whenever the policy marks the action as presence-required.
  - Otherwise the notification only opens the TUI.

**Issue-tracker connector.** Used by F08 (GitHub in M1, Linear in M3).
- **Trigger.** A label or a mention creates a run request.
- **Authorization.** The labeler must be on the repository allow-list. Otherwise the request waits for a Human Gate.
- **Untrusted input.** Issue text, comments and attachments are untrusted. They are screened by policy and passed to agents as quoted data, never as instructions.
- **Writes.** Kaisa posts progress comments and opens PRs through the outbox. Its credentials are a GitHub App installation token, kept in the keychain and scoped to the repository.

**Takeover and hand-back.** Used by F11.
- `attach --take` puts a **human-presence lease** on the attempt. The agent is frozen (§5.1), its trust mode becomes *observed*, and the PTY input switches to the human.
- `handback` records a diff of what the human changed, injects it as a steer, and resumes the agent.
- Both transitions are ledger events. Verification later treats the attempt as mixed authorship.

**Undo and restore.** Used by F17.
- Every outbox effect type declares its **inverse**, or declares that it has none:

  | Effect | Inverse |
  |---|---|
  | merge | revert commit |
  | PR open | close PR |
  | branch delete | recreate from preservation receipt |
  | archive | restore |
  | push | none: a Gate offers a revert push |

- `kaisa undo` applies the most recent reversible effect of the user's last action, as a new outbox effect.
- `kaisa restore <run|attempt>` rebuilds a worktree from its preservation receipt.

**Plan templates and policy packs.** Used by F07, F20, F22 and F25.
- **Templates** are data: roles, a DAG, gates, default agents and budgets. They live in a **policy pack**.
- **A policy pack** is a git repository signed by an admin key. It holds the allowed agents and models, budgets, egress allow-lists, approval rules and delegation (path or repository → reviewers), templates, and verifier defaults.
- **Distribution.**
  - Clients fetch and verify packs on start and on a schedule.
  - A changed pack produces a "policy updated" notification.
  - If the pack is unreachable, clients use the cached pack and label it as cached. With no cache, they fall back to built-in strict defaults.

**Admin lifecycle.** Used by user-flows F22–F25.

| Operation | Mechanism |
|---|---|
| `admin enroll` | The first admin key is bound to company SSO. Kaisa prefers a hardware-backed key (Secure Enclave, TPM, FIDO2). Enrollment is recorded as a signed pack event. |
| `admin pack validate` | Validates the schema, then **simulates** the candidate policy against the last N runs' decision log and reports which decisions would change. |
| `admin pack publish --stage P%` | Staged rollout. Clients choose a cohort deterministically by hashing user and machine. Rollout widens only when an admin approves the next stage. |
| `admin pack rollback` | Publishes the previous version with a higher sequence number. Clients never accept a pack with a lower sequence number, so a rollback is always a new publish (anti-replay). |
| Bad signature on a pack | The client keeps the last good pack and raises an admin-visible alert event. |
| `admin offboard` | Revokes requester tokens and virtual keys. Transfers runs and preserved work to the lead. Issues an **offboarding receipt** listing every resource moved and revoked. |
| `admin template test` | Dry-runs a template on a fixture repository with scripted agents (the same harness as the engine tests). |
| `admin agent add` | Runs the agent **conformance check**: detect, login probe, brokered-mode support, quota-signal parsing and hook installation. An agent row can be enabled only after it passes. |

**Localization and the plain-language renderer.** Used by U5 and U11.
- **Message catalog.** Every user-facing string comes from a catalog in Fluent format, with `ko` and `en` from M0.
- **Rendering.** A renderer maps ledger events to plain sentences, for example `coordinator_handoff` becomes "Claude ran out of quota, so GPT took over planning; nothing was lost." Internal terms (epoch, generation, lease) appear only with `--verbose` or `--json`.
- **Fix options.** Every blocking event type must declare at least one fix option. CI fails if one does not, which enforces "no unexplained blocks" (user-flows §10).

**Home TUI.** Used by U1, F02 and F10. This is the screen behind `kaisa` with no arguments.
- On first run it shows onboarding.
- Afterwards it shows the **Needs you** lane, then run cards with the plan tree, live cost, the current action of each agent, the review queue and pending questions.

**Web console (v0.6).** `kaisa console` opens the same information in a browser; nothing listens
until that command runs (P14).
- **Transport.** The daemon binds `127.0.0.1` on a random port for the console's lifetime. A
  per-launch 256-bit token is passed in the URL fragment and exchanged once for an `HttpOnly`,
  `SameSite=Strict` session cookie; the token is single-use and expires after 60 seconds unused.
  Every request must carry the exact `Host` (address and port) and, for state-changing requests, the
  exact `Origin` and a CSRF token. The console sends a strict Content Security Policy. Remote binding
  is not offered in v1.
- **Lifetime.** The server stops when `kaisa console` exits, when the last tab's heartbeat stops for
  a policy-set period, or at a hard TTL. Sessions die with it.
- **Rendering.** Terminal transcripts are rendered from the terminal model, never as raw bytes:
  ANSI sequences are interpreted, HTML is escaped, and links are inert. Server-sent events have
  resumable ids, a per-client buffer limit and a slow-consumer cut-off.
- **Residual risk.** A malicious process running as the same OS user can read the cookie store or
  the terminal the token was printed to. The console does not protect against that user's own
  malware; the agent sandbox (§5.7) is what keeps agents from being that process.
- **Authority.** The console is read-only by default: a projection of the event stream over
  server-sent events. Human actions (answer, approve, cancel) are sent as control-endpoint operations
  with the same authority rules and presence checks as the CLI (§5.12); an agent can never reach it,
  because agents have no route to loopback outside their broker (§5.7).
- **Views.** Missions and their sub-runs; the team as a role × status grid that stays readable at
  100+ agents, with drill-down to one agent's live terminal (read-only replay from the keeper's
  transcript); tokens and cost per request, role, model and Mission, with `n/a` wherever a number
  was not measured; the Needs-you queue; the verification loop of each Task; and the knowledge items
  each attempt received.
- **Build.** The page is static HTML, CSS and TypeScript compiled at build time and embedded in the
  binary, so there is no Node runtime (TypeScript 7 is used for type checking only, ADR-0001).

### 5.12 Decisions, answers and effect previews

**Two kinds of human input (user-flows U9).** v0.4 had a single Gate type. It is now split into two.

| Kind | Meaning | Who may provide it | Can it time out into "yes"? |
|---|---|---|---|
| **Answer** | Information an agent asked for, e.g. "use Redis". | Human or Requester (the question's owner chain) | No. A timeout runs the question's declared fallback, e.g. "agent chooses and notes it" or "pause the step". |
| **Decision** | Authorization: approve or publish, allow egress or files, budget increases, reduced protection, policy exceptions. | Human only, with a presence check when policy requires it. | **Never.** |

**Questions.** Every question records:
- an **owner chain**: the requester, then the run owner, then a delegate;
- a `timeout` and a `fallback`;
- whether the fallback is allowed at all. Some questions must pause.

Unanswered questions go to the Needs-you lane (§5.11) and notifications. Other steps keep running.

**Scoped, expiring decisions (F02, F14, F19n).** Every Decision is stored with:
- `scope`: `once | run | project | machine`;
- `expires_at`;
- `granted_by`;
- `presence_verified`;
- `policy_allowed`, checked against the policy pack when the Decision is made.

The policy compiler includes active Decisions in each snapshot, so an expired Decision drops out
at the next snapshot. Because a running attempt may outlive a snapshot, expiry is also re-checked
at every dispatch and every effect (v0.6.1, review M12): when a reduced-protection Decision expires,
the attempts that relied on it are frozen and their generation revoked until a human renews the
Decision or the run proceeds without it. Expiry uses the daemon's monotonic clock plus wall time; a
clock jump backwards never extends a Decision. Reduced-protection approval is a `machine`-scoped Decision whose card lists:
- the missing capabilities;
- what the agents can still reach;
- the expiry.

The approval persists as a banner until it expires or the missing capability appears.

**Effect preview API (F16, F17, F27).** Before approve, undo, publish, cancel or GC, the daemon computes a
**preview**. It does this by dry-running the outbox effects the action would enqueue. A preview contains:
- **For each effect:** its kind and target, for example "open PR from `kaisa/41-a` to `main`".
- **Reversibility:** whether and how the effect can be undone, and for how long.
- **Preconditions:** for example "base unchanged since verification".
- **Authority and presence:** the authority required, and whether presence is required.

When the user confirms, the client sends back the preview's digest. If the state changed in the meantime, the
daemon rejects the action and re-previews. This rule is "you approve exactly what you saw".

**Approval rules (F20, F33).** The policy maps paths and repositories to an approval rule:
- `self`: the authenticated human who started the Mission may approve (v0.6.1, review M11). A
  Requester token (§5.7) can never approve, whatever the rule says;
- `second-person`: someone other than the requester must approve;
- `codeowners`: an approver from CODEOWNERS;
- `n-of-m`: N approvals from a named group.

When an eligible approver is revoked, offboarded or marked away, pending approvals are **reassigned** along the
delegation chain. Every reassignment is recorded, and an approval never falls back to `self`.

### 5.13 Scale: large teams (v0.6)

"Team size" means agent processes actually running at the same time — never a count of records
(P17). The target is 100+ concurrent agents on one well-equipped host, proven by the load benchmark
in §15 before it is advertised.

**Hierarchy keeps every coordinator's context small.** One coordinator cannot follow a hundred
agents any better than one developer can; that is context collapse moved up a level.
- The root coordinator splits a large Mission into **sub-runs** along disjoint write-set areas (for
  example `api/`, `web/`, `docs/`). Each sub-run has its own coordinator epoch, budget slice and
  integration branch.
- A coordinator manages at most `max_active_tasks` Tasks (policy, default 12) and the tree is at most
  three levels deep. Beyond that the plan is refused with a fix option, not silently flattened.
- A parent coordinator sees **roll-ups** of its sub-runs — status, evidence, blocking questions,
  budget — not their raw event streams. The daemon builds the roll-ups; no coordinator summarises
  another.
- Sub-coordinators fail over independently (§5.3); a sub-run's failure is an event in its parent's
  roll-up.

**Admission is measured, not configured by hope.**
- Each host has slots for heavy attempts (worktree, keeper, sandbox) and light attempts (shared
  read-only snapshot, §5.2). Slot counts come from measured per-attempt costs (RSS, file descriptors,
  worktree disk, CPU) against free capacity, and from each provider's rate limits.
- Ready Tasks beyond the available slots wait in the queue with their position shown; the user is
  told "37 running, 12 queued (rate limit: Claude)", not left guessing.
- The ledger stays the single writer (P12); events from many keepers are batched per transaction,
  and the append budget at 100+ agents is measured (§15).

**Integration without dumping conflicts on people.**
- Results merge into their sub-run's integration branch in DAG order, each merge behind the delivery
  gate (§5.8).
- A merge conflict becomes a revision Task for the role that owns the conflicting paths, with both
  sides in its context pack. Only a conflict that the revision cannot resolve reaches a human.
- Every integration result is a candidate of its own and goes through the **whole** loop (§5.8):
  checks with the combined impact set, review of the combined diff, and security review whenever the
  combined diff touches listed paths — applicability is recomputed on the combined diff, because two
  safe changes can make an unsafe pair. This applies to each merge, to each conflict revision and to
  the final Mission result. Child receipts are inputs to the reviewer, never a substitute: evidence is
  valid only for the exact source digest it was produced on.

**Proving it (v0.6.1, review M9).** Scripted agents prove the engine, not the service, so there are
two benchmarks:
- *Engine scale*: scripted agents on a fixed reference host (CPU, memory and disk recorded with the
  result), a fixed role mix (60% light, 40% heavy), 30 minutes sustained; it reports total RSS, file
  descriptors, worktree disk, ledger append latency, admission latency, p95 queue wait and completed
  Tasks per hour.
- *Service scale*: real agent CLIs that passed conformance, with real checks and reviews, at the
  largest size the providers' rate limits allow, reported as measured — including the amplification
  that review and security add.
Write-set reservations are taken **Mission-wide** in one ledger transaction, so two sub-runs can never
own overlapping paths. Tests cover hierarchy deadlock (a parent waiting on a child that waits on a
parent's path), starvation of light Tasks behind heavy ones, ledger writer saturation (backpressure to
admission, never dropped events) and the loss of one keeper among many.

**Beyond one host (M3).** Remote workers add slots on other machines through the team-server
mode; the hierarchy and the admission rules do not change.

---

## 6. Protocol (KRPC)

- **Framing.** NDJSON JSON-RPC 2.0.
  - The control endpoint is a Unix socket in a 0700 state directory, or the named pipe `\\.\pipe\kaisa-<sid-hash>`, created as the first instance.
  - Keeper sockets are private to each attempt and are denied to every other sandbox.
- **Authentication.**
  - Control clients are authenticated by OS peer credentials: `SO_PEERCRED`, `LOCAL_PEERCRED`, or the pipe client token's SID, integrity level and local-only check.
  - Agent bridges present a capability token. The token is delivered on a private fd, or as an env var visible only inside the attempt sandbox, and never in argv.
- **Versioning.** There is one protocol version. Capabilities are exchanged in `hello`, and only advertised operations may be used.
- **Long operations.** They stream `progress` and `_keepalive` frames.
- **MCP.** Kaisa uses `rmcp` 3.8 over stdio for agent bridges.
  - The role-specific tool catalogs are compact.
  - CI checks a recorded catalog digest.
  - CI also records interoperability tests against the Claude Code and Codex MCP clients (review M8).
- **CI contract (user-flows F26).** `kaisa run --headless --json` reports its result with these exit codes:

  | Code | Meaning |
  |---|---|
  | `0` | Completed. A result was produced, or no change was needed. |
  | `3` | **Waiting for a human.** The JSON has the Decision link and the run id. |
  | `4` | Failed. The JSON has the reason and the receipts. |
  | `5` | Blocked by policy. |
  | `6` | Infrastructure unavailable: provider, network or sandbox. |

  The JSON output has a stable, versioned schema with a recorded digest. `kaisa run --resume <id>` continues a
  run after its Decisions are resolved. CI never resolves a Decision.
- **Remote access** is deferred until after v1. Its design, pinned-key pairing with an E2EE relay, is kept in §19.

---

## 7. Data model (projection tables, v1)

```
missions(id, goal, state, budget_id, policy_version, root_run_id, delivered_receipt)
runs(id, mission_id, parent_run_id, area, state, budget_id, coordinator_attempt_id,
     coordinator_epoch, policy_version, integration_branch)
tasks(id, run_id, role, kind, title, spec, status, deps_json, grade, grade_reasons_json,
      write_set_json, impact_set_digest, revision_of, revisions_used, base_revision)
attempts(id, task_id, role, generation, agent, model, effort, trust_mode,
         exec_state, outcome, verify_state, cleanup_state, supersedes, weight, context_pack_digest,
         route_decision_id, started_at, settled_at)
keepers(attempt_id, endpoint, incarnation, os_handle_kind, last_seen)
leases(id, attempt_id, resource_kind, resource_ref, expires_at, max_until, retained, state)
outbox(id, run_id, epoch_or_generation, effect_kind, intent_json, state, receipt_json)
gates(id, run_id, kind, options_json, requires_presence, state, resolved_by, resolved_at)
journal(seq, run_id, attempt_id, epoch, based_on_cursor, kind, body_json)
handoffs(id, run_id, from_attempt, to_attempt, from_epoch, to_epoch, reason, packet_digest, at)
cursors(consumer, run_id, epoch, delivered_seq, confirmed_seq)
steers(id, attempt_id, state, injected_at, acked_at)
receipts(caller, request_id, payload_hash, result_json)
preservations(id, attempt_id, inventory_digest, kind, location, verified_at)
verifications(id, attempt_id, snapshot_digest, env_digest, step, verdict, log_digest)
review_findings(id, verification_id, finding_key, severity, category, location, blocking, resolved_by)
deliveries(id, mission_id, task_id, source_digest, evidence_json, override_decision_id, at)
knowledge(id, kind, scope, body_json, provenance_json, state, usefulness_alpha, usefulness_beta,
          created_seq, last_used_seq)
knowledge_fts(knowledge_id, text)                                   -- FTS5
knowledge_vectors(knowledge_id, model_id, dims, vector_ref)         -- local vector index
injections(attempt_id, knowledge_id, rank, score, outcome)
context_packs(digest, attempt_id, role, token_count, items_json)
repo_index(revision, path, language, symbols_digest, imports_json, tests_json)
project_profiles(repo, revision, profile_json, confirmed_commands_json)
route_decisions(id, task_id, grade, options_json, chosen, reason, explore)
route_priors(role, task_kind, grade, option, alpha, beta, cost_per_success, updated_seq)
slots(host, kind, capacity, measured_at)
usage(id, attempt_id, request_id, role, tool, provider, model, input_tokens, output_tokens,
      cached_tokens, cost_usd, estimated)
reservations(id, attempt_id, budget_id, estimated_usd, reconciled_usd)
quota_state(provider, observed_at, usage_ratio, mode, cooldown_until)
events(seq, at, version, kind, subject, payload_json, prev_hash, hash)
checkpoints(seq, hash, signature, anchored_at, anchor_ref)
requester_tokens(id, session_ref, repo, user, expires_at, revoked)
notifications(id, run_id, kind, channel, state, action_taken, presence_verified, at)
integrations(id, kind, repo, installation_ref, state)                 -- github, linear
inbound_requests(id, integration_id, external_ref, author, trusted, state, run_id)
takeovers(id, attempt_id, human, started_at, ended_at, diff_digest)
effect_inverses(outbox_id, inverse_kind, inverse_intent_json, state)
policy_packs(id, repo, version, signature, fetched_at, cached)
templates(id, pack_id, name, dag_json, gates_json, defaults_json)
questions(id, attempt_id, owner_chain_json, timeout_at, fallback, state, answered_by)
decisions(id, run_id, kind, scope, expires_at, granted_by, presence_verified, policy_allowed, state)
previews(id, action, effects_json, digest, state_digest, created_at, confirmed)
approval_rules(id, pack_id, path_glob, rule, group_json)
reassignments(id, approval_id, from_user, to_user, reason, at)
admin_events(id, kind, actor, subject, receipt_json, signature, at)   -- enroll, publish, rollback, offboard
pack_rollouts(pack_id, stage_percent, approved_by, at)
```

---

## 8. Cross-platform matrix

| Concern | macOS | Linux | Windows |
|---|---|---|---|
| PTY | openpty | openpty | ConPTY |
| Durable ownership | keeper + process group + kqueue | keeper + pidfd + cgroup v2 | keeper + Job Object |
| Sandbox | Seatbelt (≥ 14) | Landlock ABI ≥ 3/4 + seccomp | AppContainer (Windows 11 / Server 2022) |
| Egress | Seatbelt → broker | Landlock TCP / netns → broker | AppContainer loopback exemption → broker |
| Control IPC | Unix socket + `LOCAL_PEERCRED` | Unix socket + `SO_PEERCRED` | First-instance named pipe + token SID / IL / local check |
| Secrets | Keychain | Secret Service. Headless with no Secret Service: refuse, or use an explicit passphrase-unlocked file | Credential Manager / DPAPI |
| CoW clone | APFS clonefile | FICLONE (btrfs, xfs) | ReFS / Dev Drive block clone |
| Service | launchd agent (user session) | systemd user unit | per-user logon task, run as the interactive user, never as a service account (review M10) |
| Signing | codesign + notarization | ed25519 + SBOM | Authenticode |
| Build | native (aarch64, x86_64) | `cargo-zigbuild` → static musl | native MSVC |

---

## 9. Security model

1. **Two authorities.** Humans act through the control endpoint, and agents act through capability-scoped bridges. Agents cannot
   reach the control endpoint, the daemon storage or the keychain items (§5.7).
2. **Agents are untrusted.**
   - They run inside a sandbox, and their network goes through the egress broker.
   - In brokered mode, every effect is authorized at the moment it executes.
   - Observed mode is labelled as weaker.
3. **Credentials (review M10).**
   - Provider API keys are never handed to agents directly. When a gateway is configured, agents get **per-attempt virtual keys** that Kaisa can revoke.
   - Subscription-login CLIs read their own auth store. The sandbox exposes only that agent's auth directory, read-only where the CLI allows it.
   - It is documented that Kaisa cannot shorten the lifetime of such credentials.
   - Child environments are built from an allow-list. Output, spool and log streams have secret redaction.
4. **Audit.**
   - Events are hash-chained, with signed checkpoints that can be anchored externally.
   - `kaisa audit verify` checks the chain and the signatures.
   - The limit against a same-user attacker is documented.
5. **Supply chain.**
   - Kaisa ships as one signed core binary with an SBOM and a reproducible lockfile with checksums.
   - Native dependencies are listed with their licences.
   - No dependency is resolved at install time.
6. **No escape hatches.** Nothing like `RunAsNode`, and no variable that loads code. Debug features are
   compiled out.

---

## 10. Workflows

**A. Single task (brokered)**

```
kaisa run "fix #123"
 → coordinator (epoch 1) proposes plan → Task(role=developer)
 → Attempt: keeper + worktree + sandbox + broker; agent edits/execs via broker
 → exit record → Verify Gate (freeze → clean checkout → trusted checks → tamper scan → review)
 → receipt → kaisa review (human approval bound to receipt) → merge (base revalidated)
 → dependent Tasks start automatically → knowledge proposal (if enabled) → preserve → reclaim
```

**B. Team pipeline.** The roles researcher → architect → developer + tester → reviewer form a DAG. Each settled and verified
attempt **advances the pipeline automatically**. Nothing waits for the coordinator to remember to start the next one.

**C. Quota exhaustion.** This is the §5.3 sequence. The coordinator moves Claude → GPT under a new epoch while workers keep running.
A worker is quiesced, preserved and then superseded on the next provider.

**D. Observed session.** A human runs an agent TUI under Kaisa supervision. Kaisa adds the sandbox, the egress broker,
advisory hooks, verification and reclamation. The run is labelled "observed".

---

## 11. Prior art: what Kaisa keeps and what it changes

### From Orca

| Kept (internal) | Changed |
|---|---|
| Fencing, idempotent receipts, never auto-retry ambiguity, staged startup with residual resources, data-driven agent catalog | Ownership moves from terminals to keepers. Completion is detected by Kaisa instead of reported by `worker_done`. Heartbeats are derived instead of sent by the LLM. Cleanup is automatic, with preservation. Delivery acks become epoch-scoped cursors. The coordinator becomes an epoch-scoped role, and handoff is automatic. There is one daemon instead of three server roles. |

### From ruflo

| Kept | Changed |
|---|---|
| Role → permissions/isolation table (team bus) | Extended with sandbox profile, tool set and model grade; enforced instead of advisory |
| `SubagentStop` → pipeline advance | The scheduler starts the next step itself instead of returning a hint |
| Verified-outcome learning (ADR-502) and gaming defences (ADR-503) | Verification is run by Kaisa against an immutable snapshot; verdicts are daemon-owned |
| Compaction snapshot (ADR-505) | Also feeds the handoff packet |
| Cost ladder + gateway enforcement (ADR-508) | Per-attempt virtual keys; reservation-based admission |
| Plugin trust and risky-settings refusal (ADR-509) | Applied at `kaisa init` and before every attempt |
| Context-footprint budget (ADR-507) | Measured in CI per role catalog |
| Tier-1 deterministic codemods (ADR-143) | Tier 0 of the router |
| No self-promotion of learned changes | Proposals are applied only after a human Gate |
| `pre-bash` tokenizer, tool-list filtering, signed artifacts, honest "record only" labels, multi-vendor execution, auto-memory bridge, witness digests | Rust rewrites, or design rules |
| **Not kept:** execution through CLAUDE.md instructions, plan-only spawn tools, a keyword routing banner on every prompt, a Node process per hook, side effects on read-only commands, the hash-embedding fallback, about 300 tools by default, a 1.7 GB install | — |

### From LearnHarness (ideas only, AGPL)

The agent CLI as a bare transport became the **brokered** mode. Kaisa also takes:
- deterministic steer ids;
- write-ahead turns;
- a global LLM semaphore;
- storing tokens and deriving dollars from them;
- verify before deliver.

### From rea

Kaisa takes:
- owned-process identity;
- an evidence ledger with Unknowns;
- bounded output;
- the per-session capability token;
- recorded catalog digests;
- a targeted mutation-testing gate.

---

## 12. User interface strategy

| Phase | Client | Technology |
|---|---|---|
| M0–M2 | CLI + TUI | `clap` 4.6, `ratatui` 0.30, `alacritty_terminal` + a Kaisa terminal widget |
| M2 | Local web console | embedded static page over the event stream (§5.11); loopback, per-launch token |
| M3 | Desktop | **GPUI** + `gpui-component` 0.8 (pinned git revision), or **Slint** 1.18, decided by a spike |

**Spike exit criteria (review Q1).** Each criterion must pass on all three OSes:
- 10 live terminal panes at 60 fps;
- IME and wide-character input;
- screen-reader basics;
- signed packaging;
- licence redistribution checked (Slint requires attribution under its royalty-free licence);
- idle memory measured.

**Electron is out.** Orca.app is 700 MB and LearnHarness.app is 439 MB, and both ship with `RunAsNode` on and ASAR
integrity off. Tauri was rejected for the terminal-heavy UI: WebView2 memory on Windows is close to Electron's, and WebKitGTK on Linux is inconsistent. Mojo has no
GUI or daemon ecosystem.

---

## 13. Code organization

```
crates/
  kaisa-domain       newtype IDs, events (versioned + upcasters), error algebra — pure
  kaisa-sys          only crate with `unsafe`: pty, pidfd/cgroup, job objects, kqueue, sandboxes, peer creds
  kaisa-ledger       event log, projections, outbox, checkpoints, single writer actor
  kaisa-keeper       per-attempt keeper: PTY master, process handle, exit record, execution broker
  kaisa-supervisor   attempt state axes, leases, reconnect-to-keepers, reclaimer
  kaisa-workspace    worktrees, CoW, preservation protocol
  kaisa-scheduler    runs, tasks, roles, attempts, gates, cursors, receipts, pipeline advance
  kaisa-coordinator  epochs, journal, handoff packet, failover sequences
  kaisa-agents       agent catalog, brokered/observed adapters, quota signal classification
  kaisa-policy       policy compiler, signed snapshots, settings risk scanner
  kaisa-egress       per-attempt proxy with allow-list
  kaisa-hook         advisory hook entry, authenticated spool
  kaisa-verify       freeze, clean checkout, trusted checks, tamper detection, receipts
  kaisa-knowledge    knowledge kinds, promotion, decay, hybrid retrieval (FTS5 + vectors), usefulness
  kaisa-context      repository index, project profile, impact sets, context-pack assembly
  kaisa-embed        local embedding model + vector index behind one trait (Rust default; Mojo optional)
  kaisa-router       tier 0, grader, learned priors, escalation, admission, slots, budgets, gateway
  kaisa-console      local web console: loopback server, session token, SSE projection, static page
  kaisa-notify       OS notifications, presence checks, status-line files
  kaisa-integrations GitHub App (M1), Linear (M3), untrusted-input screening
  kaisa-packs        policy pack fetch/verify, templates, approval delegation
  kaisa-i18n         Fluent message catalog (ko, en), plain-language event renderer
  kaisa-protocol     KRPC types + framing
  kaisa-mcp          rmcp bridges (coordinator / worker tool sets)
  kaisa-daemon       composition root
  kaisa-cli          CLI + TUI
bin/kaisa
```

**Rules.** An architecture test and `cargo-deny` enforce these.
- `kaisa-domain` has no Kaisa dependencies.
- Only `kaisa-daemon` wires adapters.
- Only `kaisa-sys` contains `unsafe`, and every `unsafe` block has a `SAFETY:` comment.

**Style.** Kaisa follows the [Google Rust style guide](https://google.github.io/styleguide/rustguide.html):
- Small crates. No glob imports. rustfmt runs with `imports_granularity = "Module"` and `max_width = 100`.
- `Option`, `Result` and `panic!` follow the guide's rules. Use `expect` instead of `unwrap`, and never `catch_unwind`.
- IDs are newtypes, and structs use named fields.
- `#[non_exhaustive]`, `#[must_use]` and the `missing_docs` lint apply.

**Deviation (ADR-0004).** Kaisa uses `tokio`. The guide's no-async rule is tagged `[chrome]`.

---

## 14. Toolchain and dependencies

Versions were checked on crates.io, npm and ziglang.org on 2026-10-30.

| Item | Version | Role | v1 default |
|---|---|---|---|
| Rust | 1.99.1, edition 2024 | everything | — |
| tokio | 1.54.1 | async runtime | on |
| rmcp | 3.8.0 | MCP bridges | deferred: M0's bridge (`kaisa-mcp`) is a minimal server over the KRPC framing, because MCP stdio is the same newline-delimited JSON-RPC and M0 needs three methods; rmcp is re-evaluated when sampling, resources or HTTP transport are needed |
| rusqlite | 0.41.1 (bundled) | ledger | on |
| gix | 0.90.0 | git reads | on |
| alacritty_terminal | 0.26.0 | terminal state | on |
| ratatui | 0.30.2 | TUI | on |
| landlock | 0.4.7 | Linux sandbox | on |
| clap | 4.6.7 | CLI | on |
| blake3 | 1.8.7 | hashing | on |
| ed25519-dalek | 3.0.0 | signatures | on |
| schemars | 1.2.3 | JSON schema | on |
| jiff | 0.2.38 | time | on |
| usearch | 2.26.6 | vector index (native C++); candidate, beside pure-Rust `hnsw_rs` 0.3.5 | **on** after the evidence gate |
| fastembed | 7.5.0 | local embeddings; depends on `ort`, which has no stable release yet | **on** after the evidence gate; BM25-only and labelled until then |
| tree-sitter | 0.27.1 | repository index grammars | on |
| axum | 0.8.9 | web console server (loopback only) | on with `kaisa console` |
| wasmtime | 50.0.1 | policy extensions | **off** |
| Mojo | (toolchain) | optional embedding / vector accelerator, macOS and Linux only | **off**, ADR-0033 |
| gpui-component | 0.8.1 | desktop (M3) | — |
| Zig | 0.17.0 | `cargo-zigbuild` cross-linking only | — |
| TypeScript | 7.0.2 | not on the core path | — |

**Dependency evidence gate (review M8).** Before M0 closes, the dependency set must have:
- a committed lockfile with checksums;
- a stated MSRV and feature set;
- a licence inventory;
- passing link and run tests on all three OSes.

---

## 15. Budgets to measure (not claims)

| Metric | Budget | How |
|---|---|---|
| Daemon idle RSS | < 30 MB | platform APIs, CI on 3 OSes |
| Keeper RSS per attempt | < 4 MB | 10/50-agent load tests |
| Screen model memory per terminal | measured, bounded by the scrollback setting | load tests |
| `kaisa hook` cold start | < 5 ms | hyperfine |
| Event append p99 (`synchronous=FULL`) | < 2 ms | criterion |
| Exit detected → execution `Exited` | < 1 s | lifecycle tests |
| Orphaned resources after the chaos suite | 0 | GC diff |
| Work lost after the chaos suite | 0 bytes not covered by a receipt | preservation audit |
| Coordinator handoff (signal → successor reconciled) | < 60 s, excluding agent startup | failover tests |
| Per-role tool catalog + preamble | < 2k tokens | CI footprint report |
| Light (read-only) attempt overhead, excluding the agent CLI | < 4 MB, no worktree | load tests |
| 100 concurrent scripted agents on the reference host | daemon RSS < 200 MB, event append p99 < 5 ms, admission decision < 50 ms | scale benchmark (CI nightly, 10/50/100/200 agents) |
| Context-pack assembly on a 10k-file repository | p95 < 300 ms | criterion |
| Hybrid retrieval over 50k knowledge items | p95 < 50 ms | criterion |
| Embedding model asset | < 100 MB, fetched once, checksum-verified | release check |

---

## 16. Testing strategy

Stability is validated **after implementation** with:

- **Conformance suites.**
  - PTY behaviour on all three OSes: ConPTY drain and close order, resize, Unicode and replay. Kaisa's wrapper is compared with `portable-pty`.
  - `gix` against `git`: submodules, symlinks, long paths, sparse checkouts.
  - CoW fallback.
- **Negative security tests.** Run per OS, and part of the definition of done for M0 and M1. An agent must be unable to:
  - write outside its write set by any route: `exec` of a shell or interpreter, a build script, rename, hard link, symbolic link, an MCP server (§5.6) — one corpus case per route;
  - reach the control endpoint, `kaisa.db`, the keychain items or another attempt's keeper;
  - reach the network without the broker;
  - escape its descendants;
  - forge spool or verdict records.
- **Named lifecycle regressions.**
  - late handshake;
  - cancel during cleanup;
  - cleanup retry;
  - PID reuse;
  - daemon restart with keepers alive;
  - keeper loss;
  - lease expiry while alive;
  - stall escalation;
  - coordinator quota exhaustion;
  - worker failover with untracked and ignored files;
  - disk full.
- **Chaos suite.** Kill agents, keepers, the daemon and clients at random stages. The suite asserts zero orphans and zero
  unreceipted work loss.
- **Property tests.** Epoch fencing, cursor monotonicity and contiguity, receipt payload conflicts, outbox recovery,
  "never auto-retry Unknown".
- **Verification tamper corpus.** One case per ruflo ADR-503 gaming pattern. Each case must yield `Tampered`.
- **Team-intelligence properties (v0.6).**
  - nothing is delivered without every required evidence item on the exact source digest;
  - two Tasks with overlapping write sets are never in progress together;
  - the revision loop stops at its budget and on a repeated finding;
  - no route goes below a `never_downgrade` grade;
  - every knowledge item an agent received is in `injections`;
  - a secret corpus never survives into `knowledge`;
  - with the embedding model missing, retrieval reports "vector search off" instead of failing or pretending.
- **Mutation testing.** `cargo-mutants` with a minimum score on the keeper, supervisor, scheduler, coordinator, policy,
  verify and ledger crates.
- **Cross-OS CI.** Every PR runs on macOS, Linux and Windows. CI also runs the catalog and schema digest checks and the
  footprint report.

---

## 17. Milestones

Each milestone ends with a measured go/no-go review. Staffing and duration estimates belong in the project plan, not
here. The flows each milestone must complete end to end are listed in [`user-flows.md`](user-flows.md) §9. Its
usability targets (§10) are part of every milestone's exit criteria.

| Milestone | Scope | Exit criteria |
|---|---|---|
| **M0 — Safe single attempt** (its first result is *preserved*, not yet *verified*: verification arrives in M1, and the 10-minute "first verified result" target is measured from M1) | domain (v0.6 types: mission, run hierarchy, write set, grade, knowledge, context pack, route decision), sys, ledger + outbox, keeper, PTY, supervisor (state axes, leases, reconnect), workspace + preservation, authority split, minimum sandbox + egress broker on 3 OSes, **project profile + context pack v1** (spec + profile), Claude adapter in brokered mode, CLI, dependency evidence gate | Issue → attempt → exit → preserve → reclaim on all 3 OSes. The attempt starts from a recorded context pack and a confirmed project profile, with no setup. Negative tests pass. Daemon restart keeps attempts. The chaos suite shows zero orphans and zero unreceipted loss. |
| **M1 — Verified team** (personal agent registration `kaisa agent add` and personal default policy ship here, so F37 needs no admin) | scheduler (missions, roles incl. documentation, pipelines, write-set ownership, gates, cursors, receipts), verification loop + delivery gate + bounded revision, repository index + impact sets + full context packs, knowledge (lessons, decisions, trajectories; hybrid retrieval behind the evidence gate), router grader + catalog grades + `openai-compatible` local models + tier 0, Codex adapter, observed mode, policy snapshots, settings risk scanner, TUI | Nothing reaches the user without fresh evidence. Tampered candidates are never delivered. Blocking review findings are fixed by revision without a human in the common case. A second session on the same repository needs no re-explanation (its packs carry the profile and lessons). Pipelines advance without an LLM. Unknown is never retried. |
| **M2 — Resilient, learning, at scale** | coordinator epochs + handoff, worker failover, quota classification, admission + budgets (hard with a gateway or `hard_cap` adapters, otherwise bounded and labelled), learned route priors, injection usefulness scoring, sub-runs + roll-ups + measured slots, web console, Hermes adapter, brokered user MCP servers | A Claude → GPT handoff mid-run with no loss of plan, work or workers. Budgets hold within their stated bound. Cost per verified task falls over a recorded benchmark set as priors learn. The 100-agent benchmark meets its budgets. |
| **M3 — Extended** | best-of-N, team-shared knowledge, desktop (spike result), team server mode, remote workers, Mojo accelerator spike | Pilot team in production. |

---

## 18. Decision record (ADR index)

| ADR | Decision | Status |
|---|---|---|
| 0001 | Rust core; Zig only for cross-linking; Mojo only as an optional accelerator (ADR-0033); TypeScript off the core path | Proposed (amended in v0.6) |
| 0002 | One core binary, one daemon, one protocol; desktop as a separate GUI package | Proposed |
| 0003 | SQLite event log + projections + outbox, single writer, signed checkpoints, upcast events | Proposed |
| 0004 | tokio async (documented deviation from the guide's `[chrome]` rule) | Proposed |
| 0005 | PTY layer chosen by a conformance suite (own wrapper vs `portable-pty`) | **Accepted: own wrapper** — passes all scenarios on 3 OSes and keeps process-tree ownership; portable-pty 0.9.0 failed or hung on Windows ([`research/pty-conformance.md`](research/pty-conformance.md)) |
| 0006 | Keep Orca's safety invariants; replace its operating model with Run / Task / Attempt / Gate and system-carried obligations | Proposed |
| 0007 | Hooks are advisory; enforcement at the execution broker and the OS boundary; brokered vs observed trust modes | Proposed |
| 0008 | Containment matrix with minimum OS versions; reduced modes need a Gate; egress through a per-attempt broker | Proposed |
| 0009 | Learn only from verification receipts plus merge; learned changes are proposals, never self-promoted | Amended by 0031 and 0032: knowledge and route priors adapt automatically inside bounds; policy changes stay proposals |
| 0010 | UI: TUI first; desktop GPUI or Slint decided by spike criteria; no Electron | Proposed |
| 0011 | Lease-based lifecycle with four state axes; preservation protocol before reclamation; retain by request | Proposed |
| 0012 | Coordinator epochs; daemon-assembled handoff packet; quiescent coordinator and worker failover | Proposed |
| 0013 | Per-attempt keeper process for durable ownership across daemon restarts | Proposed |
| 0014 | Human and agent authority split (control endpoint vs capability-scoped agent bridges) | Proposed |
| 0015 | Verification on immutable snapshots with trusted checks, tamper detection and receipt-bound approval | Proposed |
| 0016 | Admission control with cost reservation; optional gateway enforcement with per-attempt virtual keys | Proposed |
| 0017 | Requester authority for in-agent requests: it can start runs, and can never approve | Proposed |
| 0018 | Notifier with presence-checked actions | Proposed |
| 0019 | Issue-tracker connector as an untrusted input source; GitHub App credentials | Proposed |
| 0020 | Undo through declared effect inverses, and restore from preservation receipts | Proposed |
| 0021 | Signed policy packs (templates, approvals, delegation) distributed as git repositories | Proposed |
| 0022 | Fluent localization (ko, en) and a plain-language renderer; every blocking event declares a fix option | Proposed |
| 0023 | Answers and Decisions as distinct types; Decisions never time out into "yes" and carry a scope and an expiry | Proposed |
| 0024 | Effect previews with digest-confirmed actions ("you approve exactly what you saw") | Proposed |
| 0025 | Path-based approval rules (self / second-person / codeowners / n-of-m) with recorded reassignment | Proposed |
| 0026 | CI exit-code contract (0/3/4/5/6), a versioned JSON schema, and `run --resume` | Proposed |
| 0027 | Admin lifecycle: hardware-backed enrollment, simulated validation, staged rollout, anti-replay rollback, offboarding receipts, agent conformance | Proposed |
| 0028 | Missions own delivery and budget; large Missions split into sub-runs with daemon-built roll-ups | Proposed |
| 0029 | Write-set ownership is a scheduler invariant; impact sets join verification | Proposed |
| 0030 | Verification loop: schema-validated review verdicts, bounded automatic revision, one delivery gate on fresh evidence | Proposed |
| 0031 | Knowledge and context engine are first-class and on by default; content-addressed context packs; every injection logged and scored | Proposed |
| 0032 | Router on the spawn path: deterministic grader, per-category priors from verified outcomes, escalation by model before effort | Proposed |
| 0033 | Local embeddings and vector search in Rust by default; Mojo only as an optional macOS/Linux accelerator, because Windows parity is a release blocker | Proposed |
| 0034 | Local web console: loopback only, started by `kaisa console`, per-launch token, read-only by default | Proposed |
| 0035 | Agent catalog kinds (`cli-agent`, `openai-compatible`); user MCP servers only through the broker, sandboxed, pinned, with effect classes and outbox-governed external effects | Proposed |

---

## 19. Open questions

1. Do Claude Code and Codex fully support **brokered** mode, meaning built-in tools disabled and every tool served over MCP, on all
   three OSes? If not, which agents are observed-only? (v0.6.1, review M10: answered by a conformance spike on all
   three OSes that is a go/no-go **before** the Claude adapter task in M0, not after it.)
2. Which providers expose readable subscription usage, so that switching can be proactive?
3. Can cgroup v2 delegation be expected on company Linux machines and CI runners? If it cannot, the reduced mode is the default there.
4. Does the AppContainer + egress-broker profile let package managers and the agent CLIs' own login flows work?
5. Which external anchor fits the company for audit checkpoints: git notes, an internal service or WORM storage?
6. Remote and team mode (deferred). The plan is pinned-key pairing with short-lived invites over an E2EE relay. Should the knowledge base be scoped per repository or per team?

---

## Appendix A. v0.2 review findings → resolution

| Finding | Resolution |
|---|---|
| B1 same-user authority | §5.7 authority split, §6, §9 (ADR-0014) |
| B2 containment equivalence | §5.7 containment matrix + reduced mode + negative tests (ADR-0008) |
| B3 hook mediation | §5.6 brokered / observed modes, broker tool contracts (ADR-0007) |
| B4 failover writer races | §5.3 revoke → drain → quiesce → preserve → grant |
| B5 restart ownership | §5.1 keeper, single-instance lock, Unknown preserved (ADR-0013) |
| B6 reclamation safety | §5.5 preservation protocol, encrypted archive for flagged content, quotas |
| M1 lifecycle semantics | §5.1 four state axes, bounded stall escalation, monotonic deadlines |
| M2 external effects | §5.2 outbox + effect receipts + payload-conflict rejection; §5.4 durability |
| M3 cursor / steer | §5.2 epoch-scoped contiguous cursors; steer states with `Uncertain` |
| M4 packet completeness | §5.3 versioned, content-addressed packet; epoch on every mutation; successor reconciliation |
| M5 policy snapshot | §5.7 signed immutable snapshots, fail closed, broker re-check, authenticated spool, WASM limits |
| M6 verification integrity | §5.8 freeze, clean checkout, trusted checks, tamper detection, receipt-bound approval |
| M7 quota / budgets | §5.10 classification, backoff, hysteresis, reservation, gateway enforcement |
| M8 dependency closure | §14 dependency evidence gate; vector, embedding and WASM off by default |
| M9 PTY / workspace conformance | §5.1, §5.5, §16 conformance suites |
| M10 credentials | §9.3 virtual keys, auth-dir exposure, documented limits; §8 interactive-user service |
| m1 audit overclaim | §5.4 signed checkpoints, external anchor, documented limit |
| m2 contradictions | P14 and §4: read-only commands do not start the daemon; stdio MCP bridges with no loopback listener; desktop is a separate package |
| m3 milestone order | §17 minimum safety in M0; optional engines deferred |
| m4 terminal model | §5.1 versioned screen snapshot; §15 screen-memory budget |
| Q1 GPUI spike | §12 exit criteria |
| Q2 upgrade evidence | §5.4 event versioning and upcasting; §16 replay tests |

## Appendix B. ruflo strengths adopted

**v0.6.** The eight capabilities people use ruflo for are rebuilt so that Kaisa enforces each one
where it executes (P17), based on [`research/ruflo-control.md`](research/ruflo-control.md):
team splitting → missions, sub-runs and write-set ownership (§5.2, §5.13); cross-checking →
verification loop and delivery gate (§5.8); project memory and self-learning retrieval → knowledge
and context engine (§5.9); cost routing → router on the spawn path (§5.10); MCP and ecosystem →
catalog kinds and brokered user MCP servers (§5.6); TUI and web console (§5.11).

**v0.3.**

Role table (§5.2), event-driven pipeline advance (§5.2), verified-outcome learning (§5.9), gaming
defences (§5.8), compaction snapshot (§5.6, §5.3), cost ladder and gateway budgets (§5.10), risky-settings
refusal (§5.7), context-footprint budget (§5.10, §15), deterministic tier 0 (§5.10), no
self-promotion (§5.9), `pre-bash` tokenizer (§5.7), auto-memory ingestion (§5.9), witness/catalog digests
(§16).

## Appendix C. v0.6 review findings → resolution (v0.6.1)

| Finding | Resolution |
|---|---|
| B1 `exec` bypasses the write set | §5.7 write-set confinement by the OS for every process; scratch area; link rules; diff check as a second line; §16 corpus per route |
| B2 MCP relay as an effect bypass | §5.6 sandboxed, pinned servers; effect classes; `external` calls through the outbox, previews and Decisions; untrusted output |
| B3 integration skips review and security | §5.8 every integration, conflict revision and final Mission result is a candidate with its own evidence; §5.13 |
| M1 quoted knowledge is not a defence | §5.9 provenance vs truth: machine-evidence lessons, quarantine and taint, no commands or permissions in items, adversarial corpus |
| M2 gameable learning | §5.9 ablation-based usefulness; §5.10 whole-Task rewards, version-keyed priors, minimum samples, caps, shadow benchmark and rollback |
| M3 optional hard budget | §5.10 atomic chain reservation, per-request ceilings, `hard_cap` capability, shown overrun bound; §17 wording |
| M4 loopback reachable through proxies | §5.7 broker refuses host-internal destinations after DNS and redirects; §5.11 token, CSRF, lifetime, rendering, residual risk |
| M5 untrusted local endpoints | §5.10 pinned identity and model, data classification, limits and isolation; signed embedding manifest |
| M6 context overflow and freshness | §5.9 required sections never cut (split instead), cumulative budgets, unknown impact widens checks, version-keyed invalidation |
| M7 screens contradict the contract | §5.8 one eligibility state machine; `unverified` and `overridden` distinct; verified answers; user-flows F03, F06, F09 |
| M8 incomplete verification execution | §5.8 ephemeral build area, test-change review, failure taxonomy, reviewer fallback, daemon-validated JSON verdicts |
| M9 scripted 100 ≠ real 100 | §5.13 engine-scale and service-scale benchmarks, Mission-wide write-set reservation, deadlock/starvation/backpressure tests; §5.2 one keeper per attempt |
| M10 ease of use vs milestones | §17 M0 promises a preserved result; personal `kaisa agent add` in M1; §19.1 brokered spike before the adapter |
| M11 inconsistent grades and authority | §5.2 single role-grade table with floors; security T3; `self` means the initiating human |
| M12 expiry, forgetting, restart | §5.12 expiry re-checked per dispatch and effect, freeze on expiry; §5.9 tombstones and watermark-rebuilt projections |
| N1 drift | Linear in M3 everywhere; F26 exit 6; default output says "light model", not T1; PRD vector wording |

## Licensing note

Kaisa borrows **ideas**, not code. LearnHarness is AGPL-3.0, and no code or prompts are taken from it. rea and ruflo are MIT,
so any copied code will carry attribution. Orca and herdr are studied only through their shipped artifacts and public docs.
