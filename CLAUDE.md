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

`src/nudge/` — Delivers messages to coworkers via tmux `send-keys`. Handles deduplication (fingerprint-based), cooldowns per coworker, and phase tracking (Idle → Prompted → Interrupted).

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

## Key Patterns

**Effect-based side effects**: Never perform I/O in decision functions. Return `Effect` variants from `rules.rs`, execute them in `effects.rs`. This keeps the core logic pure and testable.

**Temp-file pattern for shell arguments**: When passing long text to the `claude` CLI (system prompts, initial prompts), write to a temp file and use `$(cat file)` in the command string. This avoids shell quoting issues. See `write_coworker_prompt_file()` in `tmux.rs`.

**Tmux as the process model**: All agents (Lead + Coworkers) are tmux windows. Status is communicated via `/me` channel messages which get parsed into tmux tab names. Nudges are delivered via `tmux send-keys`.

## Pull Requests

- When a PR includes visual changes to the web UI (`web-app/` or `web/`), include before/after screenshots in the PR description.
