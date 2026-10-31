# Kaisa User Flows

| | |
|---|---|
| Status | Draft v0.3 |
| Date | 2026-10-30 |
| Companion | [`architecture.md`](architecture.md) v0.6 |
| Review | [`reviews/2026-10-30-user-flows-v0.1-codex.md`](reviews/2026-10-30-user-flows-v0.1-codex.md), applied in this version |

> If Kaisa is hard to use, nobody will use it, however good the engine is. This document is the
> contract the engine must serve. When a flow and the architecture disagree, fix the architecture.

**Changes in v0.3** (team intelligence, architecture v0.6) — more capability, no more commands to learn.
- Project memory is on by default and needs no setup; `kaisa memory` shows and corrects it (F21).
- Every run says in one line what context it started from and which model tier it was routed to (F04).
- Blocking review findings are fixed by automatic revision before you see the result (F16).
- Merge conflicts go to the agent that owns the files first, and to you only if that fails (F07).
- `kaisa console` opens the same view in a browser (F10). New flows: F36 large mission, F37 light work on a local model.

**Changes in v0.2** (from the usability review)
- F19m no longer offers a duplicate-risking "start fresh".
- *Answers* (information) are now separated from *Decisions* (human-only Gates).
- Reduced protection needs explicit, scoped human authorization. (Current exception: limits of the OS itself, which no person can lift, are admitted as a labelled `platform-limit`; see architecture §5.7.)
- Approval previews its exact effect.
- Pause, billing and restart states are reported honestly.
- Undo states its limits before acting.
- Command names have been made consistent:
  - `handback` replaces the overloaded `release`.
  - `--agents` is the only agent flag.
  - `--template` replaces `--team`.
- Admin journeys now have concrete commands.
- Five missing scenarios were added (F31–F35).
- A standard blocker card and a "Needs you" lane were added in `watch`.

---

## 1. Why the prior art is easy to use, and what Kaisa copies

| Tool | What makes it easy | Kaisa copies |
|---|---|---|
| **ruflo** | One command (`npx ruflo init`). After that, *"just use Claude Code normally"*: hooks and memory work in the background. Plugins install with `/plugin install`. | **Stay where the user already is.** Use `/kaisa` inside Claude Code and Codex. `kaisa init` takes one step. |
| **Orca** | Desktop app. Every feature gets its own worktree, so there is no stashing. One prompt fans out to several agents, and you compare and merge. Issues and PRs are linked. There is a mobile monitor. | **One prompt, several candidates, pick one.** Isolation stays invisible. Work is linked to issues. Notifications come to you. |
| **LearnHarness** | Double-click the app. BYOK model *slots* (strong / cheap / vision). A guided flow and a visible plan tree. A live cost meter. | **Simple model choice, a visible plan, live cost, a guided first run.** |

**What Kaisa avoids:**
- Orca's supervision protocol (`check`, `ack`, `release`) and its finished terminals that pile up.
- ruflo's roughly 300 tools, the routing banner on every prompt, and side effects on read-only commands.
- LearnHarness's single-user, desktop-only design.

---

## 2. UX principles

| # | Principle | Concretely |
|---|---|---|
| U1 | **Three verbs.** Everything else is reachable from them. | `kaisa "<goal>"`, `kaisa watch`, `kaisa review`. Typing `kaisa` alone opens Home. Every exceptional action (take over, steer, pause, undo, keep) is a key in `watch` or `review`, so users never need to remember a fourth command. |
| U2 | **The first useful result comes before any choice.** | Onboarding detects tools and explains the defaults, then asks for **one goal**. Advanced choices come later. |
| U3 | **Stay where the user is.** | Inside Claude Code / Codex, in the terminal, or in the issue tracker. The desktop and mobile apps come later. |
| U4 | **Kaisa comes to you.** | Notifications, plus a persistent **Needs you** lane in `watch`, so nothing depends on a notification being seen. |
| U5 | **Plain language, no internals.** | No epoch, generation, lease or tier in default output. |
| U6 | **Every block is a standard card.** | *What happened · why · who must act · safe default · options · back to watch*. No block ships without a fix option, and CI enforces this (architecture §5.11). |
| U7 | **Nothing is lost, and most things can be undone.** | Kaisa says **before** an action whether it can be undone, and for how long. |
| U8 | **Safe by default, without nagging.** | Routine edits and test runs never ask. Kaisa asks only at permission boundaries. |
| U9 | **Answers and decisions are different.** | An *answer* gives information to an agent, and a requester (e.g. your Claude session) may give one. A *decision* is a Gate: approval, policy, budget, protection or publication. Only an authenticated human makes decisions. |
| U10 | **Honest states.** | Kaisa reports what it observed, such as "agent frozen", rather than what it hopes, such as "cost stopped". |
| U11 | **Progressive disclosure.** | Beginners see three verbs. Power users get flags. Admins get `kaisa admin`. |
| U12 | **Korean / English, accessible.** | `ko` / `en`. `--plain` gives screen-reader-friendly text output with no box drawing. The README is in English. |

---

## 3. Personas

