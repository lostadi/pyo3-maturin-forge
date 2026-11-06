#!/usr/bin/env bash
set -euo pipefail

# Bash wrapper for the Rust CLI. Build once, then forward commands.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$ROOT/target/release/pyo3-maturin-forge"

if [[ ! -x "$BIN" ]]; then
  echo "+ cargo build --release"
  (cd "$ROOT" && cargo build --release)
fi

exec "$BIN" "$@"
