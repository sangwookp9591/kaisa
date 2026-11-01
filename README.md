<div align="center">

<img src="docs/assets/kaisa-banner.svg" alt="Kaisa: run coding agents safely and see only work that already passed verification" width="100%">

<p>
  <a href="https://github.com/sangwookp9591/kaisa/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/sangwookp9591/kaisa/ci.yml?branch=main&style=for-the-badge&label=CI%20%C2%B7%203%20OS&labelColor=0D1626&color=2DD4BF" alt="CI on macOS, Linux and Windows"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-8B7CF6?style=for-the-badge&labelColor=0D1626" alt="MIT License"></a>
  <img src="https://img.shields.io/badge/Rust-1.99%20%C2%B7%202024-F5B841?style=for-the-badge&logo=rust&logoColor=white&labelColor=0D1626" alt="Rust 1.99, edition 2024">
  <img src="https://img.shields.io/badge/macOS%20%C2%B7%20Linux%20%C2%B7%20Windows-first--class-5EEAD4?style=for-the-badge&labelColor=0D1626" alt="macOS, Linux and Windows">
  <img src="https://img.shields.io/badge/status-M0%20%2F%20M1%20partly%20done-F5B841?style=for-the-badge&labelColor=0D1626" alt="Status: M0 and M1 partly done">
</p>

<p>
  <img src="https://img.shields.io/badge/Claude%20Code-developer%20%26%20reviewer-D6E3EC?style=flat-square&labelColor=1E2E48" alt="Claude Code">
  <img src="https://img.shields.io/badge/Codex-observed%20developer-D6E3EC?style=flat-square&labelColor=1E2E48" alt="Codex">
  <img src="https://img.shields.io/badge/local%20models-T0%20first%20pass-D6E3EC?style=flat-square&labelColor=1E2E48" alt="Local models">
  <img src="https://img.shields.io/badge/one%20binary-no%20Node%20%C2%B7%20Python%20%C2%B7%20Electron-D6E3EC?style=flat-square&labelColor=1E2E48" alt="One binary">
</p>

**A control plane for coding agents that hands you only what already passed your tests and an independent review.**

