# Offline specification workflow demo

Run from the Kaisa checkout (macOS; Rust, Git and Python 3 installed):

```sh
CARGO_NET_OFFLINE=true cargo build --locked -p kaisa -p kaisa-fake-agent
python3 examples/spec-demo/run.py --output target/spec-demo
```

The script creates a disposable standalone Rust repository from `repo/`. It drafts
an incomplete spec and asserts the R1 missing-criteria finding, drafts a corrected
version, shows it, explicitly opts into provisional approval, and runs both
candidates through production characterization, development, two private QA authors,
domain test review, staged review, and the deterministic acceptance gate.
The passing candidate gets a result branch; the failing candidate keeps accepting
empty input and gets FAIL with no result branch. The revision budget is explicitly
zero so the bad implementation is demonstrated without an automatic retry.
Only the test-only `kaisa-fake-agent` executable is selected; Cargo is offline.

Approval in this script is **provisional, NOT human-approved**. To demonstrate actual
terminal approval instead, run `kaisa spec approve spec_demo` in a real terminal
after drafting in a persistent repository/state directory, review the displayed
criteria, and type the displayed digest's first eight hex characters. Do not pipe
an answer or generate a human receipt in this script. The real PTY path is covered
by `kaisa_cli::spec::tests::a_real_pty_accepts_only_a_persons_correct_digest_and_rejects_agent_markers`.
Windows refuses human approval. Same-user PTY impersonation remains a trust limit.

Add `--measure-agent` to record wall time and RSS for all six agent stages; an observer copies the raw timing files before temporary QA scratch is removed. Each CLI operation records combined output and, on macOS, `/usr/bin/time -l`
wall time and peak RSS. `traceability.md` contains unmodified `kaisa spec report`
outputs; JSON is copied from the same runs' preserved `traceability.json` artifacts
(the CLI currently has no JSON-report flag). IDs, digests, timings and temporary
paths vary; fake answers and the asserted gate outcomes are deterministic.
`ledger-*.json` records event rows, including run-bound agent admissions.
The example integration test runs this script and checks private-QA rejection.
Native runtime claims in this example are limited to macOS.

The scripts also support the adversarial runs recorded in
[`docs/evidence/before-after.md`](../../docs/evidence/before-after.md).

```sh
python3 examples/spec-demo/before-after.py --binary target/debug/kaisa \
  --fake target/debug/kaisa-fake-agent --output target/comparison
python3 examples/spec-demo/run.py --attack exit-zero --output target/exit-zero
python3 examples/spec-demo/run.py --attack cargo-runner --output target/cargo-runner
python3 examples/spec-demo/run.py --attack build-rs-private-swap --output target/private-swap
```

The comparison script requires Node for Node patterns. It selects only explicit fake
programs and uses temporary state; it does not read or change provider credentials.
