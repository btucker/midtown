#!/usr/bin/env bash
set -euo pipefail

# Self-Testing Developer Loop — Teardown
# Stops the daemon and removes the test clone.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENVFILE="$REPO_ROOT/.dogfood-env"

if [ ! -f "$ENVFILE" ]; then
    echo "No .dogfood-env file found. Nothing to tear down."
    exit 0
fi

source "$ENVFILE"

echo "=== Midtown Dogfood Teardown ==="
echo "Clone dir: $DOGFOOD_CLONE_DIR"
echo ""

# Step 1: Stop the daemon
if [ -d "$DOGFOOD_CLONE_DIR" ]; then
    echo "[1/2] Stopping daemon..."
    cd "$DOGFOOD_CLONE_DIR"
    midtown stop 2>/dev/null || true
fi

# Step 2: Remove the clone
echo "[2/2] Removing test clone..."
rm -rf "$DOGFOOD_CLONE_DIR"

# Clean up env file
rm -f "$ENVFILE"

echo ""
echo "=== Teardown complete ==="