| Persona | Goal | Main surface |
|---|---|---|
| **P1 Developer** (most users) | Get a task done faster without babysitting agents | Inside Claude Code/Codex, terminal, TUI |
| **P2 Reviewer / Tech lead** | Approve only good, verified changes; see the team's work | `kaisa review`, issue tracker |
| **P3 Admin** (platform / security) | Safe defaults, budgets, allowed agents, audit | `kaisa admin`, policy pack repo |
| **P4 Occasional user** (PM, designer, new hire) | A small change or an answer without learning git | Desktop (M3), issue label |
| **P5 Automation** (CI, bots) | Unattended runs under policy | `kaisa run --headless` |

---

## 4. Entry points and authority

| Entry | How | Authority | Available |
|---|---|---|---|
| Terminal / TUI | `kaisa …`, `kaisa` | Human | M0 / M1 |
| Inside Claude Code / Codex | `/kaisa <goal>`, or just ask | **Requester** | M1 |
| Issue tracker | GitHub label `kaisa`, or `kaisa run --issue 123` | Request on behalf of the labeler (allow-list) | M1 GitHub, M3 Linear |
| CI | `kaisa run --headless --json` | Requester under the CI policy | M2 |
| Notification action | Click or tap | Human, with a presence check when policy requires it | M1 |
| Desktop / mobile | GUI | Human | M3 / later |

A **requester** can start runs within budget, watch them, read results, give *answers* and cancel its own
runs. It can **never** make a *decision*. The decisions are:
- approve or publish;
- allow egress or files;
- change the budget;
- accept reduced protection;
- change policy.

When a decision is needed, the requester session shows **"Waiting for you"** with a link that opens
`kaisa review` or the decision card (architecture §5.7).

---

## 5. Journey map (P1)

```
install ─► first run: detect + explain defaults ─► "try: kaisa \"<your task>\"" ─► one goal
   ─► (optional plan approve) ─► go do something else
   ◄── notification / Needs-you lane ──► answer a question · make a decision (one key + presence)
   ─► "2 verified candidates ready" ─► review: evidence + exact effect preview ─► approve
   ─► PR / merge ─► cleanup countdown visible ─► next time, Kaisa remembers what worked
```

---

## 6. Flows

Each flow lists the trigger, what the user sees and does, what Kaisa does, and the variants and edge cases.

### A. Getting started

#### F01 Install
- **Install commands**

  | OS | Command |
  |---|---|
  | macOS | `brew install kaisa` |
  | Windows | `winget install Kaisa` |
  | Linux | the distro package, or the install script from the internal portal |

  The exact internal URL and package names are fixed at release *(tbd)*.
- **Non-admin install.** On every OS a per-user install goes to `~/.local/bin` or `%LOCALAPPDATA%\Programs\kaisa`.
  - The installer adds that directory to the user PATH and prints *"Open a new terminal, then run `kaisa`"*.
  - On Windows it writes the user-PATH registry entry and broadcasts the change, so a new PowerShell window sees it.
- **What Kaisa installs.** One signed binary. Nothing else is downloaded, and no daemon starts.
- **WSL.** Native Windows is recommended. Inside WSL, Kaisa runs as a Linux install and says so.
- **Proxies and certificates.**
  - Proxy settings come from the OS.
  - A corporate root certificate is read from the OS trust store.
  - If a TLS check fails, see F34.

#### F02 First run (onboarding)
- **Trigger:** `kaisa` run for the first time.
- **User sees:**
  ```
  Welcome to Kaisa.
  ✓ Agents:   Claude Code (logged in) · Codex (logged in)
  ✓ Repo:     ~/work/payments  (clean, branch main)
  ✓ Protection: full on this machine
  ✓ Company policy: payments-team pack v12
  How Kaisa will work for you:
    • Plans with Claude, writes code with Codex and Claude, in separate copies of your repo
    • Uses your existing Claude/Codex subscriptions — no extra API keys needed
    • Asks you only before publishing or anything unusual; daily budget $20 (company default)
  Try it now:   kaisa "describe a small task, e.g. fix the flaky date test"
  [Enter] Type a task   [C] Change defaults later   [?] Learn more
  ```
- **Kaisa:**
  - runs `doctor`, detects agents and logins, and fetches the policy pack;
  - stores only the confirmed configuration;
  - asks for a **goal first** (U2).
- **Variants:**
  - **No agent installed.** Kaisa shows OS-specific install commands that can be copied, then re-checks. It never installs anything silently.
  - **Agent not logged in.** Kaisa shows the agent's own login command, then re-checks after you confirm.
  - **Reduced protection** (old OS, no cgroup delegation, restricted Windows edition). Kaisa shows a decision card:
    ```
    Partial protection on this machine
      Missing: network filtering by domain (needs Linux kernel ≥ 6.7)
      Effect:  agents can reach any host your user can; Kaisa will ask before each new host
      Scope:   this machine · until the kernel is upgraded or 30 days, whichever first
    [A]ccept for this machine (needs your password)   [N]o — run only read-only tasks   [?] Details
    ```
    After acceptance, a **persistent banner** in every view reads "Partial protection", and expiry triggers a new decision.
  - **Repository problems:** see F31.
  - **Policy unreachable.** Kaisa uses the cached pack and labels it as cached. With no cache, it falls back to the built-in strict defaults, labelled "strict defaults".

