#!/usr/bin/env bash
set -euo pipefail

# Self-Testing Developer Loop — Launcher
# One command to set up, run the loop, and clean up.
#
# Usage:
#   ./scripts/dogfood.sh           # run (or resume) the loop
#   ./scripts/dogfood.sh --fresh   # force a fresh session (ignore saved state)
#   ./scripts/dogfood.sh --setup   # just set up (don't start loop)
#   ./scripts/dogfood.sh --teardown # just tear down

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
STATEFILE="$REPO_ROOT/.dogfood-state"

case "${1:-run}" in
    --setup)
        "$SCRIPT_DIR/dogfood-setup.sh"
        exit 0
        ;;
    --teardown)
        "$SCRIPT_DIR/dogfood-teardown.sh"
        rm -f "$STATEFILE"
        exit 0
        ;;
    --fresh)
        rm -f "$STATEFILE"
        ;;
    run|"")
        ;;
    *)
        echo "Usage: $0 [--fresh|--setup|--teardown]"
        exit 1
        ;;
esac

# Check for a previous worktree to resume in
WORKTREE_DIR=""
if [ -f "$STATEFILE" ]; then
    source "$STATEFILE"
    if [ -n "$WORKTREE_DIR" ] && [ -d "$WORKTREE_DIR" ]; then
        echo "=== Resuming in previous worktree ==="
        echo "Worktree:  $WORKTREE_DIR"
    else
        echo "Previous worktree gone, starting fresh."
        WORKTREE_DIR=""
        rm -f "$STATEFILE"
    fi
fi

# Setup the test clone if not already running
if [ ! -f "$REPO_ROOT/.dogfood-env" ]; then
    "$SCRIPT_DIR/dogfood-setup.sh"
else
    echo "Test clone already set up (run --teardown to reset)."
fi

# Source the env file to get DOGFOOD_CLONE_DIR and DOGFOOD_WEB_URL
source "$REPO_ROOT/.dogfood-env"
export DOGFOOD_CLONE_DIR DOGFOOD_WEB_URL

# Create a new worktree if we're not resuming
if [ -z "$WORKTREE_DIR" ]; then
    git fetch origin

    BRANCH_NAME="dogfood/$(date +%Y%m%d-%H%M%S)"
    WORKTREE_DIR="$REPO_ROOT/.claude/worktrees/$BRANCH_NAME"
    git worktree add "$WORKTREE_DIR" -b "$BRANCH_NAME" origin/main
fi

cd "$WORKTREE_DIR"

# Save worktree path for next run
echo "WORKTREE_DIR=$WORKTREE_DIR" > "$STATEFILE"

echo ""
echo "=== Starting Claude Code session ==="
echo "Working in worktree: $WORKTREE_DIR"
echo "Test clone:          $DOGFOOD_CLONE_DIR"
echo "Web UI:              $DOGFOOD_WEB_URL"
echo ""

# Launch or resume the Claude Code session.
# --dangerously-skip-permissions lets the agent work autonomously.
# --continue resumes the most recent session in this worktree directory.
# On first run, pass the entire prompt as a /loop command so it runs every 10m.
# On subsequent runs, --continue picks up where we left off (loop is already active).
PROMPT=$(cat "$SCRIPT_DIR/dogfood-prompt.md")
if [ -f "$WORKTREE_DIR/.claude/settings.local.json" ]; then
    claude --dangerously-skip-permissions --chrome --continue
else
    claude --dangerously-skip-permissions --chrome "/loop 10m $PROMPT"
fi
