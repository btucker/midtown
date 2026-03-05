#!/usr/bin/env bash
# Generate branch-based coverage diff reports.
#
# Usage:
#   ./scripts/coverage-diff.sh
#   ./scripts/coverage-diff.sh --base origin/main
#   ./scripts/coverage-diff.sh --base "$(git merge-base HEAD origin/main)" --out target/diff-coverage
#
# Prerequisites:
#   cargo install cargo-llvm-cov
#   pip install diff-cover
#   rustup component add llvm-tools-preview

set -euo pipefail

cd "$(dirname "$0")/.."

BASE_BRANCH="origin/main"
OUTPUT_DIR="target/diff-coverage"
LCOV_PATH="${OUTPUT_DIR}/lcov.info"
HTML_REPORT="${OUTPUT_DIR}/index.html"
JSON_REPORT=""
FETCH_BASE=1

usage() {
  cat <<'EOF'
Usage: scripts/coverage-diff.sh [options]

Options:
  --base BRANCH      Base branch/commit to diff against (default: origin/main)
  --out DIR          Output directory for reports (default: target/diff-coverage)
  --json-report FILE Write JSON report to FILE inside output dir
  --no-fetch         Skip fetching base branch if missing locally
  -h, --help         Show this message

Examples:
  scripts/coverage-diff.sh
  scripts/coverage-diff.sh --base origin/main
  scripts/coverage-diff.sh --base "$(git merge-base HEAD origin/main)" --out target/diff-coverage
EOF
}

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --base)
      BASE_BRANCH="${2:-}"
      shift 2
      ;;
    --out)
      OUTPUT_DIR="${2:-}"
      shift 2
      ;;
    --json-report)
      JSON_REPORT="${2:-}"
      shift 2
      ;;
    --no-fetch)
      FETCH_BASE=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1"
      usage
      exit 1
      ;;
  esac
done

if [[ -z "${BASE_BRANCH}" || -z "${OUTPUT_DIR}" ]]; then
  echo "base and output arguments must be non-empty"
  exit 1
fi

LCOV_PATH="${OUTPUT_DIR}/lcov.info"
HTML_REPORT="${OUTPUT_DIR}/index.html"

if ! command -v cargo >/dev/null; then
  echo "cargo not found in PATH."
  exit 1
fi

if ! command -v diff-cover >/dev/null; then
  echo "diff-cover not found in PATH. Install with: pip install diff-cover"
  exit 1
fi

if ! command -v cargo-llvm-cov >/dev/null; then
  echo "cargo-llvm-cov not found in PATH. Install with: cargo install cargo-llvm-cov"
  exit 1
fi

if [[ "${FETCH_BASE}" -eq 1 ]] && ! git cat-file -e "${BASE_BRANCH}^{commit}" 2>/dev/null; then
  git fetch --all --prune
fi

if ! git cat-file -e "${BASE_BRANCH}^{commit}" 2>/dev/null; then
  echo "Base ref '${BASE_BRANCH}' does not exist locally."
  echo "Provide a valid branch/commit or rerun with --no-fetch after resolving the ref manually."
  exit 1
fi

mkdir -p "${OUTPUT_DIR}"

# --lib: restrict to library unit tests (skip integration test binaries)
# --skip sandbox: sandbox tests depend on the real HOME and fail under with-isolated-home.sh
./scripts/with-isolated-home.sh cargo llvm-cov --lib --lcov --output-path "${LCOV_PATH}" -- --skip sandbox

DIFF_COVER_ARGS=(
  "${LCOV_PATH}"
  --compare-branch "${BASE_BRANCH}"
  --html-report "${HTML_REPORT}"
)
if [[ -n "${JSON_REPORT}" ]]; then
  DIFF_COVER_ARGS+=(--json-report "${OUTPUT_DIR}/${JSON_REPORT}")
fi

diff-cover "${DIFF_COVER_ARGS[@]}"

echo ""
echo "HTML report: ${HTML_REPORT}"