#### F03 Set up a project (`kaisa init`)
- **User sees:** a preview, then a confirmation.
  ```
  Kaisa will add (existing settings are kept; a backup is saved):
    • .claude/settings.json  — /kaisa command + advisory hooks
    • .codex/config.toml     — Kaisa MCP bridge
    • kaisa.toml             — checks: `pnpm test`, `pnpm lint` (detected)
  Needs a fix before agents can run here:
    ✗ enableAllProjectMcpServers = true (auto-approves any MCP server in this repo)
    ✗ enabledMcpjsonServers = ["…"] (names project MCP servers as pre-approved)
      [F] Turn it off for Kaisa runs only   [K] Keep it — agents won't run in this repo
  [Enter] Apply   [D] Show diff   [Q] Cancel
  ```
- **Kaisa:**
  - Detects the checks.
  - Scans for risky settings. The "keep" option says plainly that agents will not run in this repo.
  - Changes nothing without confirmation.
  - Backs up the existing files.
- **Optional.** `kaisa init` is never required: the first run derives the project profile itself
  (languages, build/test/lint commands, layout) and confirms the commands by running them. `init`
  only writes agent integration files and lets you review the profile:
  ```
  Kaisa learned this project:  Rust workspace · 10 crates
    build  cargo build          ✓ ran
    test   cargo test           ✓ ran (148 tests)
    lint   cargo clippy -D warnings  ✓ ran
  [Enter] Looks right   [E] Edit   (stored in kaisa.toml [profile]; Kaisa keeps it up to date)
  ```
- **Variants:**
  - **Monorepo:** pick the packages, and each gets its own checks.
  - **No tests:** "Kaisa can't verify without checks." [P] Propose checks (an agent drafts them, and you approve) · [S] Skip — results are then *unverified*: they cannot be approved normally, only through a recorded override, and they are labelled so everywhere.

### B. Doing work

#### F04 Quick task from the terminal (the core flow)
- **Trigger:** `kaisa "fix the failing login test"`.
- **Today (M1):** quote the goal, particularly if it starts with a reserved command; ordinary unquoted goal words are also joined with spaces. A first word that is a reserved command (`review`, `run`, `gc`, `wrap`, …) exits `2` with the quoted form to use; an unknown option exits `64`; `--` ends the options. Run from a subdirectory, Kaisa uses the enclosing repository and prints `using the git repository at <root>`. Exit codes: `0` verified, `3` waiting for you, `4` not delivered (including an agent that could not be started), `1` infrastructure or configuration failure (for example the state directory cannot be located, or an old policy disables a person gate). `0` means verified on the verified path; a run with `--agent-program` and no `--reviewer-arg` has no checks or review and can still exit `0`.
- **User sees:**
  ```
  ▶ Run 41  fix the failing login test      budget $3 · est. 4–8 min
    1 candidate in a separate copy of your repo · you'll be notified
    Next: kaisa review 41   (or keep working; this terminal is free)
  ```
- **Kaisa:** picks the cheapest way that works. A script or codemod is used when it can do the job.
  One status line says what it did, so nothing about routing or memory is hidden:
  ```
    context: project profile · 2 lessons (login tests) · 6 related files    model: light (small, checkable change)
  ```
  Nothing to configure: the profile and lessons come from earlier runs on this repository. Grade names
  such as T1 appear only with `--verbose` or `--json` (U5).
- **Variants:** `--n 3`, `--agents claude` (one or more; this is the only agent flag), `--plan`, `--watch`, `--budget 5`,
  `--issue 123`.
- **Question-shaped goals.** A goal like "why …" or "how does …" is routed to F09 automatically. Kaisa says so and shows the cost ceiling.

#### F05 From inside Claude Code / Codex
- **Trigger:** `/kaisa add rate limiting to the API, 2 candidates`, or just asking.
- **User sees, in the agent session:**
  - "Started Kaisa run 42 (2 candidates)."
  - A status line `kaisa: 42 running · $0.80`.
  - When a decision is needed: **"Waiting for you → open review 42"** as a clickable link.
- **Allowed from here:** status, results, answers to agent questions and cancel.
- **Not allowed:** approve or publish. The agent replies "This needs you" and gives the link.
- **Today (M1):** `claude mcp add kaisa -- kaisa mcp serve` registers a stdio MCP server with three tools. `kaisa_start_goal {goal, agent?, repository?}` starts `kaisa "<goal>"` as a detached background process (it survives the session; several can run at once) and returns a handle; `kaisa_status {handle?}` reports running, verified (branch), waiting for you (reason, with "This needs you" and how to continue in a terminal) or not delivered (reason), with tokens and cost from the ledger once the run ends; `kaisa_result {handle}` returns the final lines and the exact `git merge --ff-only …` command, which it never runs. `/kaisa <goal>` is a project slash command (`.claude/commands/kaisa.md`, planned for `kaisa init`). One candidate only (no `--n`), no run numbers or status line, no clickable review link (`kaisa review` is planned: the agent points to a terminal), no cancel and no answering agent questions, and no Codex-side integration. Approve and publish do not exist as tools.

