#!/usr/bin/env bash
# build-all.sh — Build and install Midtown.
# Installs the daemon binary to ~/.cargo/bin/.
#
# Usage:
#   ./scripts/build-all.sh           # release build (default)
#   ./scripts/build-all.sh --debug   # debug build
set -euo pipefail

PROFILE="release"
CARGO_FLAGS="--release"

if [[ "${1:-}" == "--debug" ]]; then
    PROFILE="debug"
    CARGO_FLAGS=""
fi

echo "Building daemon ($PROFILE)..."
cargo build $CARGO_FLAGS

echo "Installing daemon binary..."
cargo install --path . $CARGO_FLAGS

echo "Done. Midtown installed to ~/.cargo/bin/midtown"
