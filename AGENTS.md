# AGENTS.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Midtown is a multi-Claude Code workspace manager. It coordinates a Lead (human-facing Claude Code in a terminal session) and multiple autonomous Coworkers (headless Claude Code sessions), each running in isolated git worktrees. Communication happens through an IRC-style append-only channel log.

See [docs/architecture.md](docs/architecture.md) for a full architecture reference.

## Build & Development Commands

```bash
# Build & install
cargo build                     # debug build (daemon only)
cargo install --path .          # release build + install to ~/.cargo/bin/

# Test
cargo test                      # unit + non-ignored integration tests
cargo test <test_name>          # run a single test by name
cargo test --test daemon_e2e -- --ignored --test-threads=1  # E2E (requires Zellij)

# Lint (CI enforces -D warnings)
cargo clippy -- -D warnings
cargo fmt -- --check

# Code coverage (requires: cargo install cargo-llvm-cov)
./scripts/coverage.sh           # HTML report → target/llvm-cov/html/
./scripts/coverage.sh --text    # text summary
./scripts/coverage.sh --open    # HTML report and open in browser

```

**Shared build cache (sccache)**: Midtown uses many worktrees simultaneously. sccache shares compiled dependency artifacts across all of them, so new worktrees build in ~15-30s instead of ~2-4min (only the `midtown` crate recompiles).

**One-time setup** (modifies `~/.cargo/config.toml`, which applies globally to all Rust projects on this machine):
```bash
cargo install sccache

# Add to ~/.cargo/config.toml:
# [build]
# rustc-wrapper = "sccache"
# [env]
# CARGO_INCREMENTAL = "0"
# SCCACHE_DIR = "/Users/<you>/.midtown/sccache"  # absolute path required (~ not expanded in TOML)

sccache --show-stats   # verify cache hits after a second build
```

**Test file placement**: Put unit tests in separate files (`src/daemon/pr_tests.rs`) rather than inline `#[cfg(test)] mod tests` blocks. Use `#[path = "pr_tests.rs"] #[cfg(test)] mod tests;` in the source file to maintain private access. This keeps PR diffs focused — reviewers can see how much is test vs. implementation at a glance. Integration/E2E tests go in `tests/` as usual.

**Pre-commit hooks** (cargo-husky): `cargo fmt` and `cargo clippy` run automatically on commit. If clippy fails, the commit is rejected — fix before retrying.

**E2E tests** require Zellij and run with `--ignored`. CI uses `MIDTOWN_WEBHOOK_PORT=0` and `MIDTOWN_CHAT_MONITOR=0` to disable network features during testing.

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

## Conventions

**Decision functions are pure**: Functions in `rules.rs` (and all functions called from `evaluate_tick()`) must not perform I/O, mutation, or async operations. Return `Vec<Effect>` instead. If data is needed for a decision, add it to `WorldSnapshot` during `collect_world_snapshot()`. See [docs/architecture.md](docs/architecture.md) for the full pipeline.

**Effect-based side effects**: Never perform I/O in decision functions. Return `Effect` variants from `rules.rs`, execute them in `effects.rs`.

**Daemon module is a thin orchestrator**: `mod.rs` wires the event loop. Domain logic lives in `pr.rs`, `health.rs`, `dispatch.rs`, `chat.rs`, `rpc.rs`.

**Temp-file pattern for shell arguments**: When passing long text to the `claude` CLI (system prompts, initial prompts), write to a temp file and use `$(cat file)` in the command string. This avoids shell quoting issues. See `launch.rs`.

**Webhooks are primary, polling adapts**: Webhooks handle real-time GitHub events. Polling is a backstop only — never duplicate a decision a webhook already triggered. See [docs/architecture.md](docs/architecture.md) for the ownership table.

**Names reflect actual responsibility**: `SessionMonitorTick` (coworker health), `TaskDispatchTick` (work assignment).

## Lead Maintenance

If you are the Lead, whenever a PR is merged into main, pull, rebuild, and restart so the running daemon and coworkers pick up the changes:

```bash
git pull && cargo install --path . && midtown restart
```

Post to the channel when done so the team knows the new code is live:

```bash
midtown channel post "Pulled main, installed updated binary, and restarted midtown."
```

## Debugging & Test Fixtures

### Debugging Unexpected Daemon Behavior (Lead Workflow)

**IMPORTANT: The Lead MUST do this PROACTIVELY whenever noticing daemon misbehavior — don't wait for the user to ask.** This includes anomalies spotted during routine channel reads (loops, stale tasks, failed spawns — see the Channel Monitoring section in lead.md).

When the Lead notices the daemon doing something unexpected (wrong decisions, missed task assignments, incorrect stuck detection, false positive warnings, reviewer not spawning, reassignment loops, etc.):