#### F06 Several candidates, compare and pick
- **Trigger:** `kaisa "…" --n 3 --agents claude,codex`.
- **User sees:**
  ```
  Run 43  "implement CSV export"      acceptance: export matches spec examples; all tests pass
  ┌ A  Codex   verified ✓ tests ✓ lint · review: 1 minor · +120 −8 · $0.62   ★ recommended: passes all, smallest change
  └ B  Claude  verified ✓ tests ✓ lint · review: 1 minor (2 major fixed by revision) · +310 −40 · $1.90
    + 1 not eligible: tests failed after 2 revisions — [Show]
  [Enter] diff  [Tab] side by side  [Space] choose  [R] request changes  [X] reject all
  ```
- **Variants:**
  - **Choose one.** This goes to the approval preview (F16).
  - **"Combine A and B."** A follow-up attempt starts with both diffs.

#### F07 Bigger feature with a template
- **Trigger:** `kaisa "add OAuth login" --template feature`. Kaisa also *suggests* a template when the goal looks large.
- **User sees:** a plan to approve.
  ```
  Plan for run 44 — template "feature"        budget $25 · est. 40–70 min
   1 research   current auth code        read-only
   2 design     OAuth flow + API         read-only
   3 build      backend   · frontend     separate copies (in parallel)
   4 test       integration tests
   5 review     security + code review   read-only · always strongest model
   Integration: step 3 results are merged by Kaisa before step 4; a conflict goes to the agent that
   owns those files first, and comes to you only if it cannot resolve it.
  [Enter] Go   [E] Edit   [Q] Quit
  ```
- **Editing the plan.** Edits are validated as you make them, for example: "step 4 depends on step 3 — can't remove 3 while 4 needs it".
  Invalid plans cannot be started.
- **Templates.** `feature`, `bugfix`, `refactor`, `security-audit`, `docs`, `migration`, plus company templates (F25).
  "Team" means *people* (`kaisa team`, F20). Pipelines are always called *templates*.

#### F08 Issue-driven work
- **Trigger:** an allowed user adds the `kaisa` label to an issue, or someone runs `kaisa run --issue 123`.
- **User sees, on the issue:**
  1. "Kaisa run 45 started for @alice".
  2. Progress updates.
  3. "2 candidates ready. Reviewer: @bob (CODEOWNERS)", with a link that opens `kaisa review 45`. Login uses the company SSO through the CLI device flow.
- **Kaisa:**
  - Issue text is untrusted data.
  - A label from a user who is not allowed waits for a decision from a maintainer.
  - A duplicate label on an issue that already has an active run adds a comment linking to that run and does not start a second one.
- **Variants:**
  - **Bulk:** `kaisa run --issues "label:good-first"` queues runs under a concurrency cap.
  - **Linear:** M3.

#### F09 A question, no code change
- `kaisa ask "why is checkout slow on Windows?"`, or a question-shaped goal (F04).
- The answer cites files, logs and verified knowledge. Kaisa checks that every citation exists and has
  an independent reviewer confirm that the citations support the claims; unsupported claims are removed
  or marked. Before it starts, Kaisa shows the cost ceiling. Nothing is written to the repo.

### C. While work is running

#### F10 Watch
- **Layout.** `kaisa watch`, or Home, shows a **Needs you** lane at the top: decisions, questions and blockers, oldest first.
  Below it come the run cards, each with a plan tree, live cost and what each agent is doing right now.
- **Filters:** `[n]` needs you · `[m]` mine · `[f]` failed · `[/]` search.
- **Today (M1):** `kaisa watch` prints a table of attempts, state, tokens and cost and exits. It is read-only: it does not append events, migrate the database or rebuild projections, so it is safe while a run is going. A closed ledger is read without file changes; reading a live WAL database can create or update SQLite shared-memory sidecars.
- **Actions** are keys on the selected card: `[t]` take over · `[s]` steer · `[p]` pause · `[u]` undo · `[k]` keep.
- **Accessibility.** `--plain` prints the same information as a text stream that screen readers can follow.
- **Browser.** `kaisa console` opens the same view in your browser on this machine only (nothing
  listens until you run it). It adds a team grid that stays readable with 100+ agents, cost per
  request and model, and each Task's evidence and revisions. Close the tab or press Ctrl-C to stop it.

#### F11 Take over and hand back
- **Take over.** In `watch`, `[t]` on an attempt (or `kaisa attach 44.3 --take`) shows:
  ```
  Take over "backend" (run 44)?
    The agent pauses; you type in its terminal. Changes you make are marked as yours in review.
  [Enter] Take over   [V] Just view (read-only)
  ```
- **Hand back.** `[h]` (or `kaisa handback 44.3`) shows "Hand back to the agent? It will be told what you changed." The
  agent resumes.
- **Naming.** `handback` is only for handing back. Kaisa never asks you to "release" anything for cleanup.

#### F12 Steer
- **Send.** In `watch`, `[s]` (or `kaisa steer 44 "…"`).
- **Delivery states.** queued → delivered → **applied** or **not applied**. The agent's own report is shown, together with Kaisa's check of
  the next actions. If delivery is uncertain, Kaisa says so and offers [Resend] or [Take over].

