# Self-Testing Developer Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create a Claude Code loop that dogfoods Midtown by acting as a new contributor — using Midtown on a clone of its own repo via the web UI (Playwright) and CLI, finding bugs, updating specs, writing failing e2e tests, fixing code, opening PRs, and addressing review feedback.

**Architecture:** A prompt file defines the persona and loop cycle. A setup script creates an isolated clone of the Midtown repo as the test project. The Claude Code session runs in a git worktree of the main repo (for code changes) and interacts with the daemon running in the test clone via Playwright and CLI.

**Tech Stack:** Markdown (prompt), Bash (setup scripts), Claude Code (loop runner), Playwright MCP (web UI interaction)

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `scripts/dogfood-prompt.md` | Create | Persona prompt + loop cycle instructions for Claude Code |
| `scripts/dogfood-setup.sh` | Create | Creates test clone, builds midtown, starts daemon |
| `scripts/dogfood-teardown.sh` | Create | Stops daemon, cleans up test clone |
| `scripts/dogfood.sh` | Create | One-command launcher: setup → Claude Code loop → teardown |

---

### Task 1: Create the persona prompt file

**Files:**
- Create: `scripts/dogfood-prompt.md`

This is the core artifact — the prompt that drives the entire loop. It must establish the persona, reference the specs, define the cycle, and give concrete instructions for each phase.

- [ ] **Step 1: Write the persona and context section**

```markdown
# Midtown Self-Testing Developer Loop

You are a developer who is new to Midtown and wants to use it to manage development
work. You've read the README and docs but haven't used Midtown before.

## Your Environment

You have two workspaces:

1. **Development worktree** (your cwd) — a git worktree of the Midtown repo where you
   make code changes (spec updates, tests, bug fixes). This is YOUR workspace.

2. **Test clone** at `DOGFOOD_CLONE_DIR` (set as an environment variable) — a separate
   clone of the Midtown repo where a Midtown daemon is running. This is the product
   you're testing. You interact with it via:
   - **Web UI**: http://localhost:47022 (use Playwright MCP tools)
   - **CLI**: Run `midtown` commands with `--project-dir $DOGFOOD_CLONE_DIR`

## Reference Material

These define what Midtown SHOULD do. When reality diverges from these, that's a bug:
- `README.md` — what a new user is told to expect
- `docs/v2-spec.md` — authoritative behavioral specification
- `docs/architecture.md` — how the system works internally
- `docs/superpowers/specs/` — feature-specific design specs

## Your Cycle

Follow this cycle continuously. Each iteration either exercises Midtown further or
fixes a bug you found.

### EXPLORE
Pick something to try that you haven't exercised yet. Read the README, docs, and
specs for ideas. Think like a real developer: "What would I try next?"

Ideas to explore (not exhaustive — be creative):
- Start the daemon, open the web UI, look around
- Create a channel, post messages, see if the lead responds
- Create a task, watch if a worker gets spawned
- Check task status through the web UI vs CLI — do they match?
- Try thread conversations
- Look at PR status
- Try stopping and restarting the daemon
- Try resuming sessions
- Post an @mention and see if the right agent gets nudged
- Create dependent tasks and check dispatch order
- Check the mobile/PWA layout for safe-area issues

### USE (Playwright + CLI)
Interact with Midtown primarily through the web UI using Playwright MCP tools:
- `browser_navigate` to http://localhost:47022
- `browser_snapshot` to see current state
- `browser_click` to interact with elements
- `browser_fill_form` to type messages
- `browser_take_screenshot` to capture evidence

Use CLI for setup and diagnostics:
- `cd $DOGFOOD_CLONE_DIR && midtown status`
- `cd $DOGFOOD_CLONE_DIR && midtown agent list`
- `cd $DOGFOOD_CLONE_DIR && midtown log`

### DIAGNOSE
When something breaks or behaves unexpectedly:
1. Is this a Midtown bug or did I do something wrong? Check the spec.
2. Check daemon logs: `cd $DOGFOOD_CLONE_DIR && midtown log`
3. Compare CLI state vs web UI state — mismatches are bugs
4. Identify the root cause in the Midtown source code (in your dev worktree)
5. Classify: daemon bug, web API bug, or web UI-only bug

### SPEC
If the spec doesn't document the expected behavior, or documents it incorrectly:
1. Find the relevant spec file (usually `docs/v2-spec.md` or in `docs/superpowers/specs/`)
2. Update it to document what the behavior SHOULD be
3. This is important — the spec update makes the fix traceable

### RED
Write a failing e2e test that reproduces the bug:
1. Add to an existing test file in `tests/` if the bug fits, or create a new one
2. Use `DaemonTestHarness` or `V2Harness` from `tests/common/mod.rs`
3. Use `fake-claude-cli` for deterministic behavior
4. Run the test and confirm it FAILS:
   ```bash
   cargo test <test_name> -- --ignored  # if marked ignored
   cargo test <test_name>               # if coordination-level
   ```
5. The test failing proves you've captured the bug

For web UI-only bugs (no daemon/API issue), document in the spec and skip the e2e test.

### GREEN
Fix the source code:
1. Make the minimal change to fix the bug
2. Run the failing test — confirm it now PASSES
3. Run the broader test suite to check for regressions:
   ```bash
   cargo test
   cargo clippy --all-targets --all-features -- -D warnings
   ```

### PR
Open a pull request:
1. Create a feature branch: `git checkout -b fix/<descriptive-name>`
2. Commit the changes (spec update + test + fix)
3. Push and open a PR: `gh pr create --title "fix: <description>" --body "..."`
4. Include in the PR body:
   - What broke (with screenshot if from web UI)
   - Which spec section was updated
   - The failing test name
   - The fix

### REVIEW
Wait for and address review feedback:
1. Check CI: `gh pr checks <pr-number>`
2. Check for review comments: `gh pr view <pr-number> --comments`
3. Also check: `gh api repos/{owner}/{repo}/pulls/<pr-number>/reviews`
4. If CI fails, fix and push follow-up commits
5. If reviewers leave comments, address them and push
6. Repeat until CI is green and reviews are addressed
7. Do NOT merge — leave that for the repo maintainer

### REBUILD
After the PR is clean:
1. Rebuild the binary: `cargo install --path .` (in your dev worktree)
2. Restart the daemon in the test clone:
   ```bash
   cd $DOGFOOD_CLONE_DIR && midtown stop
   cd $DOGFOOD_CLONE_DIR && midtown start
   ```
3. Verify the fix works in the real product via the web UI
4. Go back to EXPLORE

## Important Rules

- **One bug per cycle.** Don't try to fix multiple bugs at once. Find one, fix it,
  PR it, then move on.
- **Spec first.** Always check/update the spec before writing the test. The spec is
  the source of truth for expected behavior.
- **E2E tests only.** Write integration/e2e tests in `tests/`, not unit tests. The
  e2e tests exercise the real daemon.
- **Screenshots as evidence.** When you find a web UI bug, take a screenshot with
  Playwright before diagnosing. Include it in the PR.
- **Stay curious.** Don't just test the happy path. Try edge cases, error scenarios,
  rapid sequences, stop/restart cycles.
- **Don't fix what isn't broken.** If something works as documented, move on to
  testing something else. Don't refactor or improve working code.
```

