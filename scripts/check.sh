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
cargo test --workspace --locked
if rustup target list --installed 2>/dev/null | grep -qx "x86_64-pc-windows-msvc"; then
  echo "== clippy (Windows cross-check, compile only)"
  cargo clippy --workspace --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings
fi
echo "== all checks passed"