#### F13 Answer a question
- **What a question shows.** It names the attempt and **who should answer** (default: the requester, then the run owner):
  "Run 44 · backend asks you: refresh tokens in Redis or the DB?"
- **Answers.** Answer in the notification, the Needs-you lane or your agent session. Answers are information, not decisions.
- **Unanswered questions.**
  - After `question_timeout` (default 30 min) Kaisa offers *[Let the agent choose and note it] · [Pause this step] · [Ask @owner]*.
  - The other steps keep running.

#### F14 Make a decision (permission boundary)
- **Example card:**
  ```
  Decision · run 44 wants network: registry.npmjs.org
    Why: `pnpm install` for new dependency `oauth4webapi`
    [A] Allow for this run   [P] Allow for this project (saved; expires in 90 days; allowed by org policy)
    [D] Deny — the step will try without it
  Needs: you (Touch ID)
  ```
- **Scope preview.** Every "always" or "project" option shows its scope and expiry, and whether the company policy permits it. If
  the policy does not permit it, the option is replaced with [Request exception] (F19o).

#### F15 Pause, resume, cancel
- **Pause.** `[p]` (or `kaisa pause 44`) shows "Paused: agents frozen. Requests already sent to a provider may still be billed."
- **States.**
  - `pausing → paused`.
  - `stopping → stopped`.
  - `unknown`: Kaisa can't confirm a process stopped. It shows this state honestly and offers [Check now].
- **Cancel.** `kaisa cancel 44` stops the run, keeps the work, and offers [Keep partial result] / [Discard after 14 days].
- **Sleep and restart.** After sleep or a reboot, Kaisa checks each attempt. Attempts it can confirm continue. The rest go to Needs you
  as "needs check" (see F19l). It never starts a duplicate.

### D. Finishing

#### F16 Review and approve
- **User sees:**
  ```
  Run 41 · candidate A
    What changed: login test now waits for token refresh; fixed race in session.rs
    Evidence: all 212 tests passed · no tests removed or skipped · review: 1 minor (naming)
              revised once: reviewer found a missing null check (major) → fixed → re-reviewed ✓
    Approving will: open PR #318 from kaisa/41-a → main   (your project setting: "PR, not merge")
    Undo: close PR any time
  [A] Approve   [R] Request changes   [X] Reject   [K] Keep for later
  Needs: you
  ```
- **Two steps when publishing is risky.** Accepting a candidate and publishing it are separate decisions when the target is protected,
  for example a release branch or a direct push. The effect preview always names the destination.
- **Base moved.** If the base moved since verification, Kaisa re-checks before approval.
- **Only verified results reach this screen.** Blocking review findings were already sent back to the
  agent and fixed (at most twice). If the loop could not fix them, you see the findings and the
  attempts instead of a candidate, with [R] retry with guidance · [T] take over · [X] drop.
  **M1 status:** an exhausted revision budget ends as `not delivered` (exit 4), printing the remaining
  findings and next steps. The interactive human handoff above awaits `kaisa review`; today you can
  start a new goal with guidance or inspect the preserved branch and continue manually.
- **Reject.** The work is kept with a visible countdown (default 14 days) and a `[K] Keep` option.

#### F17 Undo and restore
- `[u]` in `watch`/`review` (or `kaisa undo`) **previews first**:
  ```
  Undo last action (approve run 41)?  → will close PR #318 and keep the branch.
  ```
  Effects that cannot be undone are named before you act. Example: "the push to `main` cannot be undone. Kaisa can
  open a revert PR instead [R]."
- `kaisa restore 41` restores preserved work. If the receipt has expired or is missing, Kaisa says what is gone and
  what still exists.

#### F18 Cleanup (normally invisible)
- **Countdown.** Finished work shows "cleaned up in 10 min · [K] Keep" in `watch` and `review`. Rejected work shows "kept 14 days".
- **Commands.** Power users have `kaisa gc --dry-run` and `kaisa retain` (the same as `[K]`).

### E. When things go wrong

Every case uses the standard card: *what · why · who must act · safe default · options · back to watch*.
The default (marked ★) is always the safe choice.