- [ ] **Step 2: Verify the prompt file reads cleanly**

Run: `wc -l scripts/dogfood-prompt.md`
Expected: ~140-160 lines

- [ ] **Step 3: Commit**

```bash
git add scripts/dogfood-prompt.md
git commit -m "feat: add self-testing developer loop prompt"
```

---

### Task 2: Create the setup script

**Files:**
- Create: `scripts/dogfood-setup.sh`

This script creates the test clone, builds the midtown binary, and starts the daemon.

- [ ] **Step 1: Write the setup script**

```bash
#!/usr/bin/env bash
set -euo pipefail

# Self-Testing Developer Loop — Setup
# Creates a test clone of the Midtown repo and starts a daemon in it.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Generate a unique clone directory
CLONE_ID="dogfood-$$"
CLONE_DIR="/tmp/midtown-${CLONE_ID}"

echo "=== Midtown Dogfood Setup ==="
echo "Repo root:  $REPO_ROOT"
echo "Clone dir:  $CLONE_DIR"
echo ""

# Step 1: Build the midtown binary from the current source
echo "[1/4] Building midtown..."
cd "$REPO_ROOT"
cargo install --path . 2>&1 | tail -5

# Step 2: Clone the repo
echo "[2/4] Cloning repo to $CLONE_DIR..."
if [ -d "$CLONE_DIR" ]; then
    echo "  Clone already exists, removing..."
    rm -rf "$CLONE_DIR"
fi
git clone "$REPO_ROOT" "$CLONE_DIR"

# Step 3: Initialize the clone as a project
echo "[3/4] Starting daemon in test clone..."
cd "$CLONE_DIR"
midtown start

# Step 4: Write the clone dir to a file for the prompt to read
ENVFILE="$REPO_ROOT/.dogfood-env"
echo "DOGFOOD_CLONE_DIR=$CLONE_DIR" > "$ENVFILE"
echo ""
echo "=== Setup complete ==="
echo "Clone dir: $CLONE_DIR"
echo "Web UI:    http://localhost:47022"
echo "Env file:  $ENVFILE"
echo ""
echo "To start the loop:"
echo "  export DOGFOOD_CLONE_DIR=$CLONE_DIR"
echo "  claude -p \"\$(cat scripts/dogfood-prompt.md)\" --allowedTools '*'"
```

- [ ] **Step 2: Make it executable**

Run: `chmod +x scripts/dogfood-setup.sh`

- [ ] **Step 3: Commit**

```bash
git add scripts/dogfood-setup.sh
git commit -m "feat: add dogfood setup script"
```

---

### Task 3: Create the teardown script

**Files:**
- Create: `scripts/dogfood-teardown.sh`

- [ ] **Step 1: Write the teardown script**

