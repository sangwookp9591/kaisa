> Independent review of architecture v0.2 by Codex `gpt-6.1-sol` (effort high), run via Orca supervised worker on 2026-10-30. Read-only, no network; dependency versions were verified separately against crates.io by the author.

# Independent architecture review — Kaisa v0.2

## Overall verdict
Proceed with a feasibility prototype; do not approve the current security or recovery guarantees.
Rust and a single authority are sensible; process exit is narrower than resource ownership.
Cross-provider continuation is feasible only after quiescence and durable effect reconciliation.
All three OSes need executable containment/recovery proofs before feature expansion.
Defer knowledge, best-of-N, desktop and remote teams until that foundation passes.

Scope: architecture.md and all four supplied research reports, read-only; no installs/network. Findings are design analysis, not runtime validation. Exact dependency releases remain unverified; prior-art static analysis cannot establish portability.

## Blocker

- **B1 — Same-user privilege confusion (§6, §9).** Peer UID/SID authentication admits agents running as that same user. If it grants ordinary client powers, agents can call approvals, retention, policy changes or secret APIs. **Fix:** separate human/admin and agent endpoints/capabilities; deny agent access to control sockets, credentials and daemon storage, including alternate paths. On Windows validate pipe client tokens, reject remote clients, and prevent endpoint squatting.

- **B2 — Confinement is not equivalent across OSes (§5.7–§5.8, §8).** Landlock/seccomp do not alone supply domain egress control; network namespaces depend on host permissions. Seatbelt compatibility needs a supported-version proof. Restricted tokens plus Jobs do not equal AppContainer filesystem/network isolation. **Fix:** specify minimum OS/kernel/ABI and required capabilities; prove negative access tests and CLI authentication/package-manager compatibility per OS. Sandbox verification commands too; fail closed when guarantees differ.

- **B3 — Hooks cannot guarantee complete mediation (§1, §5.6–§5.7).** Interactive CLIs can bypass, omit or replace hooks; shell tokenization cannot contain interpreters, downloaded executables or indirect network actions. Headless mode disables built-ins but defines no replacement read/write/exec tools. **Fix:** define a capability-scoped execution broker and concrete tool contracts; treat hooks as advisory unless adapter coverage is proven. Enforce sensitive effects at broker/OS boundaries.

- **B4 — Failover races with old writers (§5.3, §5.5, §19.3).** Worker WIP capture precedes fencing and says nothing about stopping the old process or in-flight tools. Report fencing cannot prevent filesystem/external writes; coordinator fencing likewise misses admitted operations. **Fix:** revoke admission, drain/cancel effects, prove writer quiescence, snapshot, then grant successor ownership; reconcile uncertain external effects through Gates before continuation.

- **B5 — Restart breaks ownership certainty (§4, §5.1–§5.2).** pidfds and kqueue registrations do not survive daemon death; macOS kqueue is notification, not a durable process handle. Linux pidfd signals one process; macOS process groups permit escaping descendants. Windows requires explicit Job handle inheritance/breakaway/restart policy. **Fix:** persist launch intents/incarnations, prevent concurrent daemons, design containment and restart reconciliation per OS. Unproven death remains Unknown; never convert it into retry or destructive GC.

- **B6 — Reclamation can lose work or retain everything (§5.1, §5.5).** WIP commits omit ignored files, external outputs and potentially submodule work; automatic commits may preserve secrets. Archive success/durability, review pins and retain-versus-GC ordering are undefined. Every unpushed attempt branch can also leak indefinitely. **Fix:** quiesce writers, atomically claim cleanup, preserve a verified inventory including untracked/ignored policy, durably record archive receipts, then delete only covered resources. Pin review candidates; specify quotas and explicit archive expiry.

## Major

- **M1 — Lifecycle semantics are incomplete (§5.1–§5.2, §7).** Stalled is absent from the diagram; exit can mean authentication failure or cancellation, while CPU/output can renew leases forever. Exited bypasses verification in the expiry prose. **Fix:** separate execution, task outcome, verification and cleanup states; define bounded stall escalation, monotonic deadlines/reboot handling, and retain-versus-expiry transitions.

- **M2 — Receipts do not make external effects atomic (§5.2, §5.4).** Crash between spawn/merge/tool execution and receipt persistence remains ambiguous. **Fix:** transactionally commit intent, event and projection; add operation-scoped effect receipts/outbox recovery, payload-conflict rejection and replayable results. Specify request retention, SQLite durability/disk-full behavior, and Unknown resolution for stop/cleanup as well as start.

- **M3 — Cursor confirmation is still acknowledgment (§5.2, §7).** A monotonic number alone permits stale coordinators to skip unread events. Logged steer is not injected steer; PTY injection has an unavoidable ambiguous window. **Fix:** scope cursors to consumer/epoch, reject confirmation beyond delivered contiguous events, and atomically bind decisions to progress. Track queued/injected/accepted separately; expose uncertain delivery rather than promise exactly-once execution.

