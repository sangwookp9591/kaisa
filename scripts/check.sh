#!/usr/bin/env bash
# Runs the same checks as CI, locally, and stops at the first failure.
# Usage: scripts/check.sh            (all checks)
#        scripts/check.sh --fix      (format first, then check)
set -euo pipefail

cd "$(dirname "$0")/.."

nightly_fmt_config="imports_granularity=Module,group_imports=StdExternalCrate"

if [[ "${1:-}" == "--fix" ]]; then
  cargo fmt --all
  cargo +nightly fmt --all -- --config "$nightly_fmt_config"
fi

echo "== rustfmt (stable)"
cargo fmt --all --check
echo "== rustfmt (nightly import granularity)"
cargo +nightly fmt --all -- --check --config "$nightly_fmt_config"
echo "== clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings
echo "== tests"
# The end-to-end tests run the test-only fake agent as a real process next to `kaisa`.
cargo build --locked -q -p kaisa-fake-agent
cargo test --workspace --locked --no-fail-fast
if rustup target list --installed 2>/dev/null | grep -qx "x86_64-pc-windows-msvc"; then
  # Only crates without C build steps: blake3 (assembly) and SQLite cannot be cross-compiled
  # for MSVC from macOS/Linux, so kaisa-ledger, kaisa-context, kaisa-mcp, kaisa-agents, kaisa-cli and the binary (which reach blake3) are left
  # out; their Windows-specific code lives in kaisa-sys, which is checked here. The real Windows
  # CI runner builds and tests everything.
  echo "== clippy (Windows cross-check of crates without C build steps, compile only)"
  cargo clippy --locked --all-targets --target x86_64-pc-windows-msvc \
    -p kaisa-sys -p kaisa-keeper -p kaisa-domain -p kaisa-protocol -p kaisa-workspace -p kaisa-supervisor -p kaisa-egress \
    -- -D warnings
  # blake3's C/assembly build cannot target MSVC from here; its pure-Rust build can. Crates that
  # also need bundled SQLite (ledger, daemon, cli) still rely on the Windows CI job.
  for crate in kaisa-mcp kaisa-context; do
    cargo clippy --locked --all-targets --target x86_64-pc-windows-msvc -p "$crate" --features blake3/pure \
      -- -D warnings
  done
fi
if [[ "$(uname -s)" != "Linux" ]] && rustup target list --installed 2>/dev/null | grep -qx "x86_64-unknown-linux-gnu"; then
  echo "== clippy (Linux cross-check of kaisa-sys and kaisa-mcp, compile only)"
  cargo clippy --locked --all-targets --target x86_64-unknown-linux-gnu -p kaisa-sys -p kaisa-mcp -- -D warnings
fi
echo "== all checks passed"
