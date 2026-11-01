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
  echo "== clippy (Windows cross-check of the full workspace, compile only)"
  # As in the Linux cross-check below, use Rust bindings without cross-building native
  # SQLite/blake3. This checks daemon and CLI cfg branches too; Windows CI tests runtime.
  LIBSQLITE3_SYS_USE_PKG_CONFIG=1 SQLITE3_LIB_DIR=/usr/lib \
    cargo clippy --workspace --locked --all-targets --target x86_64-pc-windows-msvc \
      --features blake3/pure -- -D warnings
fi
if [[ "$(uname -s)" != "Linux" ]] && rustup target list --installed 2>/dev/null | grep -qx "x86_64-unknown-linux-gnu"; then
  echo "== clippy (Linux cross-check of the full workspace, compile only)"
  # Check the same workspace and targets as CI, including Linux-only daemon code. Pure
  # blake3 and SQLite's linked bindings avoid cross-building C/assembly: clippy emits no
  # executable, so no target SQLite library or linker is needed. Native CI still builds
  # and runs the bundled C code with the normal features.
  LIBSQLITE3_SYS_USE_PKG_CONFIG=1 SQLITE3_LIB_DIR=/usr/lib \
    cargo clippy --workspace --locked --all-targets --target x86_64-unknown-linux-gnu \
      --features blake3/pure -- -D warnings
fi
echo "== all checks passed"