[See it run](#see-it-run) · [Kaisa vs ruflo / Orca](#kaisa-vs-ruflo-and-orca) · [Install](#install) · [First run](#first-run) · [Quick start](#quick-start) · [How a verified run works](#how-a-verified-run-works) · [Protection status](#protection-status) · [Docs](#documentation)

</div>

---

Kaisa runs coding agents such as Claude Code and Codex for you. Each attempt gets its own copy of
your repository and runs inside an OS sandbox. The result is checked against your tests and by an
independent reviewer, sent back for revision when the review finds a blocking problem, and you are
asked only when a human decision is actually needed.

> [!NOTE]
> **Status: M0 and M1 are partly done.** A goal runs end to end, and on macOS real Claude Code and
> Codex CLIs have completed verified runs, also after the latest fixes for state isolation, child
> environments, descendant cleanup and attempt locks. Several designed parts are built and tested but
> not yet used by a run, and some are not built; they are marked ◐ in [`docs/tasks/`](docs/tasks/) and
> listed under [Current differences from the design](#current-differences-from-the-design). CI runs on
> macOS, Linux and Windows on pushes to `main` and on pull requests (superseded pull-request runs may be
> cancelled). Real-agent runs have been done on macOS only. Some protections are *reduced* on some
> platforms; the [protection status](#protection-status) table says exactly which. Performance and
> security properties are published only after they have been measured and tested on every supported
> platform.

<a id="see-it-run"></a>

## 🎬 See it run

These are real runs, recorded on macOS with a logged-in Claude Code CLI against a small Rust
repository whose `format_date` prints the month one too high.

**A goal, verified.** The developer fixes the bug in its own worktree, the trusted `cargo test` runs on
a clean checkout, a second Claude Code reviews the frozen candidate, and only then is the result
offered as a branch.

<p align="center"><img src="docs/assets/screens/verified-run.png" alt="A real kaisa run: developer and reviewer attempts, then 'verified: trusted checks passed and the review approved' and the branch to merge" width="100%"></p>

**A goal that weakens the tests stops for you.** Asked to delete the failing test, the agent does it,
the review may even approve it, and Kaisa still refuses to deliver: the candidate has fewer tests.

<p align="center"><img src="docs/assets/screens/weakened-tests.png" alt="A real kaisa run that ends 'waiting for you: weakened_tests: src/lib.rs: tests and assertions decreased (1 -> 0)'" width="100%"></p>

<table>
  <tr>
    <td width="50%"><b><code>kaisa watch</code></b>: every attempt with its state, cleanup, tokens and cost<br><br><img src="docs/assets/screens/watch.png" alt="kaisa watch listing the developer and reviewer attempts, both exited and reclaimed, with tokens and cost"></td>
    <td width="50%"><b>No accidental paid runs</b>: planned commands and unknown options never start an agent<br><br><img src="docs/assets/screens/cli-guard.png" alt="kaisa refusing 'review the parser', '--n 3' and 'gc --dry-run' with exit codes 2 and 64 and a hint to quote the goal"></td>
  </tr>
</table>

**A terminal home.** `kaisa tui` keeps Needs you above the goal and attempt tree, with recorded gate
reasons, checks, review findings and the verified branch to merge in a detail pane. This recording
uses only scripted fake agents; unavailable provider usage and cost are shown as `n/a`.

<p align="center"><img src="docs/assets/screens/tui.png" alt="kaisa tui with a Needs you lane, developer and reviewer attempts, a revision, and recorded verification evidence in the detail pane" width="100%"></p>

<p align="center"><sub>Animated versions: <a href="docs/assets/screens/verified-run.gif">verified run</a> · <a href="docs/assets/screens/weakened-tests.gif">weakened tests</a> · <a href="docs/assets/screens/cli-guard.gif">CLI guard</a> · <a href="docs/assets/screens/tui.gif">terminal home (fake agents)</a>. Costs in the real-agent recordings are what the Claude Code CLI reported.</sub></p>

---

## Spec-driven workflow

```sh
kaisa spec draft --file requirements.txt
kaisa spec show <id>
kaisa spec approve <id>                 # terminal review and digest confirmation
kaisa build --spec <id>
kaisa spec report <run>
```

Spec builds characterize untested legacy behavior before development, author private
acceptance and adversarial tests, review test quality and the candidate, and publish
only after the orchestrator's final-tree gate passes. Provisional approval requires
`kaisa policy set allow_provisional_spec true` and
`kaisa spec approve <id> --provisional`; it never counts as human approval.
See the [offline fake-agent demo](examples/spec-demo/),
[recorded results](docs/evidence/spec-demo/traceability.md), and
[platform and trust limits](docs/evidence/limitations.md).

## ✨ Why Kaisa

| | Without Kaisa | With Kaisa |
|---|---|---|
| 🧹 | You babysit agents, switch branches and clean up terminals | Every attempt runs in its own copy, and cleanup happens on its own |
| 🛡️ | Agents ask for permission all the time, or you turn permissions off | Agent effects go through a broker that enforces a write set; real decisions come to you |
| ✅ | You review every attempt, including the broken ones | You see only candidates that passed your checks and an independent review |
| 🧠 | A single agent loses track of a long task | A reviewer, bounded revisions and project memory carry the context; explicit DAG plans run verified tasks in parallel |
| 📊 | Cost and progress are invisible until the end | `kaisa watch` shows every attempt with its state, tokens and cost |

<a id="kaisa-vs-ruflo-and-orca"></a>

## ⚖️ Kaisa, ruflo and Orca

They solve different problems. Orca and ruflo help you **run many agents comfortably**. Kaisa answers
a narrower question with evidence: **can you trust what an agent hands back?**

| | ruflo | Orca | Kaisa |
|---|---|---|---|
| What it is | An agent meta-harness: swarms, memory, plugins for Claude Code and Codex | A desktop workspace for many agent terminals and worktrees, with supervised orchestration | A verification gate for agent work |
| What "done" means | A caller-supplied `success` or exit 0; in our reading a dual run counts as done even when its reviewer reports a vulnerability | The worker's own `worker_done` message | Your trusted checks passed **fresh, on the exact candidate tree**, and an **independent reviewer** approved it |
| A change that weakens the tests | No built-in stop found | Not its job | Stops for a person (exit `3`), whatever the review says; seen in a real Claude Code run on macOS |
| Agent containment | Roles are labels on records; few controls on the execution path | Agents run with their own permission modes (including bypass modes) | Own worktree per attempt, an OS sandbox, a broker that enforces the write set; any reduction is labelled |
| Record of what happened | JSON records kept apart from execution | SQLite orchestration ledger | Hash-chained ledger; every check result and verdict is bound to the candidate's digest |
| Footprint | about 1.7 GB installed (npm) | about 700 MB (Electron app) | one Rust binary |

<sub>ruflo 3.65.0 and Orca 1.4.244, read statically on 2026-10-30; details and file citations in
[`docs/research/ruflo-control.md`](docs/research/ruflo-control.md) and
[`docs/research/prior-art-rea.md`](docs/research/prior-art-rea.md). Later versions may differ.</sub>

**Use them together.** Run Kaisa from an Orca terminal, or hand a goal to Kaisa from inside Claude Code
with `/kaisa`, and keep your swarm or workspace tool for everything else. What comes back through
Kaisa has passed your checks and a review.

**What Kaisa does not do yet:** automatic goal decomposition into a swarm, a plugin ecosystem, a web
console, packaged releases, and real-agent runs on Linux and Windows. See
[Planned](#planned) and [Current differences from the design](#current-differences-from-the-design).

<a id="install"></a>

## 📦 Install

**After the first release is published**, install without Rust or a C compiler:

```sh
# macOS / Linux
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL https://github.com/sangwookp9591/kaisa/releases/latest/download/install.sh | sh
```

```powershell
# Windows (PowerShell 5.1+ or pwsh)
$installer = Join-Path ([IO.Path]::GetTempPath()) (([Guid]::NewGuid().ToString()) + '.ps1'); try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -UseBasicParsing https://github.com/sangwookp9591/kaisa/releases/latest/download/install.ps1 -OutFile $installer; & ([scriptblock]::Create([IO.File]::ReadAllText($installer))) } finally { Remove-Item -LiteralPath $installer -Force -ErrorAction SilentlyContinue }
```

The installers download one binary archive and verify it against the release's `SHA256SUMS` before
installing. They need no administrator rights and start no daemon. macOS/Linux installs to
`~/.local/bin` and prints the exact PATH line to add to your shell profile when needed; Windows
installs to `%LOCALAPPDATA%\Programs\kaisa`, announces the user PATH update, then saves it and
broadcasts the change. Add the printed PATH line if needed, open a new terminal, then run: `kaisa`.

Set `KAISA_VERSION` (for example `v0.1.0`) to pin a release, or `KAISA_INSTALL_DIR` to choose an
absolute install directory. `KAISA_INSTALL_BASE_URL` overrides the release root for mirrors or
offline tests: it must provide `latest/download/VERSION` and `download/v<version>/<asset>` plus
`SHA256SUMS` in that version's directory. Local `file://` roots are supported. Proxy and certificate
handling follows curl / PowerShell; TLS verification stays enabled. Archives are checksummed,
**not code-signed**; checksums confirm integrity against the same trusted release host and are not independent signatures; Homebrew and winget packages are not available yet. The release workflow targets musl Linux
on x86_64 and aarch64, with SQLite bundled (native build validation pending); macOS supports both architectures and Windows supports x86_64.

**Build from source**

**You need**

| | macOS | Linux | Windows |
|---|---|---|---|
| Rust toolchain | [rustup](https://rustup.rs), then Rust **1.99.1** (pinned in `rust-toolchain.toml`) | same | same |
| C compiler (for the bundled SQLite and BLAKE3) | Xcode Command Line Tools: `xcode-select --install` | `build-essential` / `gcc` | Visual Studio Build Tools (C++ workload) |
| git | 2.x | 2.x | Git for Windows |
| An agent CLI, logged in | [Claude Code](https://docs.anthropic.com/en/docs/claude-code) (`claude`): the default developer and reviewer. Optional: [Codex CLI](https://github.com/openai/codex) (`codex`) for `--agent codex`, and an OpenAI-compatible local model server for the first-pass review | same | same |
| Sandbox | Seatbelt, built in (macOS 14+) | Landlock: kernel 6.2+ confines every write, 6.7+ also TCP; older kernels run reduced and say so | AppContainer (Windows 11 / Server 2022) |

**Source install**

```bash
rustup toolchain install 1.99.1

# from GitHub
cargo +1.99.1 install --locked --git https://github.com/sangwookp9591/kaisa kaisa

# or from a clone (rust-toolchain.toml selects 1.99.1 inside the checkout)
git clone https://github.com/sangwookp9591/kaisa
cd kaisa
cargo install --locked --path bin/kaisa

kaisa --version
```

`cargo install` puts `kaisa` in `~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on Windows), which rustup
adds to your `PATH`. The `+1.99.1` matters for the GitHub install: outside the checkout Cargo uses your
default toolchain, and an older one stops with a `rust-version` error.

<a id="first-run"></a>

### First run

```bash
claude                                  # once: log in to Claude Code (and `codex login` for Codex)
cd ~/code/my-project                    # any git repository with at least one commit
kaisa                                  # read-only welcome, setup diagnosis, and next step
kaisa doctor                           # detailed read-only diagnosis
kaisa init                             # preview detected checks (optional)
kaisa init --yes                       # save the preview as kaisa.toml
kaisa "fix the flaky date test"         # develop → check → review → verified, or it tells you why not
kaisa watch                             # what ran, its state, tokens and cost
git merge --ff-only kaisa/att_…         # bring a verified result in (the last output line says exactly how)
```

With no `kaisa.toml`, Kaisa finds the test command itself from the repository root: `cargo test --workspace` for
`Cargo.toml`, the `test` script of `package.json` (npm, pnpm, yarn or bun, from the lockfile),
`python -m pytest` when `pyproject.toml` mentions pytest, and `go test ./...` for `go.mod`. A goal in
a repository where none is found is not delivered (exit `4`, "no trusted check produced evidence"):
Kaisa never calls unchecked work verified.

`kaisa init` previews detected build/test/lint commands and risky repository settings. It asks
before writing on a terminal; without a terminal it only previews unless you pass `--yes`.
Existing `kaisa.toml` files are kept unchanged. A nonempty `[profile]` replaces detected commands
entirely; include `test` to keep verification available. An empty profile falls back to detection,
so initializing a project before adding tests does not disable future detection. Valid explicit
checks take precedence over a malformed `package.json`; doctor reports that manifest as a warning.
Goals read the working-tree profile. Changing `kaisa.toml` or an existing repository file named
by its trusted check argv stops delivery for a person. The same protection covers script files
named by the base `package.json` test script and its lifecycle hooks. A simple override is:

```toml
[profile]
test = ["cargo", "test", "--workspace"]
```

Init writes each command as `[[profile.test]]`, `[[profile.build]]` or `[[profile.lint]]` with an
`argv` array and an optional repository-relative `directory`, preserving monorepo checks. It does
not run the commands or mark them confirmed. Agent integration is supplied separately.

Home shows recent ledger goals and attempts after a run, including goals waiting for a person.
Home and doctor do not create state or write repository files. Doctor invokes only `claude --version`
and `codex --version`; Git status disables external filters, fsmonitor and submodule inspection
(filtered files can conservatively appear dirty). Login is **not checked** (no credentials, config, keychain or network access),
and it prints the login commands. Claude Code is required for the default developer/reviewer flow;
Codex is optional. Doctor reports the same predicted sandbox gaps as admission, including platform
limits and the reduced/observed defaults. It checks state-directory permissions without a probe
file (Windows opens the existing directory with child-creation access); actual sandbox creation and state
writes are validated when a goal starts. Doctor exits `1` for a missing required item and `0` when
required checks pass; unknown login and optional Codex remain labelled warnings.

Kaisa keeps its state (ledger, attempt worktrees, policy, agents) outside your repository:

| macOS | Linux | Windows |
|---|---|---|
| `~/Library/Application Support/Kaisa` | `$XDG_STATE_HOME/kaisa`, else `~/.local/state/kaisa` | `%LOCALAPPDATA%\Kaisa` |

Set `KAISA_STATE_DIR` to use another directory. Results stay as `kaisa/att_…` branches in your
repository until you merge or delete them.

<details>
<summary><b>Update and uninstall</b></summary>

```bash
# binary install: rerun the one-line installer to update; delete its kaisa binary to uninstall
# source install: update / uninstall with Cargo
cargo +1.99.1 install --locked --git https://github.com/sangwookp9591/kaisa kaisa

# uninstall
cargo uninstall kaisa
rm -rf ~/Library/Application\ Support/Kaisa      # macOS; ~/.local/state/kaisa on Linux, %LOCALAPPDATA%\Kaisa on Windows
git branch --list 'kaisa/*'                      # in each repository: result branches you may still want
```

</details>


<a id="quick-start"></a>

## 🚀 Quick start

Checked on macOS with logged-in Claude Code and Codex CLIs: a verified fix, a second session that
recalls the first one's lesson, Codex developing with Claude reviewing, a change to what the checks
run (stops for a person), a goal that deletes a test (stops for a person) and `--confine-claude`. A
delivered result stays on its `kaisa/att_…` branch; the last line of the output says how to bring it in.

```bash
kaisa                                    # Home: setup or recent work
kaisa doctor                             # diagnose agents, repo, checks, risks, protection and state
kaisa init --yes                          # optional: save detected project checks
kaisa "fix the flaky date test"           # run a goal, verified (Claude Code by default)
kaisa --agent codex "fix the flaky test"  # Codex develops (observed), Claude Code reviews
kaisa watch                               # attempts, state, tokens and cost
kaisa tui                                 # live terminal home; --plain prints one accessible frame
kaisa agent add local --endpoint http://127.0.0.1:11434/v1 --model qwen=T0
kaisa agent list                          # personal agents and model servers
kaisa policy show                         # your personal policy
kaisa policy set revision_limit 3         # change it (validated before it is saved)
kaisa mcp serve                           # MCP server for Claude Code (see below)
```

`kaisa tui` refreshes the ledger once a second. Use `n` for Needs you, `f` for failed, `/` to
search, Enter for details, `g` to submit a new background goal, and `?` for all keys. `--plain`
(and output without a TTY) prints the same information once and exits. `KAISA_LANG=ko` (also `ko_KR` or `ko-KR.UTF-8`, case-insensitive `ko` prefix) selects
Korean TUI labels. When `KAISA_LANG` is unset, the first nonempty value of `LC_ALL`,
`LC_MESSAGES`, then `LANG` selects Korean if it starts with `ko` (case-insensitive); otherwise
the default is English. Recorded goal text and engine codes stay as written.

A goal ends with one of these results, which scripts and CI can rely on:

| Exit code | Meaning |
|---|---|
| 🟢 `0` | verified: the candidate passed fresh checks and the review (verified path; see the note below the table) |
| ⚪ `1` | infrastructure or configuration failure, such as an unlocatable state directory or an old policy that disables a person gate |
| 🟡 `3` | waiting for you: a person must decide, and Kaisa says why |
| 🔴 `4` | not delivered: no fresh passing evidence, revisions ran out, or the agent could not be started |
| ⚪ `2` | the word is a command that is not implemented yet |
| ⚪ `64` | usage error, including an unknown option |

The scripted `--agent-program` path without `--reviewer-arg` runs no checks and no review and can still exit `0`; it is for testing, not a verified result.

Words the docs name as commands (`review`, `run`, `gc`, `wrap`, `handoff` and the rest of the planned
list) never start a run. `kaisa review the parser` exits `2` and prints the quoted form,
`kaisa "review the parser"`. An unknown option such as `--n 3` exits `64` and names the option. A goal
that must contain `--word` can follow `--`: `kaisa -- fix the --verbose bug`. Run `kaisa` from a
subdirectory and it uses the enclosing git repository and says so in one line:
`using the git repository at <root>`. `kaisa watch` only reads the ledger: it does not append events, migrate the database or rebuild projections. A closed ledger is read without any file change; reading a live WAL database can create or update SQLite shared-memory sidecar files (`-shm`).

### Parallel tasks and candidate alternatives

`kaisa "<goal>" --candidates 3` runs independent developer attempts in separate worktrees,
with the normal trusted checks, review and bounded revision for each. A reviewer compares the
passing alternatives with bounded peer patches and recorded check receipts. Only one
comparison-approved candidate is printed as the result; equivalent alternatives prefer the
lowest candidate index. If none passes, nothing is delivered. Alternatives currently use the same
configured developer/reviewer routes; automatic model diversity is not implemented.

For tasks that should contribute to one result, supply an explicit plan:

```json
{"tasks":[
  {"id":"task_api","instruction":"Implement the API change","write_set":["src/api"]},
  {"id":"task_web","instruction":"Update the web client","write_set":["src/web"]},
  {"id":"task_docs","instruction":"Document both changes","role":"documentation",
   "write_set":["docs"],"depends_on":["task_api","task_web"]}
]}
```

Run `kaisa "Update the API and client" --plan mission.json`. Task roles are `developer` (default),
`tester` or `documentation`. Task starts follow plan order within deterministic concurrent waves.
Each dependent starts from a three-way merge of **all** its parents' preserved commits. Every task
passes its own checks and review by default; the integrated tree then gets fresh checks and an
independent review before delivery. Merge conflicts and writes outside declared ownership block
the mission and name the files. Git union and external merge drivers are disabled during
integration; Kaisa does not automatically resolve conflicts.

Policy keys `max_parallel`, `on_failure` (`stop` or `continue_independent`) and `verify_each_task`
can be set with `kaisa policy set`. Their CLI overrides are `--max-parallel`, `--on-failure` and
`--verify-each-task true|false`. Default concurrency is half the available CPUs, capped at four,
with at least one slot; per-task verification defaults to true. `stop` admits no new wave after
failure; `continue_independent` runs unaffected branches. Both drain admitted siblings through
existing process-tree supervision and deadlines. Turning per-task verification off is explicitly
reported: only the final integrated candidate receives checks and review. `kaisa status`,
`kaisa watch` and the TUI show running mission task IDs from the ledger.

<a id="use-kaisa-from-claude-code"></a>

## 🔌 Use Kaisa from Claude Code

Register Kaisa's MCP server once for yourself (user scope; nothing is written into any repository):

```bash
claude mcp add kaisa -- kaisa mcp serve
```

Then, in a Claude Code session inside a git repository, ask for a goal with the slash command or in
plain words:

```text
/kaisa fix the flaky date test
```

Claude calls `kaisa_start_goal`, which starts `kaisa "<goal>"` as a **background process** (its own
process group; output in `<state>/goals/<handle>/output.log`) and returns a handle. It keeps running
if you close the session. Ask "how is it going?" and Claude calls `kaisa_status` (running, verified,
waiting for you, or not delivered, with tokens and cost from the ledger once the run ends);
`kaisa_result` returns the final lines and the exact `git merge --ff-only kaisa/att_…` command.
Several goals can run at once. `agent` can be `claude` (default) or `codex`; `repository` defaults to
the directory Claude Code was started in and must be a git repository with a commit.

| From inside the agent | |
|---|---|
| Start a verified goal, read its status and result | ✅ |
| Merge, approve or publish | ❌ never: Kaisa has no tool for it. A run that needs a person reports **"This needs you"** with the reason, and you continue in a terminal in the repository. `kaisa_result` only prints the merge command for you to run. |
| Answer an agent's question, cancel a run | not available yet |

The project slash command is `.claude/commands/kaisa.md`: `kaisa init` adds it with the rest of the
project setup, prints the `claude mcp add` line above, and does not overwrite a file you wrote. A team that wants the server in the
repository can instead commit `{"mcpServers":{"kaisa":{"command":"kaisa","args":["mcp","serve"]}}}`
as `.mcp.json` (`claude_code_project_server`), or let Claude Code write it with
`claude mcp add --scope project kaisa -- kaisa mcp serve` (which adds `"type": "stdio"` and an empty
`"env"`). Kaisa's settings scanner accepts exactly that entry, in either form, on macOS and Linux and
still flags any other server (on Windows every repository server is flagged). Claude Code asks you to
approve a project server before it uses it; approving only `kaisa` records
`"enabledMcpjsonServers": ["kaisa"]` in `.claude/settings.local.json`, which Kaisa also accepts,
while approving any other server name is still flagged.

<a id="how-a-verified-run-works"></a>

## 🔁 How a verified run works

```mermaid
flowchart LR
    goal(["kaisa &quot;goal&quot;"]) --> recall["Recall<br/>profile + lessons"]
    recall --> develop["Develop<br/>own worktree · sandbox · broker"]
    develop --> freeze["Freeze<br/>candidate digest"]
    freeze --> check["Check<br/>your tests, no agent"]
    check --> review["Review<br/>independent verdict"]
    review --> gate{"Gate"}
    gate -- "fresh evidence + approval" --> ok(["verified · exit 0"])
    gate -- "blocking finding" --> develop
    gate -- "weakened tests · changed checks" --> person(["waiting for you · exit 3"])
    gate -- "no valid evidence · revisions spent" --> no(["not delivered · exit 4"])
    ok --> learn["Learn<br/>lesson for this repo"]
    classDef good fill:#0F2E2B,stroke:#2DD4BF,color:#E6F1F5
    classDef wait fill:#33270C,stroke:#F5B841,color:#E6F1F5
    classDef bad fill:#3A1519,stroke:#F0616D,color:#E6F1F5
    classDef step fill:#13213A,stroke:#8B7CF6,color:#E6F1F5
    class ok,learn good
    class person wait
    class no bad
    class recall,develop,freeze,check,review,gate,goal step
```


1. **Recall.** The context pack carries the project profile and up to five relevant lessons from
   earlier verified runs *of this repository*; lessons never cross repositories. A second session
   needs no re-explanation.
2. **Develop.** The developer agent works in its own worktree; its file writes go through Kaisa's
   broker, which enforces the write set. Before it starts, Kaisa compares what the OS sandbox really
   guarantees (write confinement, TCP, UDP, Kaisa state read isolation, descendants) with what is needed.
   A shortfall stops the run with exit `3` and names what is missing, unless a documented default
   policy covers it (the output names it, for example `claude-reduced-until-T16.0`) or you accept it for a limited time with
   `--accept-reduced <write,tcp,udp,state-read,descendants> --accept-until <unix time in ms>`.
   Limits of the OS itself are admitted without a Decision but always labelled: `descendants` on macOS
   and Linux, and also `state-read` on Linux (output: `admitted on documented platform limits
   (platform-limit)`). A Decision has to name every other missing capability. An unconfined run misses
   all five, so on macOS and Windows a Decision written for the old four codes
   (`write,tcp,udp,descendants`) no longer covers it; add `state-read`.
3. **Freeze.** The result becomes an immutable candidate (commit and tree digest).
4. **Check.** Your project's test command runs on a clean checkout of the candidate, in the sandbox,
   with no agent involved. No setting lets a detected change to what the checks run be delivered; when that gate is reached it needs a person (exit `3`): check-definition
   files (`package.json`, `Cargo.toml`, `Makefile`, `conftest.py` and similar), CI pipeline files (`.github/workflows/`, `.gitlab-ci.yml`, `Jenkinsfile` and similar),
   and a test command that can no longer fail (`--passWithNoTests`, `--exit-zero`, `|| true`). Deleted
   or disabled tests stop for a person too. No setting turns this off. Earlier failures (no valid evidence, a missing or stale verdict, an unsuccessful developer) end as not delivered (`4`) before that gate is reached. Detection is lexical over the named files and constructs, so a differently spelled change can be missed. If a check
   changes the candidate's source files, Kaisa refuses all evidence and the run is not delivered.
5. **First pass (optional).** If you registered a T0 local model, it reviews first. Its blocking
   findings send the candidate back without spending the full review; its approval is only advisory.
6. **Review.** A reviewer reads the frozen candidate and answers with a structured verdict. Its
   context carries the candidate's diff against the base (bounded; a cut says how to read the rest in
   the worktree) and the trusted checks' results for that exact candidate: command, exit, whether it ran
   confined, and a short output tail with secrets dropped. The diff and output are quoted as untrusted
   data, so text in them cannot pose as Kaisa's results or as a verdict. A first-pass local model gets
   the same evidence. Kaisa reads the verdict only from one final `kaisa-verdict` region of the reviewer's answer; a missing,
   repeated, quoted or malformed verdict never counts as approval.
7. **Gate.** Only a candidate with fresh passing evidence and an approving verdict is delivered.
   Blocking findings go back to the developer as a revision, up to the policy's revision limit.
8. **Learn.** A delivered run records a lesson, verified by digest, for later runs on the same
   repository only.

Before any agent starts, Kaisa scans the repository's agent settings and stops if they would widen
an agent's authority (for example `enableAllProjectMcpServers: true` or a non-empty
`enabledMcpjsonServers` in `.claude/settings.json` or `.claude/settings.local.json`); the frozen candidate's own settings are scanned again before a reviewer or a
revision starts in it. Every run records the policy snapshot it ran under. A run interrupted by a
crash is found and tidied on the next `kaisa` start; an attempt in an unknown state is never retried
without proof. Each attempt holds an owner lock and records its owner's process identity, so a
second `kaisa` process skips a live attempt instead of ending its agent; if the recorded owner is
provably dead, the lock no longer protects the attempt, even if another process holds it. An attempt
left by an older Kaisa has no lock; it is kept, with the branch, the worktree and a
`git worktree remove` command printed for you to run once you have confirmed its agent is gone. The
next `kaisa` start then records that cleanup and stops reporting the attempt. An agent program that cannot be started ends the goal with exit `4`, and
its attempt is reclaimed. A direct process-creation failure is recorded as `exited:failed-to-start` and reclaimed. On macOS a sandbox wrapper that starts but cannot exec the agent can instead report its own nonzero exit (for example `exited:71`); the goal still exits `4`.

<a id="protection-status"></a>

## 🛡️ Protection status

| | macOS | Linux | Windows |
|---|---|---|---|
| Scripted and brokered agents confined | ✓ Seatbelt; label `descendants` | ✓ Landlock; labels `state-read`, `descendants` | ✓ AppContainer |
| Trusted checks confined | ✓ Seatbelt; same OS limits as above | ✓ Landlock; same OS limits as above | reduced (labelled) |
| Claude Code confined | reduced until T16.0 (default policy `claude-reduced-until-T16.0`) | reduced until T16.0 (same policy) | reduced until T16.0 (same policy) |
| Codex | observed (writes checked against the write set afterwards; default policy `observed-checked-afterwards`) | observed | observed |
| Local model (first-pass review) | ✓ confined, loopback only; label `descendants` | ✓ confined, loopback only; labels `state-read`, `descendants` | reduced (AppContainer has no loopback; default policy `windows-local-model-loopback`) |

*Reduced* runs are always labelled in the output, with the name of the policy or Decision that allowed
them. Trusted checks use Seatbelt or Landlock and share the same underlying OS limits, but check evidence currently records only whether the check ran confined, not the per-capability labels agents carry; a check that ran without OS confinement is labelled as such in the delivery output. A label `descendants` means cleanup ends an agent's attached child processes by walking the
process tree, but a daemon that has already detached from it can survive. A label `state-read` means
the OS cannot hide Kaisa's state directory from the process. Any other shortfall stops the run until
you accept it with `--accept-reduced` and `--accept-until`. `kaisa --confine-claude "<goal>"` tries a confined
Claude Code run; the steps to verify it on a machine with a logged-in CLI are in
[`docs/research/brokered-mode-spike.md`](docs/research/brokered-mode-spike.md#closing-a-row-t35).

<details>
<summary><b>What the sandbox hides, per OS</b></summary>

What the sandbox hides, as built:

- **macOS.** A confined process cannot read Kaisa's state directory (the ledger, other attempts'
  tokens and logs) and cannot reach the keychain services. It may read its own worktree, scratch and
  bridge files, and the metadata of the folders above them. One exception: a confined Claude Code agent
  keeps login-keychain access for its own OAuth login. That is access to the keychain service, not to one
  item; the output prints a `trial:` line and the ledger records it. Trusted checks and broker `exec`
  keep the keychain denied.
- **Linux.** Landlock leaves reads open, so a confined process can read Kaisa's state. The run is
  labelled `state-read`.
- **Windows.** AppContainer grants the agent's own worktree, attempt directory and scratch, plus the directories of the programs it may run through the shared programs capability (not revoked per attempt). The
  Windows code is compile-checked locally and tested by CI on Windows; it was not exercised by hand.

</details>

<details>
<summary><b>Child environment: what agents and checks inherit</b></summary>

**Child environment.** Agents, trusted checks and the local-model bridge start with an allow-listed
environment, not a copy of your shell's. It carries locale, `PATH` and home locations, the usual
toolchain locations (Cargo and Rustup, Node and its package managers, Go, Java, Python, Ruby, .NET,
Deno), TLS certificate settings, `TZ`, and the proxy variables (`HTTP_PROXY`, `HTTPS_PROXY`,
`ALL_PROXY`, `NO_PROXY` and the lowercase forms). When the egress broker is in use it replaces the
proxy variables and clears `NO_PROXY`. Each agent adapter adds only its own variables: Claude Code its
Anthropic, Bedrock, Vertex, Foundry and AWS inputs; Codex a fixed list of OpenAI and Codex names. By
default the base environment excludes `GIT_*`, `SSH_AUTH_SOCK`, `LD_*` and `DYLD_*`, `CARGO_TARGET_DIR`,
`RUSTFLAGS`, and another provider's keys. If your test command needs a variable that is not on the list, the check sees it
unset. A Codex setup with a custom provider names its extra variables in
`KAISA_PASS_ENV=OPENROUTER_API_KEY,AZURE_HEADER` (comma-separated names; Codex only; Kaisa does not read
them from Codex's configuration). `KAISA_PASS_ENV` forwards any syntactically valid name you list, including otherwise excluded ones such as `LD_PRELOAD` or `RUSTFLAGS`, so list only what you intend to pass.

</details>

<details>
<summary><b>Policy: the gates you cannot turn off</b></summary>

**Policy.** `kaisa policy show` and `kaisa policy set <key> <value>` manage your personal
`policy.json`. The keys `require_person_when_tests_weaken` and
`require_person_when_check_definitions_change` accept only `true`, because that gate is always
enforced; `set … false` exits `64`. A `policy.json` from an earlier version that has either key set to
`false` is refused, not obeyed. `kaisa policy show` (or `set`) prints the file, the key and the repair (edit that line to
`true`, or delete the file) and exits `1`; bare `kaisa policy` is a usage error (`64`); a goal stops before it starts, exit `1`, naming the key.

</details>

<a id="current-differences-from-the-design"></a>

## 🧭 Current differences from the design

[`docs/architecture.md`](docs/architecture.md) describes the target design. This is what the code does
today where the two differ; the full list is in [`docs/tasks/`](docs/tasks/).

<details>
<summary><b>Show the list</b></summary>

- **No daemon, keeper or control server.** `kaisa "<goal>"` runs the engine inside the CLI process.
  There is no `kaisa daemon`, `kaisa keeper` or control endpoint. A crashed run is cleaned up by the
  next `kaisa` start; a still-running agent is ended, not reattached.
- **Captured output, not a PTY.** Agents run with captured stdio. `kaisa wrap`, attach and steer do not
  exist.
- **Event-sourced ledger.** SQLite stores immutable specs, attempts, context packs and events; most state is folded from events. The outbox is built and tested but not on the run
  path. There is no single OS-level ledger writer.
- **Index and impact sets** are built but a run does not use them. Unpinned Claude verified goals record measured routing decisions; task impact signals remain approximate. Explicit agent/model launches keep their selections.
- **No hybrid retrieval.** Recall is lexical (BM25); there is no vector index and no "vector search
  off" label yet.
- **Localization is partial.** The TUI selects Korean or English labels; engine codes, recorded text and most CLI output retain their original language.
- **Missions** run explicit JSON DAGs through `--plan`; decomposition into tasks is supplied by the user. Dispatch uses deterministic waves, so a slow admitted sibling delays the next wave. Failure drains admitted siblings under their existing process deadlines; there is no interactive cancellation command.
- **Trusted checks** are derived from the repository's working tree when the run starts, not from the
  base revision.
- **Concurrent goals.** Several `kaisa` goals can run at once on one state directory: owner locks keep
  them from ending each other's agents, ledger writes wait for each other instead of failing, and
  attempt ids include the process id. A test runs eight writers against one ledger; there is still no
  single ledger-writer process.

</details>

<a id="planned"></a>

## 🗺️ Planned

- Packaged, signed releases per OS (FR-01), so installing does not need a Rust toolchain.
- `kaisa review` to approve a verified candidate and see exactly what approving will do.
- Provider failover (for example Claude to GPT when a quota runs out), budgets per run, user and team.
- `kaisa console` in the browser (the terminal TUI and `/kaisa` MCP goal launcher are implemented).
- Team policy packs, approval delegation and an audit log.
- Complete Korean and English coverage beyond the existing TUI labels.

## 💻 Platforms

macOS, Linux and Windows are all first-class targets and release blockers. Minimum OS versions for
full protection are listed in the
[architecture](docs/architecture.md#57-guard-policy-sandbox-and-egress).

<a id="documentation"></a>

## 📚 Documentation

| Document | Contents |
|---|---|
| [PRD](docs/prd.md) | Problem, users and requirements |
| [User flows](docs/user-flows.md) | Every way people use Kaisa: flows, failure cases and usability targets |
| [Architecture](docs/architecture.md) | Engine design, security model, cross-platform matrix, milestones and decisions |
| [Tasks](docs/tasks/) | Milestone breakdowns with the test behind each task: [M0](docs/tasks/M0.md), [M1](docs/tasks/M1.md) |
| [Test conventions](docs/testing.md) | How tests are named and written, and the checks before every commit |
| [Research](docs/research/) | Prior-art analysis of Orca, LearnHarness and ruflo, and platform spikes |
| [Reviews](docs/reviews/) | Independent reviews of the design, with their resolutions |

## 🔧 Building

Kaisa is written in Rust (edition 2024; the toolchain is pinned in `rust-toolchain.toml`) and follows
the [Google Rust style guide](https://google.github.io/styleguide/rustguide.html). It ships as one
binary, `kaisa`, with no Node, Python or Electron runtime.

```bash
cargo build                  # build everything
scripts/check.sh             # what CI runs for format, lint and tests (rustfmt stable and nightly,
                             # clippy, tests), plus Windows/Linux cross-checks; CI adds cargo-deny
scripts/check.sh --fix       # format first, then check
```

CI runs the format, lint and test steps on macOS, Linux and Windows, plus `cargo-deny` for licences and
advisories. It runs no benchmark or footprint job.

## 🧩 Principles

- **Measured claims only.** No benchmark number appears here before CI has measured it.
- **Ease of use first.** If a flow is hard to use, the architecture changes. The flow does not.
- **Honest labels.** Anything that runs with less protection than designed says so in its output.
- **Ideas, not code, from prior art.** Kaisa studies existing tools but does not copy their code.

## 🌌 Name

The name was simply chosen as a name. Kaisa is an independent project with no connection to any game,
character or trademark. The mark, a K whose lower stroke is a verification check inside a shield, and
the night-and-aurora palette (`#0D1626` night, `#2DD4BF` verified, `#8B7CF6` aurora, `#F5B841` waiting
for you, `#F0616D` not delivered) are original to this repository; the sources are in
[`docs/assets/`](docs/assets/).

## 📄 License

Kaisa is released under the [MIT License](LICENSE).

---

<div align="center">
  <img src="docs/assets/kaisa-icon.svg" alt="Kaisa" width="72">
  <br>
  <sub>Only verified work reaches you.</sub>
</div>


### Measured routing

Unpinned Claude verified goals start with the existing grade-based catalog order
at medium effort. Once a cell has enough completed task episodes, routing compares
measured cost per delivered result, including same-cell revisions, subject to the
delivery-rate target and optional p90 latency budget. Grade and declared floors
always apply. Failed measured attempts escalate model before effort; high requires
qualifying evidence, and maximum effort is never selected. Security-sensitive
tasks never explore. Explicit `--agent`, `--model` (developer) and scripted launches
keep their selections.

```sh
kaisa route explain last
kaisa route explain task_<id>
kaisa stats routing
kaisa policy set routing_min_samples 5
kaisa policy set routing_target_delivery_bps 8000
kaisa policy set routing_exploration_bps 500
kaisa policy set routing_latency_budget_ms none
kaisa policy set routing_window 100
kaisa policy set routing_seed 0
```

The numbers above are configurable policy defaults, not measured performance.
Statistics come from attributable ledger receipts. Unknown cost and latency stay
unknown; tokens are compared separately from USD. Cross-model escalation costs
are attributed to their own cells. Legacy attempts without route identity cannot
supply model statistics. See the [ADR-0032 addendum](docs/adr/0032-addendum-measured-routing.md)
for the exact scoring, deterministic exploration and current limits.