```bash
#!/usr/bin/env bash
set -euo pipefail

# Self-Testing Developer Loop — Teardown
# Stops the daemon and removes the test clone.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ENVFILE="$REPO_ROOT/.dogfood-env"

if [ ! -f "$ENVFILE" ]; then
    echo "No .dogfood-env file found. Nothing to tear down."
    exit 0
fi

source "$ENVFILE"

echo "=== Midtown Dogfood Teardown ==="
echo "Clone dir: $DOGFOOD_CLONE_DIR"
echo ""

# Step 1: Stop the daemon
if [ -d "$DOGFOOD_CLONE_DIR" ]; then
    echo "[1/2] Stopping daemon..."
    cd "$DOGFOOD_CLONE_DIR"
    midtown stop 2>/dev/null || true
fi

# Step 2: Remove the clone
echo "[2/2] Removing test clone..."
rm -rf "$DOGFOOD_CLONE_DIR"

# Clean up env file
rm -f "$ENVFILE"

echo ""
echo "=== Teardown complete ==="
```

- [ ] **Step 2: Make it executable**

Run: `chmod +x scripts/dogfood-teardown.sh`

- [ ] **Step 3: Commit**

```bash
git add scripts/dogfood-teardown.sh
git commit -m "feat: add dogfood teardown script"
```

---

### Task 4: Create the one-command launcher

**Files:**
- Create: `scripts/dogfood.sh`

This ties setup → Claude Code loop → teardown into one command.

- [ ] **Step 1: Write the launcher script**

```bash
#!/usr/bin/env bash
set -euo pipefail

# Self-Testing Developer Loop — Launcher
# One command to set up, run the loop, and clean up.
#
# Usage:
#   ./scripts/dogfood.sh           # run the loop
#   ./scripts/dogfood.sh --setup   # just set up (don't start loop)
#   ./scripts/dogfood.sh --teardown # just tear down

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

case "${1:-run}" in
    --setup)
        "$SCRIPT_DIR/dogfood-setup.sh"
        exit 0
        ;;
    --teardown)
        "$SCRIPT_DIR/dogfood-teardown.sh"
        exit 0
        ;;
    run|"")
        ;;
    *)
        echo "Usage: $0 [--setup|--teardown]"
        exit 1
        ;;
esac

# Setup
"$SCRIPT_DIR/dogfood-setup.sh"

# Source the env file to get DOGFOOD_CLONE_DIR
source "$REPO_ROOT/.dogfood-env"
export DOGFOOD_CLONE_DIR

# Create a worktree for the Claude Code session to work in
BRANCH_NAME="dogfood/$(date +%Y%m%d-%H%M%S)"
WORKTREE_DIR="$REPO_ROOT/.claude/worktrees/$BRANCH_NAME"
git worktree add "$WORKTREE_DIR" -b "$BRANCH_NAME" main
cd "$WORKTREE_DIR"

echo ""
echo "=== Starting Claude Code loop ==="
echo "Working in worktree: $WORKTREE_DIR"
echo "Test clone:          $DOGFOOD_CLONE_DIR"
echo "Web UI:              http://localhost:47022"
echo ""

# Run the loop
# The prompt file is read from the worktree since it's a copy of the repo
claude -p "$(cat scripts/dogfood-prompt.md)" --allowedTools '*'

# Teardown (optional — user may want to keep the clone for inspection)
echo ""
read -p "Clean up test clone? [y/N] " -n 1 -r
echo ""
if [[ $REPLY =~ ^[Yy]$ ]]; then
    "$SCRIPT_DIR/dogfood-teardown.sh"
fi
```

- [ ] **Step 2: Make it executable**

Run: `chmod +x scripts/dogfood.sh`

- [ ] **Step 3: Add `.dogfood-env` to `.gitignore`**

Check if `.gitignore` exists and add the entry:

```
# Dogfood loop state
.dogfood-env
```

- [ ] **Step 4: Commit**

```bash
git add scripts/dogfood.sh .gitignore
git commit -m "feat: add dogfood one-command launcher"
```

---

### Task 5: Smoke test the loop

Run the setup and verify the test clone works before trusting the full loop.

- [ ] **Step 1: Run setup**

Run: `./scripts/dogfood-setup.sh`
Expected: Clone created, daemon started, env file written. No errors.

- [ ] **Step 2: Verify the daemon is running**

Run: `source .dogfood-env && cd $DOGFOOD_CLONE_DIR && midtown status`
Expected: Daemon status shows running.

- [ ] **Step 3: Verify the web UI is accessible**

Use Playwright MCP tools:
- `browser_navigate` to `http://localhost:47022`
- `browser_snapshot` to confirm the page loads

Expected: Midtown web UI loads with project visible.

- [ ] **Step 4: Verify teardown works**

Run: `./scripts/dogfood-teardown.sh`
Expected: Daemon stopped, clone removed, env file cleaned up.

- [ ] **Step 5: Commit any fixes from smoke testing**

If the smoke test revealed issues with the scripts, fix and commit them:

```bash
git add scripts/
git commit -m "fix: dogfood script adjustments from smoke testing"
```
