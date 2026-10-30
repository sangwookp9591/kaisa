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

## Still open

| Agent | macOS | Linux | Windows |
|---|---|---|---|
| Claude Code | brokered ✓ (this spike) | not run | not run |
| Codex | observed (pending config search) | not run | not run |

Linux and Windows need the CLIs logged in on those machines; this development machine is macOS and CI
has no agent credentials. Running the same command there closes the row.
