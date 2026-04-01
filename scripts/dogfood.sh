#!/usr/bin/env bash
set -euo pipefail

# Self-Testing Developer Loop — Launcher
# One command to set up, run the loop, and clean up.
#
# Usage:
#   ./scripts/dogfood.sh           # run the loop
#   ./scripts/dogfood.sh --setup   # just set up (don't start loop)
#   ./scripts/dogfood.sh --teardown # just tear down

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

case "${1:-run}" in
    --setup)
        "$SCRIPT_DIR/dogfood-setup.sh"
        exit 0
        ;;
    --teardown)
        "$SCRIPT_DIR/dogfood-teardown.sh"
        exit 0
        ;;
    run|"")
        ;;
    *)
        echo "Usage: $0 [--setup|--teardown]"
        exit 1
        ;;
esac

# Setup
"$SCRIPT_DIR/dogfood-setup.sh"

# Source the env file to get DOGFOOD_CLONE_DIR
source "$REPO_ROOT/.dogfood-env"
export DOGFOOD_CLONE_DIR

# Create a worktree for the Claude Code session to work in
BRANCH_NAME="dogfood/$(date +%Y%m%d-%H%M%S)"
WORKTREE_DIR="$REPO_ROOT/.claude/worktrees/$BRANCH_NAME"
git worktree add "$WORKTREE_DIR" -b "$BRANCH_NAME" main
cd "$WORKTREE_DIR"

echo ""
echo "=== Starting Claude Code loop ==="
echo "Working in worktree: $WORKTREE_DIR"
echo "Test clone:          $DOGFOOD_CLONE_DIR"
echo "Web UI:              http://localhost:47022"
echo ""

# Run the loop
# The prompt file is read from the worktree since it's a copy of the repo
claude -p "$(cat scripts/dogfood-prompt.md)" --allowedTools '*'

# Teardown (optional — user may want to keep the clone for inspection)
echo ""
read -p "Clean up test clone? [y/N] " -n 1 -r
echo ""
if [[ $REPLY =~ ^[Yy]$ ]]; then
    "$SCRIPT_DIR/dogfood-teardown.sh"
fi
