# Windows confinement: AppContainer (T15.5 / T15.6c)

Status: **unverified notes**. Gathered from a research agent working from memory, with no web
verification; the agent stopped before finishing. Every claim below is a hypothesis for the
spike (T15.5s), not a design fact.

## Notes (confidence in brackets)

- Creating an AppContainer profile and its SID needs no administrator rights (`userenv`
  profile API; profile kept per user). [high]
- A process starts inside the container through the security-capabilities attribute of the
  extended startup info, added to the attribute list Kaisa already builds for ConPTY. [high]
- Access requires both the normal DACL and an ACE for the container SID (or ALL APPLICATION
  PACKAGES). System directories carry the latter; the user's profile does not, so a contained
  agent cannot even **read** its worktree, toolchains (`.cargo`, `.rustup`, Node) or its own
  config until Kaisa grants them. [high]
- A user can add ACEs for the container SID to objects they own, without administrator
  rights. [high]
- Git worktree metadata lives in the main repository's `.git/worktrees/<name>` and shared
  objects/refs, so committing from inside needs write grants outside the write set. [high]
  Kaisa preserves work itself (T13), so the agent does not need to commit: no grant there.
- Inheritable ACEs on large trees (`node_modules`, `target`) are slow to apply; grant once
  on a root and let new entries inherit. ACEs persist on files: use one fixed profile per
  installation and remove grants at reclamation. [medium]
- **Loopback to a broker on 127.0.0.1 is blocked for AppContainer processes by default**;
  the documented exemption is an administrator operation, which conflicts with FR-01. [high]
  Consequence: the brokered design must not depend on a TCP loopback broker on Windows. A
  local IPC endpoint (named pipe) with an ACE for the container SID is the candidate, to be
  confirmed by the spike.

## Spike checklist (T15.5s, Windows CI runner, non-admin)

1. Create a profile, start `cmd /c` inside it, confirm it runs. Record whether ConPTY works.
2. Without grants: reading a file in the worktree is denied.
3. Grant read+execute on the worktree and write on the write set: write inside succeeds,
   write outside is denied.
4. A named pipe served by the keeper with an ACE for the container SID is reachable; a TCP
   connection to 127.0.0.1 is refused.
5. Can `node`/`claude` start inside the container with only the grants Kaisa adds? If not,
   the run is labelled **reduced** (ADR-0029), never silently unconfined.
6. Grants are removed at reclamation; nothing is left on the files.
