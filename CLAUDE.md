# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Midtown is a multi-Claude Code workspace manager. It coordinates a Lead (human-facing Claude Code) and multiple autonomous Coworkers, each running in isolated git worktrees within a shared tmux session. Communication happens through an IRC-style append-only channel log.

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
- Gets a dedicated tmux window in the `midtown-<project>` session
- Is named after Manhattan avenues (lexington, park, madison, broadway, amsterdam, columbus, riverside, york, pleasant, vernon)

`src/tmux.rs` handles all tmux operations — window create/kill, `send-keys` for nudges, pane capture, status parsing from `/me` messages.

### Nudge System

Nudge decisions are made in `src/rules.rs` (`decide_interrupt_nudges`, `decide_prompt_nudges`) using `CooldownTracker` for per-coworker cooldowns and `CoworkerPhase` for deduplication (Idle → Prompted → Interrupted). Delivery is via `Effect::NudgeCoworker` / `Effect::NudgeLead` in `src/daemon/effects.rs`, which calls `tmux::send_keys()` in `src/tmux.rs`.

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
| Review comment → nudge owner | Webhook | Polling does not handle this |
| Merge conflict → nudge owner | Polling | GitHub doesn't webhook this reliably |
| Auto-merge eligibility | Polling | Time-based: approved + green + no conflicts |
| Stuck detection | Polling | Inherently time-based |

### Three Communication Paths, Distinct Purposes

- **Initial prompt** — "Here's your mission." One-shot context at spawn time.
- **Channel** — "Here's what's happening." Ambient team awareness, async.
- **Nudge** (`tmux send-keys`) — "Pay attention now." Synchronous interrupt for session recovery, urgent PR feedback, task assignment to active coworkers.

Don't nudge for information that can wait for the next channel read.

### Decision Functions Are Pure

Functions in `rules.rs` take immutable data and return decisions. No mutation, no I/O, no async. Phase transitions are returned as data, applied by the caller. If a decision depends on a side effect (spawn success, API call), split into two decisions with an effect in between. The `evaluate_tick()` → `Vec<Effect>` → `execute_effects()` pipeline is the canonical path.

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

**Temp-file pattern for shell arguments**: When passing long text to the `claude` CLI (system prompts, initial prompts), write to a temp file and use `$(cat file)` in the command string. This avoids shell quoting issues. See `write_coworker_prompt_file()` in `tmux.rs`.

**Tmux as the process model**: All agents (Lead + Coworkers) are tmux windows. Status is communicated via `/me` channel messages which get parsed into tmux tab names. Nudges are delivered via `tmux send-keys`.

## Lead Maintenance

If you are the Lead, whenever a PR is merged into main, pull, rebuild, and restart so the running daemon and coworkers pick up the changes:

```bash
git pull && cargo build --release && midtown restart
```

Post to the channel when done so the team knows the new code is live:

```bash
midtown channel post "Pulled main, rebuilt release, and restarted midtown."
```

## Pull Requests

- When a PR includes visual changes to the web UI (`web-app/` or `web/`), include before/after screenshots in the PR description.