| # | Situation | Card text (short) | Options |
|---|---|---|---|
| F19a | Tests fail on every candidate | "No candidate passed. Main failure: `test_refresh_expired` (token compared in wrong timezone)." | ★[R] Retry with this explanation (est. $0.60) · [A] Another agent · [V] Logs · [T] Take over |
| F19b | Agent silent | "Backend agent silent 10 min." | ★ Kaisa nudges, then, **only after confirming the old agent stopped**, moves the step to another provider · [Wait] · [T] Take over |
| F19c | Planner's provider out of quota | "Claude ran out of quota. GPT took over planning; plan and decisions were kept." | ★[Keep GPT until Claude's quota is back for 30 min] · [Keep GPT] |
| F19d | Worker's provider out of quota | "Backend moved from Claude to Codex. Its work was saved; the new agent starts from it. It will be re-verified." | ★ Continue · [V] View · [Stop] |
| F19e | No provider available | "Paused: no provider available. Next retry in 5 min. Checks already running continue." | ★ Auto-retry · [Retry now] · [Cancel] |
| F19f | Sandbox blocked a file write | "Blocked: write to `~/.npmrc` (outside the project). A project-local `.npmrc` would work." | ★[Use project-local file] · [Allow once] (decision) · [Deny] |
| F19g | Tests weakened | "Candidate B removed 2 tests, so it is not eligible." Also: `--passWithNoTests`, `--exit-zero` or `\|\| true` added to a test command; a CI pipeline file or a test-config file edited ("the candidate changed what its checks run: `.github/workflows/ci.yml`"). | ★[V] View diff · [Retry: keep tests] · [Appeal: these tests were obsolete] (decision; the reviewer confirms). **Today (M1):** no policy value lets these be delivered; when the gate is reached they stop for a person (exit `3`) with the file or signal named (earlier evidence, review or developer failures exit `4` first, and detection is lexical), and the Appeal decision does not exist yet, so a flagged candidate can only be retried. |
| F19h | Base moved | "main moved since verification. Re-checking… ✓ still passes" or "✗ conflict in session.rs" | ★ Auto re-verify · [Let an agent rebase] · [Resolve myself] |
| F19i | Agent logged out | "Codex is logged out. Run `codex login`, then press [C]." | [C] Check again · [Use another agent] |
| F19j | Budget reached | "Run 44 reached $25 (100%). Paused." | [+ $10] (decision) · ★[Stop and keep work]. "Finish the step" is offered only if it fits the budget the user just approved. |
| F19k | Disk low | "Low disk: new runs paused; running work is safe. Archives use 3.1 GB." | [Preview cleanup] · [Show usage] |
| F19l | Kaisa or machine restarted | "Recovered 2 runs. Needs check: run 44 · backend (couldn't confirm it stopped)." | ★[Check now] · [Wait] · [Cancel] (Kaisa confirms the stop first) |
| F19m | Unclear whether an agent started | "Not sure if backend started (contact lost). Kaisa won't start another copy until it knows." | ★[Check now] · [Wait] · [Cancel] (stop confirmed before closing). **No fresh start is offered while the status is unknown.** **Today (M1):** each attempt holds an owner lock, so another `kaisa` skips a live attempt; an attempt left by an older Kaisa is kept with the worktree, branch and a manual cleanup command printed. There is no card. |
| F19n | Partial protection expired | "Partial-protection approval expired." | [Re-approve] (decision) · ★[Read-only tasks only] |
| F19o | Company policy denies | "Org policy doesn't allow pushing to `release/*`." | [Request exception] → admin; the card shows the request status, the approver, and whether it was denied or expired |
| F19p | Offline | "Offline: providers unreachable. Local checks still run. Reconnecting… (next try 30 s)" | [Retry now] · [Cancel run] |

### F. Teams and administration

#### F20 Team lead overview and delegation
- **Overview.** `kaisa team` shows runs per person, review queues, cost per team and model, and the share of candidates that pass.
- **Delegating approval.** `kaisa admin delegate --path "services/payments/**" --to @bob,@carol --until 2026-12-31` previews the change, needs a decision,
  and can be revoked with `kaisa admin delegate --revoke …`.
- **Reassignment.** Pending reviews move to a new delegate automatically when the old one is revoked or absent (F33).

#### F21 Project memory
- **On by default, no setup.** Kaisa remembers each repository: its profile, decisions, and lessons
  from work that passed verification. Each run shows which items it used (F04).
- **See and correct.** `kaisa memory` lists what Kaisa knows about this repository, newest and most
  used first. `[!]` flags an item as wrong or stale (it stops being used until re-verified);
  `[d]` forgets it. Nothing an agent merely claims is remembered: lessons come only from delivered work.
- **Vector search.** On after a one-time model download at first run (from the company mirror when one
  is configured). If it is unavailable, memory still works by keyword search and says so.
- **Team sharing.** Sharing memory across a team is an admin setting (`kaisa admin knowledge share on`),
  because shared memory influences everyone's agents.

#### F22 Admin: policy packs
- **Publishing.**
  1. `kaisa admin pack validate` checks the schema and simulates the policy against recent runs.
  2. `kaisa admin pack publish --stage 10%` rolls the pack out in stages.
  3. `kaisa admin pack rollback` reverts it.
- **Bootstrap.** The first admin key is enrolled with `kaisa admin enroll`, using the company SSO and a hardware-backed key where available.
- **Signature failure.** If a pack's signature is invalid, clients keep the last good pack and show "policy update rejected (bad signature)"
  to the user and the admin (F34).

#### F23 Admin: budgets and cost
- **Setting budgets.** `kaisa admin budget set --team payments --daily 50 --monthly 800` and `--user`, `--run-default`.
- **Precedence.** run ≤ user ≤ team ≤ org. The lowest one wins.
- **Subscription costs.** Subscription usage is shown in tokens with the label "subscription (not billed per call)". API usage is shown in dollars.
- **Export.** `kaisa admin cost export --month 2026-10 --csv`.

#### F24 Admin: audit, incident, offboarding
- **Verifying the log.** `kaisa audit verify` checks the log. On a failure, the card shows the first bad record, which
  machine it came from, [Export evidence] and [Freeze runs on that machine].
- **Exporting a run.** `kaisa audit export --run 44` lists who asked for the run, which models ran, what was decided and by whom.
- **Offboarding.** `kaisa admin offboard @dave` previews the change. It revokes tokens and virtual keys, moves running and preserved work to the
  lead, and issues a receipt.

