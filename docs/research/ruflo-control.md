# How ruflo controls its headline capabilities — and what Kaisa does instead

| | |
|---|---|
| Date | 2026-10-30 |
| Method | Three supervised read-only workers (Orca Run `run_7c2c0d8abe1b`): Claude Sonnet 5.5 for swarm/MCP and memory/routing/UI, OpenAI Codex for verification/governance, so the verification topic got a second vendor's reading. The coordinator cross-checked the reports against the code citations. |
| Evidence | [`ruflo-control/a-swarm-and-mcp.md`](ruflo-control/a-swarm-and-mcp.md) · [`ruflo-control/b-verification-and-governance.md`](ruflo-control/b-verification-and-governance.md) · [`ruflo-control/c-memory-routing-and-monitoring.md`](ruflo-control/c-memory-routing-and-monitoring.md) (Korean, with `file:line` citations into ruflo at `d000e27e8`) |
| Rule | Ideas only. No ruflo code, prompts or text is copied into Kaisa. |

## 1. The one finding behind all eight capabilities

ruflo's *records* and ruflo's *execution* are different things, and most of its controls sit on the
record side. Swarm, agent, task and consensus tools write JSON ledgers; they never start, stop or
constrain a process. Recommendations (agent, model, "run tests") are printed text the model may
ignore. Policy enforcement, injection screening and signature strictness are opt-in. The places where
ruflo really *enforces* something are few and specific:

| Enforced in ruflo | Where |
|---|---|
| One tool-call chokepoint with a policy decision and a receipt | `callMCPTool` → `authorizeMcpTool` (a) |
| Child-process limits: process-group kill, timeouts, output caps, `maxConcurrent`, `maxWriters`, worktree cwd | headless worker executor, dual-mode orchestrator (a) |
| Mission state machine over an append-only, hash-chained event log | `missions/` (c) |
| Budget hard stop at the next agent spawn; real blocking at an external gateway | ADR-508 (c) |
| Dangerous-shell patterns blocked by a `PreToolUse` helper; risky plugin admission | (b) |

Kaisa's answer is a design rule, not a feature: **a capability exists only where Kaisa itself is on
the execution path.** Kaisa starts every agent, so it can enforce what ruflo can only recommend.

## 2. Capability by capability

| # | Capability | How ruflo controls it | Enforced? | Kaisa redesign (architecture v0.6) |
|---|---|---|---|---|
| 1 | Context collapse → team of scoped agents | Roles are labels on records; the host (Claude Code Task tool) runs the agents; each agent sees whatever its prompt says | No | **Context packs** assembled by Kaisa per attempt, with a declared write set. Overlapping write sets never run at once (scheduler invariant). |
| 2 | Write → review/test → security loop | Hooks record caller-supplied `success`; dual run treats exit 0 as done even when the reviewer reports a vulnerability; no re-fix step | No | **Verification loop as a state machine**: deterministic checks → structured review verdict → security review → automatic revision with findings, bounded → human. One **delivery gate** checks fresh evidence on the exact source digest. |
| 3 | Persistent project memory | SQLite/AgentDB store; injection is a hook printing ~5 keyword-matched 80-character summaries; the learning layer is an optional dependency that silently turns off | Partly | **Knowledge as a first-class, event-sourced context**: an auto-derived project profile (zero setup), lessons and decisions; injected by Kaisa into every context pack within a budget; every injection logged. |
| 4 | Cost control by model routing | Static keyword table and a Thompson bandit produce recommendations; only ruflo-registered agent records get a model; Claude Code subagents are untouched | Mostly no | **Router on the spawn path**: a deterministic grader sets the grade, the router picks agent × model × effort, per-request usage events drive budgets. Local OpenAI-compatible models (for example Ollama) serve the light tier. |
| 5 | Swarm of 100+ role agents | `maxAgents` clamped to 50 and never checked; real concurrency is `maxConcurrent` 2–4; "100+" is a record count | No | **Hierarchical runs** (sub-coordinators), lightweight read-only attempts, measured per-agent resource budgets. "100+" is a benchmarked target, never a label. |
| 6 | Self-learning memory with vector RAG | Learning signals are "matched again" heuristics; default injection is keyword, not vector; HNSW only when an agent calls a tool | Partly | **Learning from verified outcomes only**: trajectories and lessons are promoted when the delivery gate passes; hybrid BM25 + vector retrieval on by default; injection usefulness is measured against outcomes. |
| 7 | MCP and ecosystem | Static tool registry, one chokepoint; Claude Code and Codex register ruflo as an MCP server | Yes (chokepoint) | Keep the single chokepoint, enforce by default. Agent catalog adds Hermes and generic OpenAI-compatible endpoints; user MCP servers are relayed through the broker under policy. |
| 8 | TUI and web console | Status line, `ruflo tui`, console plugin reading ruflo's own files, "n/a when not measured" | Display only | TUI over the event stream (M1); **local web console** on loopback with a per-launch token and origin checks (new ADR), read-only by default. |

## 3. Ideas worth keeping (rebuilt, not copied)

- Separate the ledger from the executor, and name operations so `record` never sounds like `run`.
- One chokepoint for every tool call, fail-closed when policy is on — and policy on by default.
- Leases need expiry and renewal; a claim without an enforced TTL locks work forever.
- Budget attribution in the app, enforcement at a hard boundary, a validated file between them.
- An append-only, hash-chained event log with a rebuildable snapshot and one transition table.
- Show `n/a` for anything not measured; never compute a metric from a constant or a random number.
- Per-category priors for routing; a global prior lets one task type poison all others.

## 4. Mistakes Kaisa must not repeat

- Treating exit 0 or a caller-supplied `success` as quality.
- Fields that look like controls but are never enforced (`topology`, `maxAgents`, `ttlMs`).
- Learning that silently turns off when an optional dependency is missing.
- Two implementations of one concept (a real coordinator package beside a ledger of the same name).
- Leaving inter-agent messaging to the host instead of owning the channel.
- Estimating per-request cost by splitting a turn's cost across tool calls.

## 5. Language purposes (Context7, 2026-10-30)

| Language | Why it was created | Where Kaisa uses it |
|---|---|---|
| Rust | Memory and thread safety without a garbage collector; reliability through the type system and ownership | The whole engine: supervisor, ledger, scheduler, sandbox, router, knowledge store |
| Zig | A simpler replacement for C, and a cross-compiling toolchain that ships libc (`zig cc`) | Release builds of C dependencies (SQLite, blake3) for all three OSes |
| Mojo | One language for heterogeneous AI hardware (CPU, GPU, accelerators) with Python interoperability | Candidate accelerator for local embedding and vector search. Native on macOS and Linux only (Windows through WSL), so it can only ever be an optional backend behind a Rust implementation that every OS runs. |
