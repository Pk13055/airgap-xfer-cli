#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${PREFIX:-/usr/local}"
SRC="${1:-$ROOT/target/release/airgap-xfer}"
if [[ ! -f "$SRC" ]]; then
  echo "missing binary: $SRC (build with: cargo build --release)" >&2
  exit 1
fi
mkdir -p "$PREFIX/bin"
install -m 755 "$SRC" "$PREFIX/bin/airgap-xfer"
echo "installed $PREFIX/bin/airgap-xfer"