#### F25 Admin: templates and agents
- **Templates.**
  1. `kaisa admin template new` starts a wizard: steps, roles, gates and budget.
  2. `kaisa admin template test <name>` dry-runs the template on a sample repo.
  3. The template is published with the pack.
- **New agent.** `kaisa admin agent add` asks a few questions: the detect command, its login, whether it supports brokered mode, and
  how it reports quota. It then runs a conformance check before admins can enable the agent.

### G. Automation and special environments

#### F26 CI / headless
- **Running in CI.** `kaisa run --headless --json "update snapshots"`.
- **Exit codes.**

  | Code | Meaning |
  |---|---|
  | `0` | completed. The result is a PR, or nothing was needed. |
  | `3` | **waiting for a human**. The JSON contains the decision link, and the PR or check shows "needs approval". |
  | `4` | failed, with the reason in the JSON. |
  | `5` | blocked by policy. |
  | `6` | infrastructure unavailable: provider, network or sandbox. Safe to retry later. |
- **Continuing.** `kaisa run --resume <id>` continues after a human decides. A Gate is never auto-approved in CI.

#### F27 Monorepo and multi-repo
- **Monorepo.** Only the affected packages are verified.
- **Multi-repo.** One run creates one PR per repo. Review shows each repo's status and approval separately, and their
  dependencies, for example "api must merge before web".
- **Partial success.** If some repos merge and others fail, Kaisa offers [Retry the failed repo] or [Revert the merged ones] (shown as
  undo previews).

#### F28 Windows
- **Baseline (M0).** Windows behaves the same as macOS and Linux: native install, long paths enabled per repo, and PowerShell and cmd support.
- **Optional speed-up (M3).**
  - The card: "A Dev Drive would make copies about N× faster (measured on this machine). Creating one needs admin rights and some free disk space. [Create] [Not now] [Never]".
  - Declining keeps normal copies.
- **Optional integration (M3).** A Windows Terminal profile.

#### F29 Observed session
- **Starting.** `kaisa wrap claude` explains what changes compared with a normal session:
  - "Your normal Claude Code, plus: the project-only sandbox, network through Kaisa, automatic cleanup, cost tracking."
  - "Unlike Kaisa runs, Kaisa doesn't see every action, so this session is marked *observed*."
- **Leaving.** Exit the agent as usual. `kaisa review` offers to verify what you changed.

#### F30 Uninstall
- **Active runs first.** `kaisa uninstall` resolves active runs first: [Wait for them] · [Cancel and keep work].
- **Preview.** It then previews what will be removed: project hooks and MCP config (restored from the backups made by `init`), the service entry and
  local state. Preserved work stays readable in `~/kaisa-archive` unless you choose [Purge].
- **Platform steps.** The binary is removed through the package manager that installed it (brew, winget or the distro package). Kaisa prints
  the exact command.

### H. Added scenarios

#### F36 Large mission
- **Trigger:** `kaisa "migrate the API to v2 across all services" --template migration`, or any goal Kaisa
  judges too big for one plan.
- **User sees:** a plan split into areas, each run by its own coordinator:
  ```
  Mission 52  migrate the API to v2            budget $180 · est. 3–5 h
    api/        sub-run · 9 tasks       web/   sub-run · 14 tasks
    workers/    sub-run · 11 tasks      docs/  sub-run · 4 tasks
    up to 40 agents at once on this machine (measured) · queued work shows its position
  [Enter] Go   [E] Edit   [Q] Quit
  ```
- **While running:** the Needs-you lane still holds only real decisions; sub-run failures show as
  one card with a fix option, not as a flood of agent events.

#### F37 Light work on a local model
- **Setup:** you add a local model server once with `kaisa agent add ollama` (personal, M1), which runs the
  conformance check; an admin can do the same for everyone with `kaisa admin agent add`.
- **Effect:** summaries, classification and first-pass reviews go to it automatically when the router
  grades them as light work; F04's status line says `model: local (light work)`. Code edits and security reviews never
  go to a tool-less model.
- **As built (M1, T33).** Light work is the **first-pass review**: when a T0 model is registered, it reviews each
  candidate before the full review. Its blocking findings send the candidate back without spending the full review;
  its approval is advisory, so the full review still decides. The run prints `first pass: model: local (light work)`.

#### F31 Repository not ready
- **Dirty working tree.** "You have uncommitted changes. Kaisa works on a copy of **committed** state." The options:
  - [Include my uncommitted changes in the copy];
  - ★ [Use last commit only];
  - [Commit first].
- **Not a git repo.** [Choose a repo] · [`git init` here (preview)].
- **Submodules, LFS, sparse checkout.** Kaisa detects them and shows what the copy will include. Missing LFS objects lead to [Fetch] or [Skip LFS files].
- **Dependencies unavailable** (for example a private registry that needs a login). The card shows the failing step and the login hint.

#### F32 You're away, or notifications are off
- **Needs you lane.** Everything that needs you is kept in the Needs you lane. It is shown on the next `kaisa` command, for example
  "2 things need you", and on the issue or PR.
