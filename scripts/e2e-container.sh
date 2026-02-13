#!/usr/bin/env bash
# e2e-container.sh — Developer-facing wrapper for containerized E2E tests.
#
# Usage:
#   ./scripts/e2e-container.sh coordination          # fast, no auth needed
#   ./scripts/e2e-container.sh full                   # real Claude, needs auth
#   ANTHROPIC_API_KEY=sk-ant-... ./scripts/e2e-container.sh full
#   CLAUDE_AUTH_DIR=~/.midtown/claude-auth ./scripts/e2e-container.sh full
#
# Extra args after the mode are passed through to cargo test:
#   ./scripts/e2e-container.sh coordination -- --nocapture
set -euo pipefail

IMAGE_NAME="midtown-e2e"
DOCKERFILE="Dockerfile.e2e"
MODE="${1:-coordination}"
shift || true

# --- Locate repo root (where Dockerfile.e2e lives) ---
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

if [ ! -f "${REPO_ROOT}/${DOCKERFILE}" ]; then
    echo "ERROR: ${DOCKERFILE} not found in ${REPO_ROOT}"
    exit 1
fi

# --- Build the image if needed ---
echo "Building ${IMAGE_NAME} image..."
docker build -t "${IMAGE_NAME}" -f "${REPO_ROOT}/${DOCKERFILE}" "${REPO_ROOT}"
echo ""

# --- Assemble docker run args ---
DOCKER_ARGS=(
    --rm
    -e MIDTOWN_WEBHOOK_PORT=0
    -e MIDTOWN_CHAT_MONITOR=0
)

if [ "${MODE}" = "full" ]; then
    # Pass API key if set
    if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
        DOCKER_ARGS+=(-e ANTHROPIC_API_KEY)
    fi

    # Mount OAuth credentials directory if available
    # Check new location first (~/.midtown/auth/e2e/claude), then fall back to old locations
    if [ -z "${CLAUDE_AUTH_DIR:-}" ]; then
        if [ -d "${HOME}/.midtown/auth/e2e/claude" ]; then
            CLAUDE_AUTH_DIR="${HOME}/.midtown/auth/e2e/claude"
        elif [ -d "${HOME}/.midtown/auth/e2e" ]; then
            # Legacy: pre-restructure location
            CLAUDE_AUTH_DIR="${HOME}/.midtown/auth/e2e"
        else
            CLAUDE_AUTH_DIR="${HOME}/.midtown/claude-auth"
        fi
    fi
    if [ -d "${CLAUDE_AUTH_DIR}" ]; then
        DOCKER_ARGS+=(-v "${CLAUDE_AUTH_DIR}:/auth:ro" -e CLAUDE_CONFIG_DIR=/auth)
        echo "Mounting auth from: ${CLAUDE_AUTH_DIR}"
    elif [ -z "${ANTHROPIC_API_KEY:-}" ]; then
        echo "WARNING: Full mode requires auth. Set ANTHROPIC_API_KEY or run 'midtown auth login --profile e2e'"
        echo "Continuing anyway — the entrypoint will validate auth and fail with details."
        echo ""
    fi
fi

echo "Running E2E tests (mode: ${MODE})..."
echo ""

docker run "${DOCKER_ARGS[@]}" "${IMAGE_NAME}" "${MODE}" "$@"
