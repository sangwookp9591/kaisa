> Independent usability review of user-flows v0.1 by Codex `gpt-6.1-sol` (effort medium), run via Orca supervised worker on 2026-10-30. Read-only, no network. Applied in user-flows v0.2.

# Kaisa usability review

Scope: user-flows.md v0.1; architecture v0.4 §§4, 5.7, 5.11 only. Document review, not executable testing. Counts below estimate explicit decisions/prompts, excluding command entry; “?” means unspecified. N=new developer, L=busy lead, W=Windows-only, A=admin. Three-verb verdict: Y=covered naturally; P=partial; X=requires another command. Specialist exceptions are reasonable if discoverable through Home/watch/review.

## Blocker

- **F19m:** “won't start a duplicate” conflicts with [Start fresh] while start status is unknown. Disable replacement until exit/containment is proven; show preserved work and safe diagnostics. Cancel must also explain uncertain stop status.
- **F05/F07/F13/F14/F19f/j/n:** F05 correctly prohibits requester approval, but F13 accepts in-agent answers without separating advice from Gate resolution. Route policy, budget, plan-Gate and reduced-protection decisions to authenticated human review; show “waiting for you” in-agent. Never treat requester text as approval (§5.7).
- **F02/F19n:** “remembered” reduced protection and [Continue] obscure the required human Gate. State exact missing protection, scope and duration; require human authorization and keep the warning visible (§5.7).

## Major

- **F06/F08/F16:** “Approve” can merge or open a PR depending on hidden settings. Preview the exact effect, branch, destination and required authority; distinguish approving a candidate from approving publication. F06 shows a failed row while calling failures hidden. Provide an explicit collapsed count and eligibility reasons.
- **F15/F19b/d/l:** Freezing does not establish zero billing; reboot is not daemon restart. Replace absolute cost/survival promises with observed process state, possible outstanding charges, preserved checkpoint and confirmed restart choices. Silence alone must not trigger duplicate execution.
- **F17/F18/F30:** “Everything can be undone” exceeds §5.11: push has no inverse and undo selects a reversible effect. Preview effect, limitations and recovery deadline; distinguish hand-back `release` from cleanup. Uninstall must resolve active runs before deleting state.

### Every-flow assessment

Each row gives next-action clarity, decision cost, jargon/state gap and a concrete fix.

| Flow | Next; cost; verbs | Perspective and fix |
|---|---|---|
| F01 | Partial; 1+; X | N/W: placeholder Linux URL; per-user PATH unspecified. Supply executable install and shell-reopen instructions. |
| F02 | Partial; 1+; P | N: planner/model/budget choices precede a useful goal. Explain defaults, billing and what Start starts. |
| F03 | Partial; 1–3+; X | N/A: Fix/Skip/Apply overlap; explain blocked Skip and preserve existing configuration. |
| F04 | Clear; 0+; Y | N: “tier-0” is internal jargon; end with an explicit review action. |
| F05 | Clear; 0+; P | N: approval leaves session; provide a clickable human-review handoff and return status. |
| F06 | Partial; 1+; Y | L: recommendation lacks rationale; show acceptance criteria, eligibility and effect before selection. |
| F07 | Partial; 1+; P | N/L: editing DAG-like steps has no failure path; validate edits and show integration ownership. |
| F08 | Partial; 0–2+; P | L/A: review link destination and credentials unspecified; name reviewer, authentication and duplicate-label behavior. |
| F09 | Clear; 0; X | N: route question-shaped goals to research automatically; show cost ceiling. |
| F10 | Clear; 0; Y | L: many cards lack priority; sort by needs-you and expose actionable filters. |
| F11 | Partial; 2+; X | N: attempt IDs/observed mode unexplained; add Take over/Hand back buttons and trust warning. |
| F12 | Partial; 1; P | N: acknowledged need not mean followed; show applied/rejected outcome and recovery. |
| F13 | Partial; 1/question; P | L: no timeout/absent-owner state; assign respondent and offer defer/default escalation. |
| F14 | Partial; 1+presence; P | A: Always widens persistent policy; preview scope, expiry and whether org rules permit it. |
| F15 | Partial; 0–1; X | W/N: distinguish paused, stopping, stopped and unknown; expose controls in watch. |
| F16 | Partial; 1+presence; Y | L: receipt hash is jargon; prioritize changed behavior, evidence and exact approval effect. |
| F17 | Partial; 1?; X | N: show undo preview and expired/missing-receipt alternatives. |
| F18 | Partial; 0–1; X | L: unspecified cleanup delay; show retention countdown and Keep in review. |
| F19 | Partial; varies; P | All: standardize blocker cards with owner, safe default and return-to-watch action. |
| F20 | Partial; ?; X | L/A: delegation has no workflow; show scope, expiry, revocation and pending-review reassignment. |
| F21 | Partial; ?; P | N/A: entry and opt-in absent; use ask, explain enablement and stale-note correction. |
| F22 | Partial; 1+?; X | A: signing bootstrap/update failure absent; add preview, validation, staged rollout and rollback. |
| F23 | Unclear; ?; X | A: no setting/export command; provide budget editor, precedence and subscription-cost explanation. |
| F24 | Partial; ?; X | A: audit failure/offboarding unfinished; show incident actions, ownership transfer and access revocation receipt. |
| F25 | Unclear; ?; X | A: catalog/DAG jargon and no validation path; provide template wizard, test and approval preview. |
| F26 | Partial; 0 or blocked; X | A/L: Gate destination ambiguous; define exit status, approval link and continuation semantics. |
| F27 | Unclear; ?; P | L: cross-repo partial success absent; show per-repo approvals, dependencies and recovery. |
| F28 | Partial; 1+?; X | W: automatic system change is surprising; preview privilege/restart needs and decline fallback. |
| F29 | Partial; ?; X | N: sandbox jargon hides difference from normal session; show protection, exit and recovery controls. |
| F30 | Partial; 1+?; X | W/A: binary/service removal unspecified; show platform steps and preserved-work access afterward. |

