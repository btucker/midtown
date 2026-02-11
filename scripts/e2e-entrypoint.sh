#!/usr/bin/env bash
# e2e-entrypoint.sh — In-container test orchestrator for Midtown E2E tests.
#
# Modes:
#   coordination (default) — Runs daemon/tmux/nudge/channel/task E2E tests.
#                            No Claude auth needed; uses a stub lead command.
#   full                   — Runs coordination tests first, then full_stack_e2e
#                            tests that exercise real Claude Code integration.
#                            Requires ANTHROPIC_API_KEY or CLAUDE_CONFIG_DIR.
#
# Test suites run in parallel where safe. Each suite uses unique resources
# (PID-based names, unique sockets, dynamic ports) so different suites don't
# conflict. Suites that use tmux need --test-threads=1 within themselves
# but can still run concurrently with other suites.
set -euo pipefail

MODE="${1:-coordination}"
shift || true  # consume mode arg; remaining args pass through to cargo test

echo "=== Midtown E2E Test Runner ==="
echo "Mode: ${MODE}"
echo "Rust: $(rustc --version)"
echo "tmux: $(tmux -V)"
echo ""

# --- Git config (tests create repos and need an author identity) ---
git config --global user.email "e2e@midtown.test"
git config --global user.name "Midtown E2E"
git config --global init.defaultBranch main

# --- Start tmux server (tests expect it) ---
tmux start-server
echo "tmux server started"

# --- Parallel job management ---
# Track background PIDs and their labels for error reporting.
declare -a PIDS=()
declare -a LABELS=()
FAILED=0

# Launch a test suite in the background, capturing output to a temp file.
run_bg() {
    local label="$1"; shift
    local logfile
    logfile=$(mktemp "/tmp/e2e-${label}-XXXXXX.log")
    echo "[parallel] starting: ${label}"
    (
        echo "=== ${label} ===" >> "${logfile}"
        "$@" >> "${logfile}" 2>&1
    ) &
    PIDS+=($!)
    LABELS+=("${label}:${logfile}")
}

# Wait for all background jobs. Print output for failures.
wait_all() {
    local i=0
    for pid in "${PIDS[@]}"; do
        local entry="${LABELS[$i]}"
        local label="${entry%%:*}"
        local logfile="${entry#*:}"
        if wait "${pid}"; then
            echo "[parallel] passed:  ${label}"
        else
            echo ""
            echo "[parallel] FAILED:  ${label}"
            echo "--- output from ${label} ---"
            cat "${logfile}"
            echo "--- end ${label} ---"
            echo ""
            FAILED=1
        fi
        rm -f "${logfile}"
        i=$((i + 1))
    done
    PIDS=()
    LABELS=()
}

# --- Coordination tests (no auth required) ---
run_coordination_tests() {
    echo ""
    echo "=== Running coordination E2E tests (parallel) ==="

    local test_args=("$@")

    # Wave 1: Independent suites run concurrently.
    # Each suite uses unique PID-based resource names so they don't conflict.
    # Suites with tmux need --test-threads=1 *within* themselves but can
    # overlap with other suites safely.

    run_bg "daemon_e2e" \
        cargo test --release --test daemon_e2e -- --ignored --test-threads=1 "${test_args[@]}"

    run_bg "tmux_e2e" \
        cargo test --release --test tmux_e2e -- --ignored --test-threads=1 \
            --skip test_lead_pane_width_stable_across_reinits \
            --skip test_setup_chat_pane_is_idempotent \
            --skip test_spawn_claude_with_initial_prompt_renders_tui \
            "${test_args[@]}"

    run_bg "nudge_delivery_e2e" \
        cargo test --release --test nudge_delivery_e2e -- --ignored "${test_args[@]}"

    run_bg "chat_e2e" \
        cargo test --release --test chat_e2e -- --ignored "${test_args[@]}"

    run_bg "task_sharing" \
        cargo test --release --test task_sharing -- "${test_args[@]}"

    run_bg "mailbox_e2e" \
        cargo test --release --test mailbox_e2e -- "${test_args[@]}"

    run_bg "mailbox_e2e_daemon" \
        cargo test --release --test mailbox_e2e -- --ignored --test-threads=1 \
            --skip test_real_claude \
            --skip test_mailbox_fallback \
            "${test_args[@]}"

    wait_all

    if [ "${FAILED}" -ne 0 ]; then
        echo ""
        echo "=== Coordination tests FAILED ==="
        exit 1
    fi

    echo ""
    echo "=== Coordination tests complete ==="
}

# --- Full tests (requires Claude auth) ---
run_full_tests() {
    echo ""
    echo "=== Running full E2E tests (real Claude) ==="

    # Validate auth is available
    if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
        echo "Auth: ANTHROPIC_API_KEY is set"
    elif [ -n "${CLAUDE_CONFIG_DIR:-}" ] && [ -d "${CLAUDE_CONFIG_DIR}" ]; then
        echo "Auth: CLAUDE_CONFIG_DIR=${CLAUDE_CONFIG_DIR}"
    else
        echo "ERROR: Full mode requires either ANTHROPIC_API_KEY or CLAUDE_CONFIG_DIR with OAuth credentials."
        echo "  export ANTHROPIC_API_KEY=sk-ant-..."
        echo "  or mount credentials: -v /path/to/auth:/auth -e CLAUDE_CONFIG_DIR=/auth"
        exit 1
    fi

    local test_args=("$@")

    # Full-stack tests run in parallel with each other.
    run_bg "full_stack_e2e" \
        cargo test --release --test full_stack_e2e -- --ignored --test-threads=1 "${test_args[@]}"

    run_bg "mailbox_e2e_claude" \
        cargo test --release --test mailbox_e2e -- --ignored --test-threads=1 \
            --skip test_spawn_creates \
            --skip test_daemon_delivers \
            "${test_args[@]}"

    wait_all

    if [ "${FAILED}" -ne 0 ]; then
        echo ""
        echo "=== Full E2E tests FAILED ==="
        exit 1
    fi

    echo ""
    echo "=== Full E2E tests complete ==="
}

# --- Main ---
case "${MODE}" in
    coordination)
        run_coordination_tests "$@"
        ;;
    full)
        # Run coordination first for fast feedback, then full
        run_coordination_tests "$@"
        run_full_tests "$@"
        ;;
    *)
        echo "Unknown mode: ${MODE}"
        echo "Usage: $0 [coordination|full] [-- extra cargo test args]"
        exit 1
        ;;
esac

echo ""
echo "=== All requested E2E tests passed ==="
