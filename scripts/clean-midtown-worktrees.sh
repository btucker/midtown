#!/usr/bin/env bash
# Clean Cargo build artifacts from all midtown worktrees.
#
# Usage:
#   ./scripts/clean-midtown-worktrees.sh [WORKTREES_DIR]
#
# Defaults to ~/.midtown/worktrees/midtown when no directory is provided.

set -euo pipefail

WORKTREES_DIR="${1:-${HOME}/.midtown/worktrees/midtown}"

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