- **Optional digest.** An email or chat digest is available *(after v1)*.
- **Timeouts.** Question timeouts (F13) apply. Decisions never time out into "yes".

#### F33 No eligible reviewer
- **Self-approval.** By default, the person who requested the run can approve it, unless the policy requires a second person for that path.
- **Second-person paths.** If such a path is requested and no delegate is available, the card shows "needs a second reviewer: @bob (away until Mon)" with
  [Ask @carol] · [Ask lead].

#### F34 Trust problems
- **Proxy or certificate failure.** "Can't verify `api.anthropic.com` (certificate not trusted). Your company proxy may need its root certificate installed." → [Show how] · [Retry].
  Kaisa never offers "ignore certificate".
- **Bad policy-pack signature:** see F22.
- **Revoked credentials or virtual key.** "Your access to provider X was revoked." → contact admin. The affected runs pause with their work kept.

#### F35 Owner leaves, or partial external effects
- **Owner leaves.** Runs owned by someone who left move to their lead (F24). Each item shows its receipts and what was already published.
- **Partial external effects.** For example, a PR was opened but the merge failed. The card names what happened and offers the matching undo or a retry.

---

## 7. Command surface (v1)

| Level | Commands |
|---|---|
| Everyone | `kaisa "<goal>"` · `kaisa watch` · `kaisa review` · `kaisa` (Home) |
| Also reachable as keys in watch/review | take over `[t]`, hand back `[h]`, steer `[s]`, pause `[p]`, undo `[u]`, keep `[k]` |
| Common commands | `ask`, `attach`, `steer`, `pause`, `resume`, `cancel`, `undo`, `restore`, `init`, `doctor`, `status`, `console`, `memory`, `agent add` |
| Advanced | `run` flags `--n`, `--agents`, `--template`, `--plan`, `--budget`, `--issue`, `--issues`; `retain`, `gc`, `handoff` (planner), `handback` (attempt), `wrap` |
| Team / admin | `team`, `admin enroll`, `admin pack validate\|publish\|rollback`, `admin budget set`, `admin delegate`, `admin offboard`, `admin template new\|test`, `admin agent add`, `admin knowledge share on\|off`, `admin cost export`, `audit verify`, `audit export` |
| Automation | `run --headless --json`, `run --resume` |

Every command supports `--json`, `--plain` and `--help`, in `ko` or `en`. Read-only commands never start the daemon or write state.

**Today (M1):** `kaisa "<goal>"`, `watch`, `agent`, `policy` and `--help`/`--version` work. Every other verb in this table is reserved and answers "not implemented yet" with exit `2`, whatever follows it, so it cannot start a run as goal text; there is no `--json`, `--plain` or `ko`/`en` yet.

---

## 8. Architecture deltas

The v0.1 deltas are included in `architecture.md` v0.4: §5.7 Requester authority, §5.11 experience services, and
ADR-0017 to ADR-0022.

The v0.2 additions below are included in `architecture.md` v0.5: §5.5 (source state), §5.11 (admin lifecycle), §5.12 (decisions, answers, previews and approval rules), §6 (the CI contract), and ADR-0023 to ADR-0027.

| Delta | Needed by |
|---|---|
| Answer vs decision typing on every Gate and question; question timeouts with owner fallback | U9, F13, F32 |
| Scoped, expiring decisions (machine / run / project, with expiry), and reduced-protection approval cards | F02, F14, F19n |
| Effect preview API (what approving or undoing will do, and what cannot be undone) | F16, F17, F27 |
| Second-person approval rules and reviewer reassignment | F20, F33 |
| CI exit-code contract and `run --resume` | F26 |
| Dirty-tree and LFS handling in workspace creation | F31 |
| Admin journeys: enroll, staged pack rollout and rollback, offboarding receipts, template test, agent conformance | F22–F25 |

## 9. Milestone mapping

| Milestone | Flows that must work end to end |
|---|---|
| M0 | F01, F02 (no policy pack), F04 (profile + context line), F15, F18, F19i/k/l/m/n, F28 baseline, F31 |
| M1 | F03, F05, F06, F08 (GitHub), F10–F14, F16 (with automatic revision), F17, F19a/b/f/g/h/o, F21 (per repository), F29, F32, F34, F37 |
| M2 | F07, F09, F10 (`console`), F19c/d/e/j/p, F23, F26, F27, F33, F36 |
| M3 | F20, F21 (team sharing), F22, F24, F25, F28 optional, F35, desktop, Linear, mobile/chat |

## 10. Usability acceptance (measured, not assumed)

| Measure | Target |
|---|---|
| Install → first verified result, new user, no docs | ≤ 10 minutes |
| Commands a P1 user must learn for daily work | 3 |
| Prompts per routine task with no permission boundary | 0 |
| Blocker cards without a safe default and a fix option | 0 (CI-enforced) |
| Things a user must re-explain about a repository in its second session | 0 |
| Settings needed before memory, routing or context work | 0 |
| Duplicate agent starts in chaos tests | 0 |
| Abandoned resources after a normal week | 0 |
| 4-week retention of pilot users | tracked; target set after the first pilot |

These targets are checked with scripted sessions run by 5 new users per milestone on each of the three OSes.
