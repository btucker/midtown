#!/usr/bin/env bash
# build-all.sh — Build and install the Midtown daemon and Zellij WASM plugin.
# Installs the daemon binary to ~/.cargo/bin/ and the plugin to ~/.midtown/plugins/.
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

# Ensure WASM target is installed
if ! rustup target list --installed | grep -q wasm32-wasip1; then
    echo "Installing wasm32-wasip1 target..."
    rustup target add wasm32-wasip1
fi

echo "Building daemon ($PROFILE)..."
cargo build $CARGO_FLAGS

echo "Building Zellij plugin ($PROFILE)..."
cargo build $CARGO_FLAGS -p midtown-zellij-plugin --target wasm32-wasip1

echo "Installing daemon binary..."
cargo install --path . $CARGO_FLAGS

echo "Installing plugin WASM..."
PLUGIN_DIR="${HOME}/.midtown/plugins"
mkdir -p "$PLUGIN_DIR"
cp "target/wasm32-wasip1/${PROFILE}/midtown_zellij_plugin.wasm" "$PLUGIN_DIR/"

echo "Done. Daemon installed to ~/.cargo/bin/midtown, plugin to ${PLUGIN_DIR}/midtown_zellij_plugin.wasm"
