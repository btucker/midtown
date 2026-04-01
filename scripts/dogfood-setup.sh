#!/usr/bin/env bash
set -euo pipefail

# Self-Testing Developer Loop — Setup
# Creates a test clone of the Midtown repo and starts a daemon in it.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Generate a unique clone directory
CLONE_ID="dogfood-$$"
CLONE_DIR="/tmp/midtown-${CLONE_ID}"

echo "=== Midtown Dogfood Setup ==="
echo "Repo root:  $REPO_ROOT"
echo "Clone dir:  $CLONE_DIR"
echo ""

# Step 1: Build the midtown binary from the current source
echo "[1/4] Building midtown..."
cd "$REPO_ROOT"
cargo install --path . 2>&1 | tail -5

# Step 2: Clone the repo
echo "[2/4] Cloning repo to $CLONE_DIR..."
if [ -d "$CLONE_DIR" ]; then
    echo "  Clone already exists, removing..."
    rm -rf "$CLONE_DIR"
fi
git clone "$REPO_ROOT" "$CLONE_DIR"

# Step 3: Initialize the clone as a project
echo "[3/4] Starting daemon in test clone..."
cd "$CLONE_DIR"
midtown start

# Step 4: Write the clone dir to a file for the prompt to read
ENVFILE="$REPO_ROOT/.dogfood-env"
echo "DOGFOOD_CLONE_DIR=$CLONE_DIR" > "$ENVFILE"
echo ""
echo "=== Setup complete ==="
echo "Clone dir: $CLONE_DIR"
echo "Web UI:    http://localhost:47022"
echo "Env file:  $ENVFILE"
echo ""
echo "To start the loop:"
echo "  export DOGFOOD_CLONE_DIR=$CLONE_DIR"
echo "  claude -p \"\$(cat scripts/dogfood-prompt.md)\" --allowedTools '*'"
