# F2b — follow-up to the F2 review

## Review follow-up

This addresses `target/kaisa-review/REVIEW-RF2.md` against fa743bf. The original F2 report remains at `target/kaisa-review/REVIEW-FIX.md`; this follow-up supersedes its statement that explicitly confined Claude cannot use the keychain. No real Claude/Codex process was run and no external network was used; Cargo ran offline.

- **B1:** readable paths now receive `file-read-metadata` allowances on each literal ancestor inside each protected state root. These expose neither directory listings nor file contents. Agent, trusted-check and broker-exec constructor regressions run `/bin/sh -c 'cd sub && pwd'` and Node `fs.realpathSync('.')` when Node is present. Each fails with `cd: sub: Not a directory` when the metadata allowance is removed (logs `target/kaisa-review/f2b-mutation-{agent,check,exec}.log`). The check constructor also preserves its separate temporary directory. Existing secret/token/symlink assertions remain in place, and an additional test denies ancestor directory listings.
- **S1:** Mach service denial covers exact legacy names and `^com[.]apple[.]security(d)?[.]`, including secd services. The native probe tests the real `com.apple.SecurityServer` and `com.apple.securityd.xpc` names, plus both legacy and data-protection `SecItemCopyMatching` queries for the same nonexistent fixture item. Restoring the old regex fails the real secd lookup assertion (`f2b-mutation-secd.log`). No real credential item was requested.
- **S2:** the Claude adapter declares `needs_keychain_login`; only a confined Claude agent receives the Seatbelt keychain exception. Trusted checks, broker exec and other adapters retain the denial. The macOS trial prints a one-line disclosure, and the execution-start event records `login_keychain_access` plus a human-readable notice. A copied **fake agent**, named `claude` solely to select the adapter, verifies the ledger disclosure and replay compatibility without invoking any real provider CLI. Constructor and native service tests verify the exception; dropping its propagation fails the constructor test (`f2b-mutation-claude.log`). State secrets stay denied even with the exception. The T35 procedure documents the exception. This permits keychain access, rather than item-scoped authorization: it cannot restrict the OS service to a particular credential.
- **S3:** README capability examples include `state-read` and explain that Decisions accepting only the old four codes no longer cover unconfined runs.
- **N1:** deny both environment HOME and the passwd account home, obtained with reentrant `getpwuid_r`; all unsafe code stays in kaisa-sys with SAFETY comments.
- **N2:** classify readable directory grants in `protect_state`, after canonicalization, instead of querying the filesystem during profile rendering. A path missing at construction remains an exact-file grant.
- **N3:** macOS exec sandbox setup errors degrade to `ExecSandbox::Unavailable`; the attempt server stays alive and exec fails closed. A subprocess regression removes HOME and KAISA_STATE_DIR without mutating the parent test environment.
- **N4 (optional):** new path-walking setup uses named command helpers. Existing cat closures and success-only cleanup in the three earlier constructor fixtures remain; a broader shared-fixture refactor is deferred.
- **N5 (optional):** Windows ACL grants and containment labels are unchanged, as requested. Its own attempt-directory exposure remains asymmetric with macOS and requires separate Windows work if that policy is tightened.

No platform-limit admission logic was added; integration with F3 remains the coordinator's responsibility.

## Reviewer's tool table

Replayed the preserved review harness and its dependency-free Node/Rust fixtures with the new profile. `target/kaisa-review/f2b-tool-table.log` records:

| Command in protected checkout | Result |
| --- | --- |
| `node test.js` | exit 0, `node test ok` |
| `npm test --offline` | exit 0, `node test ok` |
| `cd crate && cargo test --offline` | exit 0, one test passes |
| `/bin/sh -c 'cd crate && pwd'` | exit 0 |
| `cat <state>/kaisa.db` | exit 1, `Operation not permitted` |

The ignored harness was adjusted only for the new agent-constructor boolean argument. Production constructor tests, rather than the copied check harness alone, cover all three call sites.

## Validation

Focused daemon, MCP and adapter tests pass. Additional mutation checks also fail for the intended reason: broadening ancestor grants to `file-read*` exposes directory listings (`f2b-mutation-listing.log`); removing the passwd home leaves the real keychain unprotected under an overridden HOME (`f2b-mutation-home.log`); propagating the state lookup error takes down exec setup (`f2b-mutation-unavailable.log`); dropping the keychain capability from the execution notice loses the ledger disclosure (`f2b-mutation-ledger.log`); removing the CLI notice fails the output disclosure assertion (`f2b-mutation-output.log`); and ignoring the agent-only exception denies the actual keychain services (`f2b-mutation-login-service.log`). Every mutation was restored before final validation.

The first full validation attempt caught a collapsible-if lint, the next caught an unused-mut warning in the Windows compile check, and a later test-constant cleanup caught one wrong namespace in a test. These were corrected; the final check result is appended below.

Final command: `CARGO_NET_OFFLINE=true KAISA_STATE_DIR="$PWD/target/kaisa-review/check-state" scripts/check.sh` exited **0**, ending `== all checks passed`. Stable/nightly rustfmt, workspace clippy with warnings denied, all workspace tests (**664 passed, 0 failed, 6 ignored**, summed across binaries), and installed Windows/Linux compile checks passed. Log: `target/kaisa-review/f2b-check.log`.