- **M4 — Packet omits continuation-critical state (§5.3, §7).** Missing: consistent snapshot sequence, pending effect IDs, cancellation, ownership/policy epochs, artifact identities, base revisions, unresolved tool calls and consumed events. Unjournaled reasoning cannot be reconstructed. **Fix:** make meaningful decisions transactional, version packets, bind their source/effect digests, enforce coordinator epoch on every mutation, and require successor reconciliation before dispatch.

- **M5 — Policy mmap lacks revocation semantics (§5.7).** A reader may authorize from an obsolete mapping; agent-controlled spool entries can forge observations. **Fix:** publish immutable authenticated snapshots atomically; validate schema/length/expiry/epoch, fail closed on corruption, and recheck live authority at effect execution. Define ask/resume tokens, spool authentication/bounds, path-handle TOCTOU protection and WASM fuel/memory/hostcall limits.

- **M6 — Verified work can change before merge (§5.8–§5.9).** Worktree checks and model independence do not establish artifact integrity or prevent malicious tests from forging success. **Fix:** verify an immutable source snapshot in isolation, bind logs/artifacts/environment and approvals to its digest, revalidate the merge base, and promote knowledge only from that receipt; treat repository instructions and extracted facts as untrusted.

- **M7 — Quota and budgets need admission control (§5.10).** A 429 may be transient throttling; stderr can be spoofed. Reactive billing plus downgrades cannot enforce hard concurrent budgets. **Fix:** classify adapter signals, add retry/backoff, cooldown and handback hysteresis, reserve cost before admission, reconcile usage afterward, and separately cap unpriced/subscription work. Specify capability requirements rather than an undefined cross-provider “weaker tier.”

- **M8 — Native dependency closure is unproven (§14–§15).** Bundled rusqlite is reasonable; rmcp 3.8 needs an actual resolved build and protocol interoperability evidence. usearch adds native C++; fastembed adds ONNX/runtime/model assets; wasmtime adds substantial runtime cost. **Fix:** record exact lockfile/checksums/MSRV/features/licenses and three-OS linking results; disable embeddings/WASM by default until footprint and offline-asset distribution are measured.

- **M9 — PTY and workspace contracts need conformance tests (§5.1, §5.5, §14).** Release age alone does not justify replacing portable-pty. ConPTY pipe draining/close ordering, resize and Unicode differ; 64 KiB/1 s truncates evidence. gix reads may disagree with git on repository edge cases; shared git metadata and copied caches weaken isolation. **Fix:** compare mature wrappers; test terminal replay, job exit/drain, submodules, symlinks, long paths and clone fallbacks; label truncation and restrict shared metadata access.

- **M10 — Credential leases need enforcement (§4, §8–§9).** A short-lived daemon lease cannot shorten a copied provider credential's lifetime; native CLIs may read their own auth stores. A Windows service can run under another identity/session. **Fix:** define authentication brokering, sandbox-visible credential storage, redaction and revocation; run per-user agents under the intended interactive identity, and specify headless Linux key unlock/failure behavior.

## Minor

- **m1 — Audit overclaim (§5.4, §9).** A writable hash chain can be recomputed or truncated. **Fix:** externally anchor signed checkpoints; disclose same-user compromise limits and redact secrets in PTY/spool/audit retention.
- **m2 — Contradictions (§2, §4, §9, §12).** Read-only commands autostart a daemon; loopback MCP contradicts no listeners; desktop has another executable. **Fix:** distinguish inspection/startup, agent listeners, single deployable core and GUI packaging. “Static” must allow OS frameworks/native runtime assets.
- **m3 — M0 ships cleanup before safety (§16–§17).** Fencing/Gates/sandbox arrive at M1; verification at M2. **Fix:** move preservation, Unknown handling and minimum confinement into the first vertical slice; defer optional engines. Add staffing/time estimates and measured go/no-go gates rather than implied schedule certainty.

- **m4 — Terminal model is not a complete UI (§5.1, §12, §14).** alacritty_terminal 0.26 is a plausible parser/state core, but does not supply KRPC snapshot serialization or a ratatui terminal widget. **Fix:** specify versioned screen snapshots, cell/Unicode compatibility, safe escape handling and bounded scrollback; benchmark screen memory rather than counting only the ring buffer.

## Question

- **Q1 (§12, §19.7):** Can pinned GPUI support the required Windows/Linux terminal, accessibility and packaging matrix? **Fix:** define spike exit criteria; evaluate Slint terminal rendering/IME/accessibility costs and verify redistribution licensing before choosing either.
- **Q2 (§5.4, §6, §14):** What supports “verified” versions and forward-only upgrade compatibility? **Fix:** attach reproducible dependency evidence and schema/protocol upgrade-recovery tests; projections being rebuildable does not eliminate event-schema migration.
