#!/usr/bin/env bash
# Generate code coverage report for midtown
#
# Usage:
#   ./scripts/coverage.sh          # Generate HTML report
#   ./scripts/coverage.sh --text   # Print text summary
#   ./scripts/coverage.sh --lcov   # Generate lcov.info for CI
#
# Prerequisites:
#   cargo install cargo-llvm-cov
#   rustup component add llvm-tools-preview

set -euo pipefail

cd "$(dirname "$0")/.."

case "${1:-html}" in
  --text|-t)
    ./scripts/with-isolated-home.sh cargo llvm-cov --summary-only
    ;;
  --lcov|-l)
    ./scripts/with-isolated-home.sh cargo llvm-cov --lcov --output-path target/lcov.info
    echo "Coverage report: target/lcov.info"
    ;;
  --html|-h|html)
    ./scripts/with-isolated-home.sh cargo llvm-cov --html
    echo "Coverage report: target/llvm-cov/html/index.html"
    ;;
  --open|-o)
    ./scripts/with-isolated-home.sh cargo llvm-cov --html --open
    ;;
  *)
    echo "Usage: $0 [--text|--lcov|--html|--open]"
    exit 1
    ;;
esac
