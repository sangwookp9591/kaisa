# ruflo@3.65.0 / @claude-flow/cli@3.65.0 - static shipped-artifact report

Tags: [REA] = in REA evidence; [READ] = direct file reading/grep of the installed tree; [UNK] = not determined.
Root: `/private/tmp/claude-501/-Users-psw-Projects-ruflo/096e5429-5666-41e0-9d50-63650dc75cba/scratchpad/target-ruflo/node_modules` (CLI = `@claude-flow/cli`, abbreviated `cli/`).
REA evidence: `evidence/ruflo-cli.json` (parent `ev_06a31f380ab0c5242a5f0f382bf31603697c2b9c7db45a4b8a05f3cbd14ec7a4`; summary `ev_9ede94c7...5985b`; extract `evidence/ruflo-extract.txt`).

**REA caveat.** The run covered the whole `cli/` dir including its nested 67 MB `node_modules`: 10,127 files, 6,890 JS, 25,177 findings, 0 parse failures, 0 Electron surface [REA]. The semantic graph hit its 100,000-node cap, so it kept only 4 child-process nodes and 12 request nodes. REA's child-process/request lists are therefore partial and mostly argv-less ("dynamic"). The tallies below come from grep [READ].

## 1. Startup path and cost

**Normal CLI** (`cli/bin/cli.js` -> `dist/src/index.js`) [READ]
- `--version` is a fast path that exits before any import.
- `index.js` statically imports parser, output, command registry, `update/` and `blocking-stdio`. Subcommands load lazily.
- Before every command except `init`, `update` and observe-only ones (`harness`), it runs, in this order:
  1. A non-blocking npm-registry update check (`update/checker.js:35`, 24h rate limit).
  2. `autoMigratePolicyStateIfNeeded(cwd)`.
  3. An awaited `autoRefreshHelpersIfStale(cwd, {alsoRefreshGlobal:true})`. This **writes `.claude/helpers` in the project and `~/.claude/helpers` globally**, with integrity/tamper-heal checks against a signed manifest.
  4. `autoAdoptProvenConfigIfStale` and `applyChampion`.
  5. `ensureDaemonRunning` (`services/daemon-autostart.js`): a **detached `node cli daemon start`** per project, with a 12h TTL and 30m idle. Opt out with `RUFLO_DAEMON_AUTOSTART=0`.
- On exit it calls `shutdownBridge()`.

