# Self-Testing Developer Loop

## Problem

Midtown has a strong spec-driven test pipeline (spec → spec_tests → e2e) but no mechanism for **exploratory dogfooding** — using Midtown as a real developer would and discovering bugs that scripted tests miss. Edge cases in daemon behavior, web UI rendering, CLI workflows, and session lifecycle only surface when someone actually uses the product end-to-end.

## Goal

Create a Claude Code loop that acts as a "new contributor" using Midtown on a real project. It interacts primarily through the web UI (via Playwright), falls back to CLI for setup and diagnostics, and when it finds bugs: updates specs, writes failing e2e tests, fixes the code, and resumes exploring.

## Design

### Two-workspace architecture

The loop operates across two separate copies of the Midtown repo:

1. **Development worktree** — a git worktree of `~/projects/midtown`, where all code changes happen (spec updates, new e2e tests, bug fixes). This is where the Claude Code session runs.

2. **Test clone** — a fresh `git clone` of the Midtown repo to a temporary location (e.g., `/tmp/midtown-dogfood-<id>/`). The agent builds and runs `midtown start` here to exercise the product as a real user. This clone is disposable — it gets recreated when needed.

This separation ensures:
- The daemon under test runs against a real, isolated project directory
- Code changes (specs, tests, fixes) happen in the development worktree without affecting the running daemon
- The test clone can be blown away and recreated to test fresh-install experiences
- No risk of the daemon's worktree management conflicting with the development worktree

### Persona

The agent adopts the persona of a **developer new to Midtown** who:
- Has read the README and docs but hasn't used Midtown before
- Wants to use Midtown to manage development work on a project
- Tries CLI commands, explores the web UI, creates channels, spawns workers
- Has reasonable expectations based on documentation — when reality doesn't match docs, that's a bug
- Uses `v2-spec.md` and design specs as the authoritative reference for expected behavior

The persona is NOT scripted. The agent decides what to try next based on what it's already explored, what the docs say is possible, and natural curiosity. This exploratory approach finds bugs that scripted tests miss.

### Interaction layers

**Primary: Playwright (web app)**
- Navigate the web UI at `http://localhost:47022`
- Browse channels, post messages, monitor worker status, check tasks, review PRs
- Use Playwright MCP tools: `browser_navigate`, `browser_snapshot`, `browser_click`, `browser_fill_form`, `browser_take_screenshot`
- Screenshots serve as evidence when documenting bugs
- Compare what the web UI shows vs what the CLI/daemon state reports — mismatches are bugs

**Secondary: CLI (setup + diagnostics)**
- `midtown start` / `midtown stop` for daemon lifecycle on the test clone
- `midtown channel create`, `midtown task create` for bootstrapping workflows
- `midtown status`, `midtown agent list` for diagnosing when web UI behavior seems wrong
- Direct RPC via the daemon socket for deep inspection

### Loop cycle

```
EXPLORE → SETUP → USE → [works?]
    → yes → USE (continue exploring in browser)
    → no  → DIAGNOSE → SPEC → RED → GREEN → PR → REVIEW → REBUILD → EXPLORE
```

**EXPLORE** — Read docs, pick something to try that hasn't been exercised yet. Consult README, v2-spec.md, and design specs for ideas.

**SETUP** — Build midtown in the development worktree, copy/install the binary to the test clone, start the daemon there.

**USE** — Interact with Midtown through the web UI (Playwright) and CLI. Try real workflows: create a channel, post a message, create a task, watch a worker spawn, check PR status.

**DIAGNOSE** — When something breaks or behaves unexpectedly: Is this a Midtown bug or user error? Check the spec. Check daemon logs (`midtown log`). Compare CLI state vs web UI state. Identify the root cause.

**SPEC** — If the spec doesn't document the expected behavior, or documents it incorrectly, update the relevant spec file (usually `docs/v2-spec.md` or a design spec in `docs/superpowers/specs/`).

