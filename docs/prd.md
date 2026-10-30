# Kaisa — Product Requirements Document

| | |
|---|---|
| Status | v1.0 draft |
| Date | 2026-10-30 |
| Sources | [`user-flows.md`](user-flows.md) v0.2 (what users do) · [`architecture.md`](architecture.md) v0.5 (how it works) |
| Task breakdown | [`tasks/M0.md`](tasks/M0.md) (current milestone) |

## 1. Problem

Developers already use coding agents such as Claude Code and Codex. When they want **several agents
working at once**, three things go wrong:

1. **Babysitting.** People switch branches, watch terminals and clean up after agents by hand.
   Orchestrators that exist today, such as Orca, push a supervision protocol onto the user or the LLM, and
   finished terminals pile up.
2. **Trust.** Agents edit and run code with the user's full rights. Hooks can be bypassed. Results
   are reported by the agent itself, so a weakened test still shows "passed".
3. **Fragility.** When one provider's quota runs out, the run stops, because the coordinator was an
   LLM session that held all the state.

## 2. Product

Kaisa is a single cross-platform binary that:

- **runs** agents in isolated workspaces;
- **supervises** them without help from the LLM;
- **contains** them with an OS sandbox and a broker;
- **verifies** results on immutable snapshots;
- asks a human only for real **decisions**;
- **keeps work moving** across providers.

## 3. Users

| ID | Persona | Primary need |
|---|---|---|
| P1 | Developer | Finish tasks faster without babysitting |
| P2 | Reviewer / Tech lead | Approve only verified changes |
| P3 | Admin | Safe defaults, budgets, audit |
| P4 | Occasional user | Small changes without git knowledge (M3) |
| P5 | Automation | Unattended runs under policy |

## 4. Goals and success metrics

| Goal | Metric | Target |
|---|---|---|
| G1 Easy to use | Time from install to the first verified result, for a new user without docs | ≤ 10 min |
| G1 | Commands a P1 user needs for daily work | 3 |
| G2 Nothing is lost | Resources orphaned and work lost without a receipt, after the chaos suite | 0 / 0 |
| G3 Safe | Negative security tests passing on every OS | 100% |
| G3 | Candidates with weakened tests that get approved | 0 |
| G4 Resilient | Plan or work lost on a provider handoff | 0 |
| G5 Lightweight | Daemon idle RSS | < 30 MB (measured) |
| G6 Honest | Published numbers not backed by a CI measurement | 0 |

## 5. Scope

### In scope (v1, M0–M2)

- CLI and TUI on macOS, Linux and Windows.
- Claude Code and Codex adapters.
- Brokered and observed trust modes.
- Worktrees with preservation.
- Sandbox and egress broker.
- Verify Gate.
- Coordinator and worker failover.
- Budgets.
- GitHub connector.
- ko / en localization.

### Out of scope (v1)

- Hosting models.
- An IDE.
- Multi-tenant SaaS.
- Distributed consensus.
- The desktop app, remote peers, Linear, mobile and chat. These come in M3 or later.

## 6. Functional requirements

Each requirement links to the flow that motivates it and to the milestone where it ships.

| ID | Requirement | Flows | Milestone |
|---|---|---|---|
| FR-01 | One signed binary per OS; per-user install without admin rights | F01 | M0 |
| FR-02 | `--version` / `--help`; unimplemented commands fail honestly, with a non-zero exit code | F01 | M0 |
| FR-03 | Identifiers for runs, tasks and attempts that are validated and display-stable | — | M0 |
| FR-04 | Append-only, hash-chained event log with a single writer; tampering is detectable | F24 | M0 |
| FR-05 | Attempt lifecycle modelled on four independent state axes, with legal transitions only | F15, F19l/m | M0 |
| FR-06 | Outbox: an effect intent, its event and the projection commit atomically; recovery reconciles pending intents | F17, F19l | M0 |
| FR-07 | KRPC framing: NDJSON JSON-RPC with bounded frames and capability negotiation | — | M0 |
| FR-08 | Keeper owns the PTY and process handle on 3 OSes and writes a durable exit record | F15, F19l | M0 |
| FR-09 | Workspace from committed state; dirty-tree choice; preservation protocol before removal | F18, F31 | M0 |
| FR-10 | Authority split: control endpoint vs agent bridge; agents cannot reach control | F05, F14 | M0 |
| FR-11 | Minimum sandbox + egress broker per OS; a reduced mode needs a Decision | F02, F19n | M0 |
| FR-12 | Claude adapter in brokered mode; one-candidate run end to end | F04 | M0 |
| FR-13 | Scheduler: roles, pipelines advanced by events, Gates, cursors, receipts | F07, F10 | M1 |
| FR-14 | Verify Gate on immutable snapshots, with tamper detection | F06, F16, F19g | M1 |
| FR-15 | Answers vs Decisions; scoped, expiring Decisions; effect previews | F13, F14, F16 | M1 |
| FR-16 | Codex adapter; observed mode (`wrap`) | F29 | M1 |
| FR-17 | GitHub connector with untrusted-input screening | F08 | M1 |
| FR-18 | Coordinator epochs, handoff packet, worker failover | F19c/d | M2 |
| FR-19 | Admission control, budgets, quota classification | F19j, F23 | M2 |
| FR-20 | CI contract (exit codes 0/3/4/5/6), `run --resume` | F26 | M2 |

## 7. Non-functional requirements

| ID | Requirement |
|---|---|
| NFR-01 | All three OSes are release blockers. Every PR runs CI on all three. |
| NFR-02 | The Google Rust style guide applies. `unsafe` is allowed only in `kaisa-sys`, and every use carries a `SAFETY:` comment. |
| NFR-03 | Read-only commands have no side effects. |
| NFR-04 | No silent degradation. A reduced capability is always labelled. |
| NFR-05 | Every user-facing string comes from the ko/en message catalog, from the first user-facing feature onward. |
| NFR-06 | Tests follow the three test conventions in [`testing.md`](testing.md). |
| NFR-07 | Dependencies pass `cargo-deny`. A licence is added to the allow-list only when a reviewed dependency needs it. |
| NFR-08 | Numbers in docs are budgets until a CI benchmark measures them. |

## 8. Release plan

| Milestone | Theme | Exit (summary) |
|---|---|---|
| M0 | Safe single attempt | Issue → attempt → exit → preserve → reclaim on 3 OSes; negative tests and chaos suite green |
| M1 | Supervised runs | Verified candidates only reach review; pipelines advance without an LLM |
| M2 | Resilient runs | Claude → GPT handoff mid-run without loss; budgets enforced |
| M3 | Extended | Pilot team in production |

## 9. Risks

| Risk | Mitigation |
|---|---|
| Agents may not support brokered mode on every OS | Observed mode as the labelled fallback; record support per agent with a conformance check (open question §19.1) |
| Sandbox capabilities differ by OS version | Minimum version matrix, reduced mode behind a Decision, negative tests per OS |
| Scope creep | Milestone exit criteria are measured; optional engines (vectors, WASM, desktop) stay off until M3 |
| Prior-art licensing (LearnHarness is AGPL) | Ideas only; no code or prompts copied |