**MCP stdio** (`!stdin.isTTY` and no args, or `mcp start`) [READ]
- It skips all of the above.
- It imports `mcp-client.js`, which **statically imports about 50 tool modules** (lines 11-84) and builds one `TOOL_REGISTRY` Map. It also imports the policy enforcer.
- Heavy deps (`agentdb`, `ruvector`, `@ruvector/*`) are dynamic imports, so they load on first tool use.
- `bin/cli.js` installs console filters first, redirecting ONNX/model-download log lines to stderr (#2253).
- There is a 10 MB stdin cap.

**Child processes** [READ, 74 files import child_process; REA saw only 4 spawn/fork sites]
- Counts from grep: git (17), gh (11), gcloud (15), npm/npx (about 16), ps/tasklist/taskkill/launchctl/systemctl (daemon and process mgmt), `claude` (3 spawn, i.e. headless `claude -p`), docker, python3, agent-browser, powershell.
- REA-confirmed: `fork` in `commands/daemon.js:574`, `spawn` in `appliance/rvfa-runner.js`, `spawn` in `.claude/helpers/hook-handler.cjs`.
- I found no `codex exec` in the dist/src grep.

**Network** [READ, REA partial]
- Hosts found in dist/src and helpers:
  - registry.npmjs.org (update check; plugin discovery)
  - api.anthropic.com (`agent_execute`, `ANTHROPIC_BASE_URL` override)
  - openrouter.ai, api.openai.com, ollama.com, generativelanguage.googleapis.com
  - IPFS and Pinata (`gateway.pinata.cloud`, `api.pinata.cloud`, ipfs.io, w3s.link, dweb.link, cloudflare-ipfs, web3.storage) for the plugin registry
  - huggingface.co (model download)
  - `us-central1-claude-flow.cloudfunctions.net/publish-registry` (`services/registry-api.js`)
  - **`funnel.ruv.io`** (`/enroll`, `/v1/click`, `/v1/messages`) and `relay.ruv.io` / `x.ruv.io` (federation)
  - cognitum.one and html.duckduckgo.com (the `benchmarks/gaia-*` modules)
- REA corroborates fetch sites in `commands/doctor.js`, `benchmarks/gaia-*`, `appliance/rvfa-distribution.js` (http `request`), plus app-graph endpoints in `mcp-tools/{agent-execute-core,managed-agent,seraphina,x-federation*,http-fetch}-tools.js`, `commands/{providers,neural,verify}.js`, `proxy/release.js` and `plugins/store/discovery.js`.
- Funnel/promo: consent domains (telemetry, advisor-tips, spinner-verbs, company-announcements, ...) default to `granted:false` (`funnel/consent.js`). The `SessionStart` helper does a one-time "first-run auto-enable" (`hook-handler.cjs:335-360`) and spawns detached `refresh-funnel` and `refresh-advisor`. Kill switches: `RUFLO_FUNNEL=0`, `RUFLO_NO_AUTO_ENABLE`. The auto-enable gating logic I did not trace [UNK]. Whether it can pass for opt-in is unverified.

**Env** [READ]: 208 distinct `process.env.*` keys. Top ones: CLAUDE_PROJECT_DIR, HOME, DEBUG, ANTHROPIC_API_KEY, many `CLAUDE_FLOW_ROUTER_*`, PINATA_*, PG* (postgres), OPENROUTER_API_KEY, IPFS_API_URL, CLAUDE_FLOW_MCP_TOOLS / _TRANSPORT, CLAUDE_FLOW_DB_PATH, RUFLO_*. REA config-source nodes also saw GOOGLE_AI_API_KEY, HF_TOKEN, PINATA_API_JWT, SSH_TTY/CONTAINER.

## 2. MCP surface as shipped

- 44 `*-tools.js` modules in `dist/src/mcp-tools/`, plus `x-federation-join/-channels` and `ruvector/coverage-tools.js` [READ].
- My regex count is **about 306 tool definitions**; this is not exact because a few modules build tools dynamically (browser, wasm). `cli.d.ts`/README claims should be cross-checked [UNK].
- Largest groups: wasm-agent (27), browser (23), metaharness (16), memory (15), claims/workflow (12 each), transfer (11).
- `policy_*` is filtered to evaluate/status only. The README-claimed "tool count" is therefore not pinned to a number I could confirm.
- Selection: `CLAUDE_FLOW_MCP_TOOLS` or `--tools` filters advertised tools by name, category or prefix. The default is "all", so a full tool list goes into context.
- Transport: stdio is inline in `bin/cli.js` (and a duplicate `bin/mcp-server.js`), newline-delimited JSON-RPC with protocol `2024-11-05`. Methods handled: initialize, tools/list, tools/call, resources/* (empty), ping. The `ws`/`express` deps indicate an http/ws transport in `mcp start -t http` [READ, not traced].
- Policy enforcement (`policy-enforcer`) is opt-in via `isPolicyEnforcementEnabled()`.

## 3. Hooks

- `.claude/helpers/` ships 42 files: `hook-handler.cjs` (1,280 lines), `intelligence.cjs` (1,364), `statusline.cjs` (1,300), `auto-memory-hook.mjs`, `learning-service.mjs`, `metrics-db.mjs`, `router.cjs`, `session.cjs`, `memory.cjs`, about 25 shell helpers, and a signed `helpers.manifest.json` (Ed25519) [READ].
- `init/executor.js` writes helpers and `settings.json`, using `helpers-generator`, `helper-companions` and `helper-refresh`.
- The package's own `.claude/settings.json` registers 8 hook entries [READ]:

| Event | Matcher | Command | Timeout |
|---|---|---|---|
| PreToolUse | Bash | `hook-handler pre-bash` | 5s |
| PostToolUse | Write\|Edit\|MultiEdit | `post-edit` | 10s |
| UserPromptSubmit | - | `route` | 10s |
| SessionStart | startup\|resume | `session-restore` | 15s |
| SessionStart | startup\|resume | `auto-memory-hook import` | 8s |
| SessionEnd | - | `session-end` | 10s |
| SubagentStart | - | `status` | 3s |
| Stop | - | `auto-memory-hook sync` | 10s |

- `init/settings-generator.js` also emits PreCompact and SubagentStop.
- Cost: **every hook call is a fresh Node process** (no daemon IPC). It requires `intelligence.cjs`, `session.cjs` etc. at startup.
- `route` fires on every prompt. `session-restore` does intelligence init, a risky-settings scan, and two detached CLI spawns (funnel and advisor refresh). The code comments themselves note `npm prefix -g` costs 70-80 ms, so a CLI spawn is expensive.

## 4. Storage and learning

- Paths [READ]:
  - `.swarm/memory.db` (default; `CLAUDE_FLOW_MEMORY_PATH` overrides; also probes `.claude/memory.db`, `data/memory.db`, `.claude-flow/memory.db`)
  - `.claude-flow/` (agents.json, daemon locks, config, state)
  - `.hive-mind`, `.claude/memory`, `~/.claude/helpers`
  - funnel `consent.json` under a user state dir
- REA storage nodes ("sqlite") cover `memory-initializer`, `memory-bridge`, `memory-peek`, `rabitq-index`, `memory-distill`, `sibling-store`, `commands/{doctor,embeddings}` and helpers `metrics-db.mjs` / `learning-service.mjs`.
- Installed tree on darwin-arm64 with `--ignore-scripts` [READ]:
  - Present: `sql.js` (WASM sqlite; the guaranteed fallback), `better-sqlite3` (source only, **no `build/`, so it will not load**), `hnswlib-node` (no `build/`, also unloadable), `agentdb`, `agentic-flow`, `ruvector`, `ruvector-core-darwin-arm64/ruvector.node`, `@ruvector/{attention,gnn,graph-node,router,rvf-node,sona,tiny-dancer}-darwin-arm64` (prebuilt NAPI, so they load), `onnxruntime-node`, `@huggingface/transformers` (475 MB), `sharp` + libvips, `@agntcy/slim-bindings-darwin-arm64` (53 MB), `@napi-rs/keyring`.
  - So a Mac install gets real ruvector/ONNX; the sql.js path covers a missing better-sqlite3.
- Embedding fallback chain [READ]: ONNX via ruvector, then agentdb/transformers, then **deterministic 128-dim hash embedding** (`memory-initializer.js:2306-2341`, "mock"). The code comments say the hash fallback yields semantically meaningless similarity. `embedding-policy.js` can make this an error rather than silent degradation.
- Is CLAUDE.md's "HNSW" the active index in the default sql.js path? [UNK]

## 5. Package weight

- Installed tree: **1.7 GB**, 418 top-level entries, 1,154 lockfile entries [READ]. Your "1017" count is a different count method; I don't have it [UNK].
- Heaviest: `@huggingface/transformers` 475 MB, `@claude-flow/*` 221 MB (cli 85 MB; memory 67; neural 66), `@opentelemetry` 199 MB, `onnxruntime-node` 98 MB, `@anthropic-ai/claude-agent-sdk` 71 MB, `onnxruntime-web` 69 MB, `agentic-flow` 62 MB, `agentdb` 60 MB, `@agntcy` 53 MB.
- Inside `cli/`: dist 11 MB (489 JS), plugins 2 MB, `.claude` 3.1 MB, own nested node_modules 67 MB.

## 6. Shipped vs repo/docs discrepancies

- **Inline stdio server hardcodes `VERSION = '3.0.0'`** (`cli/bin/cli.js:188`, `bin/mcp-server.js:37`). Clients see `serverInfo {name:'ruflo', version:'3.0.0'}` on a 3.65.0 package. The repo source has the same literal at `v3/@claude-flow/cli/bin/cli.js:188`, so there is no drift. It is a stale constant in both [READ].
- **Swarm tools are coordination records only.** `swarm_init` writes a JSON store entry (`persisted:true`, with a comment that the PID is not the swarm lifetime). `agent_spawn` returns `status:'registered'` and its note says real work needs `agent_execute` (a direct `fetch` to Anthropic Messages, requires `ANTHROPIC_API_KEY`) or Claude Code Task / `claude -p` [READ]. This matches the "recording-only" claim.
- `agent_execute` is the only in-package LLM executor; the tier router can mark `canSkipLLM` and point at the `hooks_codemod` tool.
- The hash-embedding fallback is shipped as described. Agentdb's "falling back to mock embeddings" console warnings are actively **suppressed** in `bin/cli.js` because `memory-bridge` swaps in the ruvector ONNX embedder [READ].
- Package `.claude/` carries a `proven-config.signed.*` champion and helper manifest. Startup auto-adopts/overwrites these, so the on-disk state changes without an explicit user command [READ].

## 7. Design lessons for Kaisa

Adopt:
1. A fast path for `--version`/`--help` and for MCP mode that does no side effects. In ruflo, MCP mode is the only clean startup.
2. One SQLite file per project, with a single path resolver and an env override. A Rust build gets rusqlite statically linked, which removes ruflo's better-sqlite3/sql.js/native-addon matrix.
3. A signed manifest for generated hook scripts, with tamper detection (ruflo's helper integrity gate), but applied only at `init`/`upgrade`.
4. `tools/list` filtering by category/prefix, and a small default set. Ruflo advertises about 300 tools by default, which costs context on every request.
5. An explicit ledger-vs-executor boundary. Ruflo's swarm tools honestly record only; Kaisa should state this in tool descriptions and return `status` fields that cannot be mistaken for execution.
6. A consent receipt per capability, default off, with an env kill switch.

Avoid:
7. Mutating state on every command: auto-refreshing `~/.claude/helpers`, adopting configs, spawning a daemon. These are surprising side effects of a read-only call.
8. One Node process per hook with heavy `require` chains and detached CLI re-spawns (`session-restore` spawns two). Kaisa hooks should be a single static binary with no runtime resolution (no `npm prefix -g` lookups).
9. Silent embedding degradation to a hash vector. Fail loudly, or tag results with the embedder in use.
10. Hardcoded protocol/server versions in duplicated entry points (`bin/cli.js` and `bin/mcp-server.js` both carry `3.0.0`); take the version from one source at build time.
11. Optional native dependencies resolved at install time (1.7 GB tree, two unloadable natives under `--ignore-scripts`). Ship one binary with an embedded index.
