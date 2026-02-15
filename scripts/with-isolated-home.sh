#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "Usage: $0 <command> [args...]"
  exit 2
fi

ORIGINAL_HOME="${HOME:-}"
if [ -z "${ORIGINAL_HOME}" ]; then
  echo "HOME must be set"
  exit 2
fi

if [ -n "${MIDTOWN_TEST_HOME:-}" ]; then
  ISOLATED_HOME="${MIDTOWN_TEST_HOME}"
  mkdir -p "${ISOLATED_HOME}"
  CLEANUP=0
else
  ISOLATED_HOME="$(mktemp -d "${TMPDIR:-/tmp}/midtown-test-home.XXXXXX")"
  CLEANUP=1
fi

cleanup() {
  if [ "${CLEANUP}" -eq 1 ]; then
    rm -rf "${ISOLATED_HOME}"
  fi
}
trap cleanup EXIT

# Keep Rust toolchain/cache locations stable while isolating user-state paths.
export CARGO_HOME="${CARGO_HOME:-${ORIGINAL_HOME}/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-${ORIGINAL_HOME}/.rustup}"
export HOME="${ISOLATED_HOME}"

# Preserve global git identity/behavior needed by tests that create commits.
if [ -f "${ORIGINAL_HOME}/.gitconfig" ] && [ ! -f "${HOME}/.gitconfig" ]; then
  cp "${ORIGINAL_HOME}/.gitconfig" "${HOME}/.gitconfig"
fi

exec "$@"
