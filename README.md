# Kaisa

**Run coding agents safely in parallel. You only review what has already passed verification.**

Kaisa is a lightweight control plane for coding agents such as Claude Code and Codex. It gives each
agent its own isolated copy of your repository and keeps it inside a sandbox. It checks the result
against your tests and an independent reviewer, and asks you only when a human decision is actually
needed. When one provider runs out of quota, Kaisa hands the work to another one without losing the
plan or the work.

> **Status: design stage.** Nothing is implemented yet. Everything below describes the planned
> behaviour of v1. Performance and security properties will be published only after they have been
> measured and tested on every supported platform.

---

## Why Kaisa

| Without Kaisa | With Kaisa |
|---|---|
| You babysit agents, switch branches and clean up terminals | Every task runs in its own copy, and cleanup happens on its own |
| Agents ask for permission all the time, or you turn permissions off | Routine work never asks. Only real decisions do (publishing, new network access) |
| You review every attempt, including the broken ones | You see only candidates that passed your checks and an independent review |
| When a provider's quota runs out, work stops | Another provider takes over planning or coding, and the work so far is kept |
| Cost and progress are invisible until the end | Kaisa shows a live plan, current activity and cost at all times |

## How it works (planned)

There are three verbs.

```bash
kaisa "fix the flaky date test"      # start a task (add --n 3 for several candidates)
kaisa watch                          # see progress and anything that needs you
kaisa review                         # approve a verified candidate
```

You can also stay inside your agent:

```text
/kaisa add rate limiting to the API, 2 candidates
```

1. **Plan.** A planner proposes the steps. Small tasks skip this.
2. **Run.** Each step runs in its own copy of the repository, inside an OS sandbox. Network access goes through
   an allow-list.
3. **Verify.** Kaisa freezes the result, runs your project's checks on a clean checkout, detects weakened tests,
   and asks a different model to review it.
4. **Review.** You see the evidence and exactly what approving will do, for example "open PR #318 to main". Then
   you approve.
5. **Clean up.** Work is preserved first and cleaned up afterwards. Almost everything can be undone, and Kaisa
   tells you beforehand when something can't.

## Planned features

- **Real execution and supervision.** Kaisa owns the agent processes on macOS, Linux and Windows. It never starts a
  duplicate when the state of an agent is uncertain.
- **Containment.** Each agent gets a sandbox and an egress allow-list, and its effects go through a broker. Humans
  and agents hold separate authority: an agent can never approve its own work.
- **Verification.** Checks run on immutable snapshots. Kaisa detects tampered tests. Every candidate is reviewed by
  a different model, and the receipt is bound to your approval.
- **Provider failover.** When a provider runs out of quota, planning and coding move between providers, for
  example from Claude to GPT.
- **Cost control.** Budgets work per run, user and team, and an optional gateway can enforce them.
- **Teams and admins.** Policy packs are signed, and templates, approval delegation and an audit log are
  included.
- **Small and portable.** Kaisa ships as one core binary per platform. It needs no Node, Python or Electron
  runtime.
- **Languages.** The UI is available in Korean and English.

## Platforms

macOS, Linux and Windows are all first-class targets. Minimum OS versions for full protection are
listed in the [architecture](docs/architecture.md#57-guard-policy-sandbox-and-egress). On older systems
Kaisa runs in a clearly labelled partial-protection mode, and only after you approve it.

## Documentation

| Document | Contents |
|---|---|
| [User flows](docs/user-flows.md) | Every way people will use Kaisa: 35 flows, failure cases and usability targets |
| [Architecture](docs/architecture.md) | Engine design, security model, cross-platform matrix, milestones and decision record |
| [Prior-art research](docs/research/prior-art-rea.md) | Static analysis of Orca, LearnHarness and ruflo with REA |
| [Reviews](docs/reviews/) | Independent reviews of the design, with their resolutions |

## Building

Kaisa is written in Rust (stable, edition 2024) and follows the
[Google Rust style guide](https://google.github.io/styleguide/rustguide.html). The source code does not
exist yet. Build instructions will be added with milestone M0.

## Principles

- **Measured claims only.** No benchmark number appears here before CI has measured it.
- **Ease of use first.** If a flow is hard to use, the architecture changes. The flow does not.
- **Ideas, not code, from prior art.** Kaisa studies existing tools but does not copy their code.

## Name

The name was simply chosen as a name. Kaisa is an independent project with no connection to any game, character or trademark.

## License

To be decided. Kaisa is currently intended for internal company use.
