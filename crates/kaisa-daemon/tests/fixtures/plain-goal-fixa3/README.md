# Plain-goal FIXA3 output fixtures

`n-spec-reporter.txt` and `n-two-node-runs.txt` were captured offline by running
`npm test` against the original 57-goal review's respective Node projects on
2026-11-01 (macOS arm64). The npm scripts are printed in the captured output.

The two nextest files preserve the reviewer's realistic scripted output: one
suppresses passing status lines, and one contains a failed first attempt followed
by a successful retry. No real nextest installation was available. Durations and
run IDs are incidental; regressions inspect completion counts and start counts.

`libtest-child-probes.txt` is the libtest header/result excerpt from the retained
workspace replay's sandbox tests. Child probes deliberately die before their
summaries; the enclosing 63-test harness reports successful completion. Unrelated
diagnostic stdout and most named test lines are omitted from this excerpt.
