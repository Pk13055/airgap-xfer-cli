#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${PREFIX:-$ROOT}"
if [[ -n "${1:-}" ]]; then
  SRC="$1"
elif [[ -f "$ROOT/target/release/airgap-xfer" ]]; then
  SRC="$ROOT/target/release/airgap-xfer"
elif [[ -f "$ROOT/target/debug/airgap-xfer" ]]; then
  SRC="$ROOT/target/debug/airgap-xfer"
else
  SRC="$ROOT/target/release/airgap-xfer"
fi
if [[ ! -f "$SRC" ]]; then
  echo "missing binary: $SRC (build with: cargo build --release)" >&2
  exit 1
fi
mkdir -p "$PREFIX/bin"
install -m 755 "$SRC" "$PREFIX/bin/airgap-xfer"
echo "installed $PREFIX/bin/airgap-xfer"
