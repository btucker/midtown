# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Midtown is a multi-Claude Code workspace manager. It coordinates a Lead (human-facing Claude Code in tmux) and multiple autonomous Coworkers (headless Claude Code sessions), each running in isolated git worktrees. Communication happens through an IRC-style append-only channel log.

## Build & Development Commands

```bash
# Build
cargo build                     # debug build
cargo build --release           # release build

# Test
cargo test                      # unit + non-ignored integration tests
cargo test <test_name>          # run a single test by name
cargo test --test daemon_e2e -- --ignored --test-threads=1  # E2E (requires tmux)

# Lint (CI enforces -D warnings)
cargo clippy -- -D warnings
cargo fmt -- --check

# Code coverage (requires: cargo install cargo-llvm-cov)
./scripts/coverage.sh           # HTML report → target/llvm-cov/html/
./scripts/coverage.sh --text    # text summary
./scripts/coverage.sh --open    # HTML report and open in browser

# Install locally
cargo install --path .
```

**Pre-commit hooks** (cargo-husky): `cargo fmt` and `cargo clippy` run automatically on commit. If clippy fails, the commit is rejected — fix before retrying.

**E2E tests** require tmux and run with `--ignored`. CI uses `MIDTOWN_WEBHOOK_PORT=0` and `MIDTOWN_CHAT_MONITOR=0` to disable network features during testing.

**Containerized E2E tests** (canonical way to run E2E — reproducible environment):

```bash
# Using the CLI:
midtown e2e auth                            # one-time: authenticate for container testing
midtown e2e run coordination                # fast, no auth needed
midtown e2e run full                        # real Claude, needs auth setup first

# Or use the scripts directly:
./scripts/e2e-container.sh coordination
./scripts/e2e-container.sh full
```

**While waiting for GitHub CI**: After pushing a PR, don't wait idle for CI results. Run the full containerized E2E tests locally:

```bash
midtown e2e run coordination    # run while CI is in progress
```

This catches failures faster than waiting for GitHub Actions and keeps you productive. The container environment matches CI, so local passes should match remote passes.

## Architecture

### State Machine Daemon

The daemon (`src/daemon/`) is the central coordinator. It implements an **event-driven state machine** with strict separation between pure decision logic and side effects:

```
Event sources (timer ticks, webhooks, RPC, signals)
    → DaemonEvent
    → collect_snapshot() → WorldSnapshot (immutable)
    → evaluate_tick(event, snapshot, state) → Vec<Effect>   // pure, in rules.rs
    → execute_effects(effects)                               // imperative shell, in effects.rs
```

- **`rules.rs`**: Pure decision functions. All daemon intelligence lives here — returns `Vec<Effect>` without performing any I/O. This is where coworker lifecycle, PR management, and task assignment decisions are made.
- **`daemon/effects.rs`**: The only place side effects execute. Each `Effect` variant maps to a concrete action (spawn, shutdown, nudge, post message).
- **`daemon/snapshot.rs`**: `WorldSnapshot` — immutable view of all state, collected once per tick.
- **`daemon/events.rs`**: Event dispatch mapping RPC calls and timer ticks to `DaemonEvent`.

### Channel System

`src/channel.rs` — Append-only JSONL log at `~/.midtown/projects/<repo>/channel.jsonl`. File-locked (fs2) for concurrent access. `src/cursor.rs` tracks per-agent read positions for incremental reads.

### Coworker Lifecycle

`src/coworker.rs` manages coworker state (spawn, nudge, shutdown). Each coworker:
- Runs in an isolated git worktree (`~/.midtown/coworkers/<repo>/<name>/`)
- Runs as a headless Claude Code session using `SessionManager` with JSON streaming
- Is named after Manhattan avenues (lexington, park, madison, broadway, amsterdam, columbus, riverside, york, pleasant, vernon)

`src/launch.rs` builds Claude CLI commands and settings for both lead (tmux-based) and coworkers (headless). `src/session_manager.rs` manages headless coworker sessions using JSON streaming for communication.

### Nudge System

Nudge decisions are made in `src/rules.rs` (`decide_interrupt_nudges`, `decide_prompt_nudges`) using `CooldownTracker` for per-coworker cooldowns and `CoworkerPhase` for deduplication (Idle → Prompted → Interrupted). Delivery is via `Effect::NudgeCoworker` / `Effect::NudgeLead` in `src/daemon/effects.rs`:
- **Lead nudges**: `tmux::send_keys()` to inject messages into the tmux pane
- **Coworker nudges**: JSON streaming via `SessionManager` for headless sessions

