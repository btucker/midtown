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
   - **Web UI**: `DOGFOOD_WEB_URL` (set as an environment variable, use Playwright MCP tools)
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
- `browser_navigate` to $DOGFOOD_WEB_URL
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
