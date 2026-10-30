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