### GitHub Integration

- `src/webhook.rs`: Receives GitHub webhook events (PR, review, check runs), verified with HMAC-SHA256
- PR polling (30s interval) for CI status and merge conflicts
- `src/github_state.rs`: Persistent reviewer assignment tracking

### Web Layer

- `src/web.rs`: WebSocket server for the Svelte frontend (real-time chat, coworker status)
- `src/webserver.rs`: Multi-project HTTP server on port 47022
- `web-app/`: Svelte 5 + Vite SPA with PWA support

### Task Coordination

`src/tasks.rs` reads Claude Code's native task storage (`~/.claude/tasks/`). Coworkers share a task list with the Lead — the daemon monitors task ownership and status to coordinate spawning and assignment.

### Configuration

- Global: `~/.midtown/config.toml`
- Per-project: `~/.midtown/projects/<repo>/config.toml`
- `src/config.rs` handles TOML parsing with environment variable overrides

### Agent System Prompts

`src/agents.rs` generates system prompts. The markdown templates live in `agents/` (lead.md, coworker.md, common.md, personalities.md).

## Architecture Principles

### Webhooks Are Primary, Polling Adapts

Webhooks handle real-time GitHub events. Polling runs at a relaxed cadence (~2 min) as a backstop for missed deliveries and time-based stuck detection. When webhooks are degraded, polling increases cadence to compensate. Polling should never duplicate a decision that a webhook already triggered.

| Concern | Primary owner | Notes |
|---|---|---|
| PR needs review → spawn reviewer | Webhook | Polling reconciles if missed |
| CI failure → notify owner | Webhook | Polling detects time-based stuck conditions |
| Review comment → nudge owner | Webhook | Polling reconciles if missed |
| Merge conflict → nudge owner | Polling | GitHub doesn't webhook this reliably |
| Approved PR → nudge author | Polling | Author-driven merge decisions |
| Stuck detection | Polling | Inherently time-based |

### Three Communication Paths, Distinct Purposes

- **Initial prompt** — "Here's your mission." One-shot context at spawn time.
- **Channel** — "Here's what's happening." Ambient team awareness, async.
- **Nudge** (`tmux send-keys`) — "Pay attention now." Synchronous interrupt for session recovery, urgent PR feedback, task assignment to active coworkers.

Don't nudge for information that can wait for the next channel read.

### Decision Functions Are Pure

Functions in `rules.rs` take immutable data and return decisions. No mutation, no I/O, no async. Phase transitions are returned as data, applied by the caller. If a decision depends on a side effect (spawn success, API call), split into two decisions with an effect in between. The `evaluate_tick()` → `Vec<Effect>` → `execute_effects()` pipeline is the canonical path.

**This constraint applies to ALL functions called from `evaluate_tick()`**, not just those in `rules.rs`. Domain modules like `pr.rs`, `dispatch.rs`, and `health.rs` contain `collect_*_effects()` functions that are called during the decision phase — these must also be pure. Specifically: no `.await`, no `state.persistent_state.lock()`, no `session_manager.is_alive()`, no direct state queries. If data is needed for a decision, add it to `WorldSnapshot` during `collect_snapshot()` so it's available as immutable input.

### Daemon Is the Single Authority for State

The daemon owns all coordination state. Coworkers report workflow state via RPC (`midtown` CLI). Pane scraping is a safety net for health checks (stuck, zombie, crash) — not the primary source of workflow information. If RPC and pane scraping disagree, pane scraping wins for health decisions.

### The Channel Is for Communication, Not State

State flows through RPC to the daemon. The channel records events and conversations for awareness. No system should read the channel to determine current state.

### Clear Ownership Between Webhooks and Polling

Each concern has a primary owner. The non-owner path only acts as reconciliation when the primary failed. Enforce via explicit tracking ("webhook handled PR #42"), not passive deduplication (cooldowns).

### Daemon Module Is a Thin Orchestrator

`mod.rs` is the event loop wiring. Domain logic lives in domain modules (`pr.rs`, `health.rs`, `dispatch.rs`, `chat.rs`, `rpc.rs`).

### Names Reflect Actual Responsibility

`SessionMonitorTick` (coworker health), `TaskDispatchTick` (work assignment). Name components for what they do, not their historical origin.

