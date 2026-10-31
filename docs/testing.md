# Test conventions

Every test in Kaisa follows three rules (PRD NFR-06). Reviewers reject tests that break them.

## Rule 1 — The name states a condition and an expectation

A test name is a sentence in `snake_case`. It says **when** something happens and **what** must
follow. A reader should understand the behaviour from the name alone.

| ✗ Rejected | ✓ Accepted |
|---|---|
| `parses_valid_ids` | `a_run_id_with_a_lowercase_suffix_round_trips_through_display` |
| `rejects_bad_suffixes` | `a_suffix_longer_than_the_maximum_is_rejected_as_bad_length` |
| `chain_test` | `editing_a_recorded_event_breaks_the_chain_at_that_event` |

Practical shape: `<situation>_<verb: is / returns / fails / keeps / rejects …>_<outcome>`.

## Rule 2 — The setup speaks the domain, not raw values

Build test inputs with named helpers, builders or fixtures that use domain words, such as
`a_finished_attempt()`, `RunGoal::fix_flaky_test()` or `ledger_with_events(&[run_started(), attempt_admitted()])`.
A reader should not have to decode string literals, magic bytes or anonymous tuples.

| ✗ Rejected | ✓ Accepted |
|---|---|
| `Event::new(3, "x", vec![1, 2, 3])` | `run_started_event(goal_fix_login_test())` |
| `"run_" + "a".repeat(33)` | `run_id_text_with_suffix_of_length(MAX_SUFFIX_LEN + 1)` |

Literals are allowed inside these helpers, because that is where they get a name.

## Rule 3 — Assertions have no unexplained numbers

Every number in an assertion is a named constant, or it is derived from the setup.

| ✗ Rejected | ✓ Accepted |
|---|---|
| `assert_eq!(status, 2)` | `assert_eq!(status, EXIT_NOT_IMPLEMENTED, …)` |
| `assert_eq!(ledger.len(), 3)` | `assert_eq!(ledger.len(), recorded_events.len(), …)` |
| `assert_eq!(seq, 1)` | `assert_eq!(seq, Sequence::FIRST, …)` |

Every assertion also carries a message that explains *why* the expectation must hold (Google Rust
style guide: "context strings in `assert!`").

## Layout

- Unit tests live in a `#[cfg(test)] mod tests` block next to the code.
- Shared test vocabulary lives in a `test_support` module (`#[cfg(test)]`) or a `kaisa-testkit` crate
  once more than one crate needs it.
- Integration tests live in `tests/` with the same naming rules.

## Platform-specific tests

A test that only runs on some platforms is gated with `#[cfg(...)]`, and so is every constant or
helper only that test uses; otherwise the other platforms fail on an unused item. The binary's
integration tests are not part of the local Windows cross-check (SQLite needs a C build), so CI is
where such a miss shows up.

## Security tests are mutation-checked

A test that guards a security or correctness property must fail when the guard is removed. Before
committing such a test, remove or weaken the guard, run the test, and confirm it fails **for the
reason the test names**; then restore the guard. Examples: confining an observed agent must break the
observed-run test; dropping the write-set check after an observed run must let the outside write
through.

## Integration tests need the fake agent built first

The binary's end-to-end tests start the scripted agent `kaisa-fake-agent` as a separate program. Cargo
does not build it for `cargo test -p kaisa --test <name>`, so run `cargo build -p kaisa-fake-agent`
first. Without it those tests fail with errors such as `sandbox-exec: execvp() of …/kaisa-fake-agent
failed` (for example, 7 of the 9 tests in `--test goal`). `scripts/check.sh` already builds it.

## Before every commit

Run `scripts/check.sh` (or `scripts/check.sh --fix` to format first). It runs the same format, lint and
test steps as CI: stable and nightly rustfmt, clippy with `-D warnings` and the tests, plus compile-only
cross-checks for Windows and Linux that CI does not need because it builds natively. CI also runs
`cargo-deny` and a smoke run of the binary, which `scripts/check.sh` does not. It stops at the first
failure. There is no benchmark or footprint step in either (architecture §15, §16).

Capture its exit code and commit only on zero, for example
`scripts/check.sh --fix > check.log 2>&1; ec=$?`. Never pipe it into another command before an
`&&`-chained commit, because the pipe hides the failure. Stage explicit paths, never `git add -A`.
