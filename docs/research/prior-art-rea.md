# Prior art: Orca, LearnHarness and ruflo, investigated with REA

| | |
|---|---|
| Date | 2026-10-30 |
| Tool | [REA](https://github.com/morluto/rea) `rea-agents` 6.8.0 on Node 24.22.1 |
| Method | Static analysis only. Nothing was launched, no network calls were made, and no target files were modified. |
| Detailed reports | [`rea/orca.md`](rea/orca.md) · [`rea/learnharness.md`](rea/learnharness.md) · [`rea/ruflo.md`](rea/ruflo.md) |

Every claim in the detailed reports carries one of three evidence tags:
- **[REA]**: REA Evidence id
- **[FILE]**: direct reading of the shipped files
- **[?]**: unknown

The headline facts below were re-checked by hand against the shipped files.

## Targets

| Target | Artifact analysed | Version |
|---|---|---|
| Orca | `/Applications/Orca.app` (Electron) | 1.4.244 |
| LearnHarness | `/Applications/LearnHarness.app` (Electron + Next.js standalone server) | 0.4.0 |
| ruflo | npm `ruflo@3.65.0` → `@claude-flow/cli@3.65.0`, installed with `--ignore-scripts` | 3.65.0 |

## REA coverage and limits

| Run | Result |
|---|---|
| `inspect-desktop-application` Orca / LearnHarness | complete (6,482 / 5,506 files) |
| `inspect-electron-fuses` Orca / LearnHarness | complete |
| `analyze-javascript-application` LearnHarness `app.asar` | complete: 228 JS files parsed, 2,408 findings, 0 parse failures; semantic graph capped at 100k nodes |
| `analyze-javascript-application` ruflo `@claude-flow/cli` | complete: 6,890 JS files parsed, 25,177 findings, 0 parse failures; semantic graph capped |
| `analyze-javascript-application` Orca `app.asar` | **failed**: provider timeout (300 s per source) on a large main-process chunk; facts were taken from direct reading of the mostly unpacked `app.asar.unpacked/out` and scoped runs |
| LearnHarness Next.js server chunks | **did not finish** (stopped after about 10 min); confirmed by grep only |
| `compare-source-to-bundle` LearnHarness Electron shell vs upstream | 10 files unchanged, 0 modified, 0 removed, 1 unknown |
| Native binaries (`agent-browser`, `orca_shipit_scheduling.node`) | inventory only, since no native disassembler was installed; details came from `strings` |

## Headline findings

### Footprint

| Target | Size | Composition |
|---|---|---|
| Orca.app | 700 MB | Electron app (renderer 69 MB), Node daemon template `orcad-template` (54 MB), Rust `agent-browser` (10 MB), sherpa-onnx speech |
| LearnHarness.app | 439 MB | Electron launcher plus a second Electron binary running as the Node server (`server/standalone`, 173 MB) |
| ruflo | 1.7 GB installed | `@huggingface` 465 MB, `@claude-flow/*` 221 MB, `@opentelemetry` 199 MB, `onnxruntime-node` 96 MB |

### Electron hardening (Orca and LearnHarness, same result)

Fuse states:
- `RunAsNode`: enabled
- `EnableNodeOptionsEnvironmentVariable`: enabled
- `EnableNodeCliInspectArguments`: enabled
- `EmbeddedAsarIntegrityValidation`: disabled
- `OnlyLoadAppFromAsar`: disabled
- `EnableCookieEncryption`: disabled

Both apps rely on `ELECTRON_RUN_AS_NODE=1` to start their Node side (`orcad`, the Next server).

### Orca: orchestration model worth adopting

- **Process roles.** There are three server roles: a terminal-host daemon (`daemon-vNN.sock` with a `.token` and a `.pid`), the app runtime RPC (`orca-runtime.json` with an `authToken`, NDJSON) and `orcad` for headless hosts.
- **Storage.**
  - `node:sqlite` with `user_version` 43.
  - The tables for runs, tasks, dispatch contexts, messages, deliveries and decision gates are listed in [`rea/orca.md`](rea/orca.md).
  - `mutation_receipts` provide idempotency, and `attempt_observation_facts` is an append-only evidence table.
  - Federation uses relay items.
- **Worker start.** The pipeline is persisted stage by stage: `accepted → worktree_creating → worktree_created → terminal_creating → terminal_readying → [setup_start] → input_accepted → ready`. Ambiguous outcomes become `start_unknown` / `stop_unknown` and are never auto-retried.
- **Terminals.**
  - `node-pty` runs inside the daemon, with the ConPTY DLL and Job Objects on Windows.
  - `@xterm/headless` and the serialize addon produce reattach snapshots.
  - OSC 133 shell integration marks command boundaries.
- **Agents.** 44 agents are driven from one config table that declares the detect command, prompt-injection mode, ready signals and model / effort / permission flag mappers. About 40 provider hook translators feed a loopback hook listener, which has a JSONL spool fallback.
- **Weak points.**
  - Tokens are stored in plaintext 0600 files.
  - The compatibility surface is large: 53 daemon protocol versions and 43 DB versions.
  - Liveness partly depends on terminal-title and spinner heuristics.

### LearnHarness: harness patterns worth adopting (ideas only, AGPL)

- **Agent CLI as a bare transport.** The `claude` CLI runs with its own built-in tools disabled. Tools are served over a per-turn loopback MCP endpoint with a bearer token.
- **Delivery semantics.** Job and steer ids are deterministic. A message counts as delivered if and only if it is present in the durable log.
- **Cost and concurrency.** Background LLM work shares one global FIFO semaphore with 3 permits. Usage is stored as tokens, and dollars are derived at read time.
- **Turn safety.** Each turn is persisted write-ahead. When a tool call is left dangling, an interrupted result is synthesized for it.
- **Weak points.**
  - Credentials sit in plaintext JSON files.
  - The full parent environment is forwarded to the server.
  - Chat and job writes are not atomic.
  - Crash recovery is lazy and triggered by polling.
  - Sender validation is uneven across IPC channels.

### ruflo: what actually ships

- **Side effects on ordinary commands.** An npm update check, a helper refresh into the project and into `~/.claude/helpers`, config adoption, and a detached daemon autostart.
- **MCP stdio mode.** This is the only side-effect-free startup path. It imports about 50 tool modules and advertises about 300 tools by default.
- **Hooks.** Every hook is a fresh Node process, and `session-restore` spawns two more detached CLIs.
- **Embeddings.** The fallback chain ends in a deterministic hash embedding, and agentdb's mock-embedding warnings are suppressed.
- **Stale version constants.** The stdio servers hardcode `VERSION = '3.0.0'` (`bin/cli.js`, `bin/mcp-server.js`).
- **Network endpoints.** The npm registry, Anthropic, OpenRouter, Pinata/IPFS and `funnel.ruv.io` (promo/attribution). Consent defaults to off; the first-run behaviour is unverified.

## Mapping to Kaisa

| Kaisa decision | Evidence |
|---|---|
| Single binary, single daemon, single protocol (ADR-0002) | Orca's three server roles and version surface; LearnHarness's second Electron runtime |
| Run / Task / Dispatch / Gate / Delivery with fencing and receipts (ADR-0006) | Orca orchestration schema and worker-start pipeline |
| Daemon-owned PTYs with snapshots | Orca terminal-host daemon, `@xterm/headless` serialize |
| Data-driven agent catalog, interactive + headless modes | Orca 44-agent table; LearnHarness transport mode |
| Native hook with a local policy decision (ADR-0007) | ruflo's Node-per-hook cost; Orca's hook spool |
| Peer-credential IPC, keychain secrets, allow-listed child env | Plaintext tokens in Orca and LearnHarness; full env forwarding in LearnHarness |
| No silent degradation (P6) | ruflo's hash-embedding fallback |
| No side effects on read-only calls (P7) | ruflo startup behaviour |
| No Electron (ADR-0010) | Footprint table and fuse findings above |
