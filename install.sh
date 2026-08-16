#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${PREFIX:-$ROOT}"
DEST="$PREFIX/bin/airgap-xfer"
if [[ -n "${1:-}" ]]; then
  SRC="$1"
elif [[ -f "$ROOT/target/release/airgap-xfer" ]]; then
  SRC="$ROOT/target/release/airgap-xfer"
elif [[ -f "$ROOT/target/debug/airgap-xfer" ]]; then
  SRC="$ROOT/target/debug/airgap-xfer"
else
  SRC="$ROOT/bin/airgap-xfer"
fi
if [[ ! -f "$SRC" ]]; then
  echo "missing binary: $SRC (copy bin/airgap-xfer or build with: cargo build --release)" >&2
  exit 1
fi
mkdir -p "$PREFIX/bin"
if [[ "$SRC" -ef "$DEST" ]]; then
  chmod 755 "$DEST"
else
  install -m 755 "$SRC" "$DEST"
fi
echo "installed $DEST"