**RED** — Write a failing e2e test in `tests/` that reproduces the bug. Use existing test infrastructure (`DaemonTestHarness`, `V2Harness`, `fake-claude-cli`). Run it, confirm it fails.

**GREEN** — Fix the source code. Run the test again, confirm it passes. Run the full test suite to check for regressions.

**PR** — Commit the fix on a feature branch, push, and open a PR via `gh pr create`. Each bug fix gets its own PR with: spec update (if any), failing test, and fix.

**REVIEW** — Wait for review feedback. Check for:
- CI results (`gh pr checks`)
- GitHub review comments (`gh pr view --comments`, `gh api repos/.../pulls/.../comments`)
- If the test clone's daemon is watching the repo, Midtown's own code-reviewer agents may post reviews — this itself exercises the PR review pipeline

Address any review feedback: push follow-up commits, respond to comments. Repeat until CI is green and reviews are addressed. This mirrors what a real contributor does — they don't just open a PR and walk away.

**REBUILD** — Rebuild the binary from the now-reviewed branch, update the test clone, restart the daemon. This picks up the fix so the agent can verify it in the real product too.

### E2E test conventions

New tests follow the established patterns:
- Use `DaemonTestHarness` or `V2Harness` from `tests/common/mod.rs`
- Use `fake-claude-cli` for deterministic behavior (coordination-level tests preferred)
- Test file naming: add to an existing test file if the bug fits an existing category, otherwise create a new `tests/<area>_e2e.rs`
- Test function naming: `test_<what_should_happen>` (e.g., `test_worker_resumes_after_channel_post`)
- No `#[ignore]` unless the test requires real Claude CLI

### Bug classification

The agent may find bugs in three layers:

| Layer | Signal | Test approach |
|-------|--------|---------------|
| **Daemon** | Wrong decision logic, spawn failures, routing errors, state corruption | E2E test with `V2Harness` exercising daemon RPC |
| **Web API** | Wrong HTTP response, missing WebSocket events, stale data | E2E test hitting the daemon's HTTP/WS API |
| **Web UI** | Rendering bugs, broken interactions, stale state display | Note in spec + manual verification (Playwright tests are out of scope for now) |

For web UI-only bugs (no daemon/API issue), the agent documents the bug in a spec update and moves on rather than writing an e2e test. Web UI bugs will be addressed when Playwright-based web testing infrastructure is built.

### Test clone lifecycle

1. **Create**: `git clone ~/projects/midtown /tmp/midtown-dogfood-<id>/`
2. **Build**: `cargo build` in the test clone (or copy the binary from the dev worktree)
3. **Start**: `cd /tmp/midtown-dogfood-<id> && midtown start`
4. **Use**: Interact via web UI + CLI
5. **Rebuild after fix**: After fixing a bug in the dev worktree, rebuild and copy the binary to the test clone, restart daemon
6. **Destroy**: When done or when a fresh start is needed, `rm -rf /tmp/midtown-dogfood-<id>/`

### Claude Code loop mechanism

The loop runs as a single Claude Code session with a prompt file that establishes the persona and cycle. The session is started with something like:

```bash
claude -p "$(cat docs/self-test-loop-prompt.md)" --allowedTools '*'
```

Or via the `/loop` skill for recurring execution. The prompt file contains the persona, the cycle instructions, and pointers to the specs as reference material.

### Graduation path (future)

Once this loop finds fewer bugs per iteration and Midtown is stable enough:
1. Extract the persona prompt into `agents/definitions/midtown-bug-hunter.md`
2. Register it as a Midtown agent type
3. Run it as a Midtown coworker posting to a `#quality` channel
4. Other Midtown workers pick up fix tasks from its findings
5. True self-hosting: Midtown tests itself through itself

## What this does NOT include

- No custom framework or tooling — just a prompt, Claude Code, and Playwright
- No test project scaffolding — uses a clone of Midtown itself
- No orchestrator process — single Claude Code session in a loop
- No reporting dashboard — findings are commits, tests, and spec updates
- No Playwright-based web UI test infrastructure (out of scope — may be a future addition)