## Key Patterns

**Effect-based side effects**: Never perform I/O in decision functions. Return `Effect` variants from `rules.rs`, execute them in `effects.rs`. This keeps the core logic pure and testable.

**Temp-file pattern for shell arguments**: When passing long text to the `claude` CLI (system prompts, initial prompts), write to a temp file and use `$(cat file)` in the command string. This avoids shell quoting issues. See `write_lead_prompt_file()` in `tmux.rs` and prompt writing in `launch.rs`.

**Hybrid process model**: Lead runs in a tmux window for interactive use; Coworkers run as headless Claude Code sessions. Status is communicated via `/me` channel messages. Lead nudges use `tmux send-keys`; coworker nudges use JSON streaming via `SessionManager`.

## Lead Maintenance

If you are the Lead, whenever a PR is merged into main, pull, rebuild, and restart so the running daemon and coworkers pick up the changes:

```bash
git pull && cargo build --release && midtown restart
```

Post to the channel when done so the team knows the new code is live:

```bash
midtown channel post "Pulled main, rebuilt release, and restarted midtown."
```

## Debugging & Test Fixtures

### Debugging Unexpected Daemon Behavior (Lead Workflow)

**IMPORTANT: The Lead MUST do this PROACTIVELY whenever noticing daemon misbehavior — don't wait for the user to ask.** This includes anomalies spotted during routine channel reads (loops, stale tasks, failed spawns — see the Channel Monitoring section in lead.md).

When the Lead notices the daemon doing something unexpected (wrong decisions, missed task assignments, incorrect stuck detection, false positive warnings, reviewer not spawning, reassignment loops, etc.):

1. **Capture the state immediately** before it changes:
   ```bash
   midtown e2e capture --label <brief-bug-description>
   ```

2. **Create a task for a coworker** to write a failing E2E test and fix the bug:
   ```
   TaskCreate with:
   - subject: "Fix <bug description>"
   - description: "Captured snapshot: snapshot-<label>-<timestamp>.json

     Expected: <what should have happened>
     Actual: <what happened instead>

     Write a failing E2E test using the captured snapshot, then fix the bug."
   ```

3. **Post to the channel** so the team is aware of the issue.

4. **The coworker should**:
   - Load the captured snapshot in a test
   - Write assertions that fail with the current behavior
   - Fix the bug
   - Verify the test passes

This ensures bugs get test coverage before fixes, preventing regressions. **Act immediately** — the daemon state changes quickly and valuable debug info is lost if not captured promptly.

### Debug Logging

For detailed daemon debugging, run with increased log level:

```bash
RUST_LOG=midtown=debug midtown daemon    # summary pane info per tick
RUST_LOG=midtown=trace midtown daemon    # full pane content + WorldSnapshot JSON
```

The daemon logs:
- **debug**: Pane summary (lines, has_output, has_input_text) per coworker
- **trace**: Full pane content and serialized WorldSnapshot

This helps diagnose issues like incorrect stuck detection, nudge timing, or pane parsing bugs.

### Creating Failing Test Cases

When a bug involves daemon behavior:

1. **Capture the state**: Run `midtown e2e capture --label <bug-description>` while the issue is occurring
2. **Create a test**: Load the fixture and verify the expected behavior against the captured state
3. **Fix and verify**: The test should fail, then pass after the fix

Example test pattern:
```rust
#[test]
fn test_stuck_detection_with_usage_limit() {
    let fixture = include_str!("fixtures/snapshot/snapshot-usage-limit-20250202-123456.json");
    let snapshot: WorldSnapshot = serde_json::from_str(fixture).unwrap();
    // Test that stuck detection handles usage limit screen correctly
}
```

## Keeping README.md Up-to-Date

When your changes affect anything documented in README.md, update the README as part of the same PR. This includes:

- **New CLI commands or subcommands** — add usage examples to the relevant section
- **Changed CLI interfaces** — update command syntax, flags, or options
- **New features** — add a section or update an existing one (e.g., new daemon capabilities, web UI features)
- **Architecture changes** — update the "How It Works" section if the system design changes
- **Configuration changes** — update config examples if new settings are added or existing ones change
- **Removed or renamed functionality** — remove or update stale references

The README is the first thing new users and contributors see. If your PR changes user-facing behavior, the README should reflect it.

## Pull Requests

- When a PR includes visual changes to the web UI (`web-app/` or `web/`), include before/after screenshots in the PR description.
