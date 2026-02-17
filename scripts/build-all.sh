#!/usr/bin/env bash
# build-all.sh — Build and install Midtown.
# Installs the daemon binary to ~/.cargo/bin/.
#
# Usage:
#   ./scripts/build-all.sh           # release build (default)
#   ./scripts/build-all.sh --debug   # debug build
set -euo pipefail

PROFILE="release"
BUILD_FLAGS="--release"
INSTALL_FLAGS=""

if [[ "${1:-}" == "--debug" ]]; then
    PROFILE="debug"
    BUILD_FLAGS=""
    INSTALL_FLAGS="--debug"
fi

echo "Building daemon ($PROFILE)..."
cargo build $BUILD_FLAGS

echo "Installing daemon binary..."
cargo install --path . $INSTALL_FLAGS

echo "Done. Midtown installed to ~/.cargo/bin/midtown"