1. **Capture the state immediately** before it changes:
   ```bash
   midtown e2e capture --label <brief-bug-description>
   ```

2. **Move the snapshot into test fixtures** so it gets committed:
   ```bash
   mv tests/fixtures/snapshot/captured/snapshot-<label>-<timestamp>.json tests/fixtures/snapshot/
   ```

3. **Create a task for a coworker** to write a failing E2E test and fix the bug:
   ```
   TaskCreate with:
   - subject: "Fix <bug description>"
   - description: "Captured snapshot: snapshot-<label>-<timestamp>.json

     Expected: <what should have happened>
     Actual: <what happened instead>

     Write a failing E2E test using the captured snapshot, then fix the bug."
   ```

4. **Post to the channel** so the team is aware of the issue.

5. **The coworker should**:
   - Load the captured snapshot in a test
   - Write assertions that fail with the current behavior
   - Fix the bug
   - Verify the test passes

This ensures bugs get test coverage before fixes, preventing regressions. **Act immediately** — the daemon state changes quickly and valuable debug info is lost if not captured promptly.

### Daemon Log

**Always check the daemon log when debugging.** The running daemon writes to:

```bash
~/.midtown/projects/<repo>/logs/daemon.log
```

This log captures all daemon activity — task assignments, coworker spawns, RPC calls, PR events, effect execution. Check it *first* when investigating unexpected behavior, before reaching for snapshots or other debugging tools.

```bash
# View recent daemon activity
tail -100 ~/.midtown/projects/<repo>/logs/daemon.log

# Follow live
tail -f ~/.midtown/projects/<repo>/logs/daemon.log
```

The daemon respects the `MIDTOWN_LOG_LEVEL` environment variable for controlling log verbosity:

```bash
MIDTOWN_LOG_LEVEL=debug midtown daemon    # task assignments, coworker spawns, pane summaries per tick
MIDTOWN_LOG_LEVEL=trace midtown daemon    # full pane content + serialized WorldSnapshot JSON
```

### Creating Failing Test Cases

When a bug involves daemon behavior:

1. **Capture the state**: Run `midtown e2e capture --label <bug-description>` while the issue is occurring (saves to gitignored `captured/` staging area)
2. **Move to fixtures**: `mv tests/fixtures/snapshot/captured/<file> tests/fixtures/snapshot/`
3. **Create a test**: Load the fixture and verify the expected behavior against the captured state
4. **Fix and verify**: The test should fail, then pass after the fix

Example test pattern:
```rust
#[test]
fn test_stuck_detection_with_usage_limit() {
    let fixture = include_str!("fixtures/snapshot/snapshot-usage-limit-20250202-123456.json");
    let snapshot: WorldSnapshot = serde_json::from_str(fixture).unwrap();
    // Test that stuck detection handles usage limit screen correctly
}
```

## Keeping docs/architecture.md Up-to-Date

`docs/architecture.md` is the living reference for how the codebase works. Keep it current:

- **When exploring the codebase** (using the Explore agent or deep code reads), capture what you learn in `docs/architecture.md`. If a module's behavior, data flow, or key invariant isn't documented there yet, add it.
- **When reviewing PRs**, check whether the changes should be reflected in `docs/architecture.md`. New modules, changed data flows, new state fields, and altered decision logic should all be documented.

## Keeping README.md Up-to-Date

When your changes affect anything documented in README.md, update the README as part of the same PR. This includes:

- **New CLI commands or subcommands** — add usage examples to the relevant section
- **Changed CLI interfaces** — update command syntax, flags, or options
- **New features** — add a section or update an existing one (e.g., new daemon capabilities, web UI features)
- **Architecture changes** — update the "How It Works" section if the system design changes
- **Configuration changes** — update config examples if new settings are added or existing ones change
- **Removed or renamed functionality** — remove or update stale references

The README is the first thing new users and contributors see. If your PR changes user-facing behavior, the README should reflect it.

## Web App (PWA) Guidelines

The web app runs as a PWA on mobile devices. When modifying layout or adding new UI sections:

- **Always account for `safe-area-inset-*`** — iOS PWAs have a status bar/notch that overlaps content. Use `env(safe-area-inset-top)`, `env(safe-area-inset-bottom)`, etc. for any element positioned at the edges of the viewport. Forgetting this causes content to be cut off on mobile.
- **Test vertical space** — Mobile viewports are constrained. Avoid adding headings, padding, or chrome that pushes primary content (chat, status) off-screen.

## Pull Requests

- When a PR includes visual changes to the web UI (`web-app/` or `web/`), include before/after screenshots in the PR description.
