# Kaisa

**Run coding agents safely. You only see work that has already passed verification.**

Kaisa is a lightweight control plane for coding agents such as Claude Code and Codex. It gives each
attempt its own copy of your repository and keeps the agent inside an OS sandbox. It checks the result
against your tests and an independent reviewer, sends it back for revision when the review finds a
blocking problem, and asks you only when a human decision is actually needed.

> **Status: M0 and M1 run end to end with real Claude Code and Codex on macOS; some M0/M1 tasks are
> built but not yet connected** (marked ◐ in [`docs/tasks/`](docs/tasks/)). CI runs on macOS, Linux
> and Windows for every commit; real-agent runs have been checked on macOS only. Some protections are still *reduced* on some platforms;
> the [protection status](#protection-status) table says exactly which. Performance and security
> properties are published only after they have been measured and tested on every supported
> platform. Task-level progress lives in [`docs/tasks/`](docs/tasks/).

---

## Why Kaisa

| Without Kaisa | With Kaisa |
|---|---|
| You babysit agents, switch branches and clean up terminals | Every attempt runs in its own copy, and cleanup happens on its own |
| Agents ask for permission all the time, or you turn permissions off | Agent effects go through a broker that enforces a write set; real decisions come to you |
| You review every attempt, including the broken ones | You see only candidates that passed your checks and an independent review |
| A single agent loses track of a long task | Planned tasks, a reviewer, bounded revisions and project memory carry the context |
| Cost and progress are invisible until the end | `kaisa watch` shows every attempt with its state, tokens and cost |

## What works today

Checked with logged-in Claude Code and Codex CLIs on macOS: a verified fix, a second session that
recalls the first one's lesson, Codex developing with Claude reviewing, a goal that weakens tests
(stops for a person), a change to what the checks run (stops for a person) and `--confine-claude`.
A delivered result stays on its `kaisa/att_…` branch; the last line of the output says how to
bring it in.

```bash
kaisa "fix the flaky date test"           # run a goal, verified (Claude Code by default)
kaisa --agent codex "fix the flaky test"  # Codex develops (observed), Claude Code reviews
kaisa watch                               # attempts, state, tokens and cost
kaisa agent add local --endpoint http://127.0.0.1:11434/v1 --model qwen=T0
kaisa agent list                          # personal agents and model servers
kaisa policy show                         # your personal policy
kaisa policy set revision_limit 3         # change it (validated before it is saved)
```

A goal ends with one of these results, which scripts and CI can rely on:

| Exit code | Meaning |
|---|---|
| `0` | verified: the candidate passed fresh checks and the review |
| `3` | waiting for you: a person must decide, and Kaisa says why |
| `4` | not delivered: no fresh passing evidence, or revisions ran out |
| `64` | usage error |

### How a verified run works

1. **Recall.** The context pack carries the project profile and up to five relevant lessons from
   earlier verified runs *of this repository*; lessons never cross repositories. A second session
   needs no re-explanation.
2. **Develop.** The developer agent works in its own worktree; its file writes go through Kaisa's
   broker, which enforces the write set. Before it starts, Kaisa compares what the OS sandbox really
   guarantees (write confinement, TCP, UDP, Kaisa state read isolation, descendants) with what is needed.
   A shortfall stops the run with exit `3` and names what is missing, unless a documented default
   policy covers it (the output names it, for example `claude-reduced-until-T16.0`) or you accept it for a limited time with
   `--accept-reduced <write,tcp,udp,state-read,descendants> --accept-until <unix time in ms>`.
   Decisions written for the old four capability codes no longer cover unconfined runs; they must
   also accept `state-read`.
3. **Freeze.** The result becomes an immutable candidate (commit and tree digest).
4. **Check.** Your project's test command runs on a clean checkout of the candidate, in the sandbox,
   with no agent involved. A candidate that changes its own check definitions needs a person. If a check
   changes the candidate's source files, Kaisa refuses all evidence and the run is not delivered.
5. **First pass (optional).** If you registered a T0 local model, it reviews first. Its blocking
   findings send the candidate back without spending the full review; its approval is only advisory.
6. **Review.** A reviewer reads the frozen candidate and answers with a structured verdict. Kaisa
   reads the verdict only from one final `kaisa-verdict` region of the reviewer's answer; a missing,
   repeated, quoted or malformed verdict never counts as approval.
7. **Gate.** Only a candidate with fresh passing evidence and an approving verdict is delivered.
   Blocking findings go back to the developer as a revision, up to the policy's revision limit.
8. **Learn.** A delivered run records a lesson, verified by digest, for later runs on the same
   repository only.

Before any agent starts, Kaisa scans the repository's agent settings and stops if they would widen
an agent's authority; the frozen candidate's own settings are scanned again before a reviewer or a
revision starts in it. Every run records the policy snapshot it ran under. A run interrupted by a
crash is found and tidied on the next `kaisa` start; an attempt in an unknown state is never retried
without proof.

### Protection status

| | macOS | Linux | Windows |
|---|---|---|---|
| Scripted and brokered agents confined | ✓ Seatbelt | ✓ Landlock | ✓ AppContainer |
| Trusted checks confined | ✓ | ✓ | reduced (labelled) |
| Claude Code confined | reduced until T16.0 (default policy `claude-reduced-until-T16.0`) | reduced until T16.0 (same policy) | reduced until T16.0 (same policy) |
| Codex | observed (writes checked against the write set afterwards; default policy `observed-checked-afterwards`) | observed | observed |
| Local model (first-pass review) | ✓ confined, loopback only | ✓ confined, loopback only | reduced (AppContainer has no loopback; default policy `windows-local-model-loopback`) |

*Reduced* runs are always labelled in the output, with the name of the policy or Decision that allowed
them. Any other shortfall stops the run until you accept it with `--accept-reduced` and `--accept-until`. `kaisa --confine-claude "<goal>"` tries a confined
Claude Code run; the steps to verify it on a machine with a logged-in CLI are in
[`docs/research/brokered-mode-spike.md`](docs/research/brokered-mode-spike.md#closing-a-row-t35).

## Planned

- `kaisa review` to approve a verified candidate and see exactly what approving will do.
- Several candidates per goal (`--n 3`), provider failover (for example Claude to GPT when a quota runs
  out), budgets per run, user and team.
- A full-screen TUI and `kaisa console` in the browser; `/kaisa` from inside your agent.
- Team policy packs, approval delegation and an audit log.
- Korean and English UI.

## Platforms

macOS, Linux and Windows are all first-class targets and release blockers. Minimum OS versions for
full protection are listed in the
[architecture](docs/architecture.md#57-guard-policy-sandbox-and-egress).

## Documentation

| Document | Contents |
|---|---|
| [PRD](docs/prd.md) | Problem, users and requirements |
| [User flows](docs/user-flows.md) | Every way people use Kaisa: flows, failure cases and usability targets |
| [Architecture](docs/architecture.md) | Engine design, security model, cross-platform matrix, milestones and decisions |
| [Tasks](docs/tasks/) | Milestone breakdowns with the test behind each task: [M0](docs/tasks/M0.md), [M1](docs/tasks/M1.md) |
| [Test conventions](docs/testing.md) | How tests are named and written, and the checks before every commit |
| [Research](docs/research/) | Prior-art analysis of Orca, LearnHarness and ruflo, and platform spikes |
| [Reviews](docs/reviews/) | Independent reviews of the design, with their resolutions |

## Building

Kaisa is written in Rust (edition 2024; the toolchain is pinned in `rust-toolchain.toml`) and follows
the [Google Rust style guide](https://google.github.io/styleguide/rustguide.html). It ships as one
binary, `kaisa`, with no Node, Python or Electron runtime.

```bash
cargo build                  # build everything
scripts/check.sh             # exactly what CI runs: rustfmt (stable and nightly), clippy, tests,
                             # and Windows/Linux cross-checks
scripts/check.sh --fix       # format first, then check
```

CI runs the same checks on macOS, Linux and Windows, plus `cargo-deny` for licences and advisories.

## Principles

- **Measured claims only.** No benchmark number appears here before CI has measured it.
- **Ease of use first.** If a flow is hard to use, the architecture changes. The flow does not.
- **Honest labels.** Anything that runs with less protection than designed says so in its output.
- **Ideas, not code, from prior art.** Kaisa studies existing tools but does not copy their code.

## Name

The name was simply chosen as a name. Kaisa is an independent project with no connection to any game,
character or trademark.

## License

Kaisa is released under the [MIT License](LICENSE).
