# Brokered-mode spike (T16.0, architecture §19.1)

| | |
|---|---|
| Date | 2026-10-30 |
| Question | Can each agent CLI run with **all built-in tools disabled**, using only tools Kaisa serves over MCP, on macOS, Linux and Windows? |
| Status | **Partial**: macOS / Claude Code answered; Codex and the other two OSes still open |

## Claude Code 2.1.314 on macOS — brokered: yes

Command (a minimal stdio MCP server exposing one tool, `kaisa_echo`):

```
claude -p --model haiku --tools "" \
  --mcp-config mcp.json --strict-mcp-config \
  --allowedTools mcp__kaisa__kaisa_echo \
  --output-format stream-json --verbose "<task>"
```

Observed (from the `stream-json` events and the MCP server's own log):
- tools available to the session: exactly `["mcp__kaisa__kaisa_echo"]` — no built-in tools;
- the model called that tool, and the server logged the call;
- final result: `KAISA-ECHO brokered-ok`.

`--tools ""` removes every built-in tool, `--strict-mcp-config` ignores MCP servers from the user's
or the project's settings, and the permission flag limits what may run without a prompt. This is
the shape the Claude adapter (T16) uses.

## Codex CLI 0.165.0 — brokered: not shown yet

`codex exec --help` lists sandbox modes (`--sandbox read-only | workspace-write`), approval policies and
`-c key=value` overrides, but no flag that removes its built-in shell tool. Until a configuration that
does is found and tested, Codex is treated as **observed-only**: its own sandbox plus Kaisa's OS sandbox
(Seatbelt, Landlock, AppContainer) and egress broker, labelled "observed" (architecture §5.6).

### Config search, 2026-10-30 (macOS, codex-cli 0.165.0)

Two `codex exec` runs in an empty directory with `-s read-only`, asked to list their tools without
calling any:

1. `--disable shell_tool --disable unified_exec --disable view_image --disable browser_use
   --disable computer_use --disable apps -c include_apply_patch_tool=false -c web_search="disabled"`
2. the same plus `--disable code_mode_host --disable multi_agent --disable sleep_tool
   --disable tool_suggest -c 'mcp_servers={}'`

Both reported the same tools: `functions.exec`, `functions.wait`, `functions.request_user_input*`,
`collaboration.*` (spawn and message sub-agents) and an MCP server from the user's own setup
(`mcp__cua_repl`). The second run's extra flags changed nothing, so either they do not reach those
tools or the model's self-report does not reflect its real tool list (the user's Codex hooks also
loaded). Neither outcome shows a way to leave Codex with only Kaisa's tools.

Decision: Codex stays **observed-only**. A real test must check behaviour, not self-report: give it
Kaisa's MCP bridge, ask it to change a file, and see whether the change arrives through the broker
or around it. That needs an isolated `CODEX_HOME` (no user plugins or MCP servers) with credentials,
which is the user's call.

## Still open

| Agent | macOS | Linux | Windows |
|---|---|---|---|
| Claude Code | brokered ✓ (this spike) | not run | not run |
| Codex | observed (pending config search) | not run | not run |

Linux and Windows need the CLIs logged in on those machines; this development machine is macOS and CI
has no agent credentials. Running the same command there closes the row.

### Closing a row (T35)

On a machine with Claude Code logged in:

1. Run the brokered command above; the session's tools must be exactly `["mcp__kaisa__kaisa_echo"]`.
2. In a scratch git repository, run `kaisa --confine-claude "Add a NOTES.md that says hello"`.
   Kaisa prints `trial (T16.0): ...` with the hosts and session paths it allows.
3. The row closes if the run ends `verified` with the note on its branch. If Claude Code fails to start or
   cannot reach its provider, record the error and which path or host it needed; that is the
   allow-list finding T16.0 is waiting for.