### F19 failure-table audit

All are partial three-verb coverage unless watch/review exposes their actions. Estimated decisions and concrete fixes:

- **a (1+):** explain test failure in words; price retry.
- **b (0→1):** prove old worker stopped before replacement.
- **c (0–1):** explain provider choice; avoid unnecessary switch-back churn.
- **d (0–1):** disclose context loss and renewed verification.
- **e (0–1):** show retry time and cancel option.
- **f (1+):** show needed file operation and narrower alternative; human-only Gate.
- **g (1):** distinguish legitimate test edits from tampering; explain appeal.
- **h (0–1):** separate base movement from conflict; offer human conflict resolution.
- **i (1+):** offer copyable OS-specific login and confirmed recheck.
- **j (1):** disable budget-overrunning “finish” without authorized extension.
- **k (1):** preview deletion and reclaimable bytes; handle insufficient space.
- **l (1+):** name each unknown attempt and safe next action.
- **m (1+):** remove unsafe fresh-start shortcut while status unknown.
- **n (1+presence):** explain actual protection and refusal alternative.
- **o (1+):** show approver, request status and denied/expired outcome.
- **p (0):** add manual retry, cancel and reconnection status.

## Minor

- **F04/F06, §7:** `--agent` versus listed `--agents`; define singular alias or standardize.
- **F07/F25:** `--team feature` versus plan/templates terminology; distinguish team from workflow and clarify `--plan` semantics.
- **F11/F18:** `release` both exists and is “never” needed; label hand-back explicitly.
- **F28/§9:** Windows M3 conflicts with M0 tests on all OSes; specify baseline versus optional integration. Companion header says v0.3 although deltas reference v0.4.

## Missing scenarios

- **F02/F04:** dirty/non-git repositories, submodules and unavailable dependencies: preview copied state and remediation.
- **F10/F13:** notifications disabled, inaccessible TUI or user absent: provide persistent inbox and accessible text mode.
- **F08/F20:** reviewer unavailable or requester is sole reviewer: define eligible human reassignment.
- **F19/F22:** proxy/certificate failure, invalid policy signature and revoked credentials: explain trusted recovery.
- **F27/F30:** partial external effects and departed owners: preserve receipts and assign recovery owner.

## Top-10

1. F19m: eliminate uncertain duplicate starts.
2. F13/F14: visibly separate answers from human approvals.
3. F16: preview exact approval effect.
4. F02: guide one useful goal before advanced choices.
5. F10/F19: make every blocker actionable in watch.
6. F15: show honest process and billing states.
7. F17/F18: preview undo and retention limits.
8. F28: provide complete non-admin Windows path.
9. F20–F25: supply executable admin journeys.
10. F04/F09/F11: expose exceptional commands through the three-verb surfaces.
