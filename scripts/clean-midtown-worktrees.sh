#!/usr/bin/env bash
# Clean Cargo build artifacts from all midtown worktrees.
#
# Usage:
#   ./scripts/clean-midtown-worktrees.sh [WORKTREES_DIR]
#
# Defaults to ~/.midtown/projects/midtown/worktrees when no directory is provided.
# Also checks the legacy path ~/.midtown/worktrees/midtown as a fallback.

set -euo pipefail

WORKTREES_DIR="${1:-}"

if [ -z "${WORKTREES_DIR}" ]; then
  # Try new path first, fall back to legacy
  if [ -d "${HOME}/.midtown/projects/midtown/worktrees" ]; then
    WORKTREES_DIR="${HOME}/.midtown/projects/midtown/worktrees"
  elif [ -d "${HOME}/.midtown/worktrees/midtown" ]; then
    WORKTREES_DIR="${HOME}/.midtown/worktrees/midtown"
  else
    echo "ERROR: No worktrees directory found"
    exit 1
  fi
fi

if [ ! -d "${WORKTREES_DIR}" ]; then
  echo "ERROR: Directory not found: ${WORKTREES_DIR}"
  exit 1
fi

for worktree in "${WORKTREES_DIR}"/*; do
  if [ -d "${worktree}" ] && [ -f "${worktree}/Cargo.toml" ]; then
    echo "cargo clean: ${worktree}"
    (cd "${worktree}" && cargo clean)
  fi
done
