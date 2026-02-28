> Back to [README](../README.md)

# CLI Reference

## Core Commands

| Command | Description |
|---------|-------------|
| `midtown start [--swap-layout] [--project <name>] [--add-repo <path>]` | Start the daemon and Zellij session |
| `midtown stop [--keep-session]` | Stop the daemon (optionally keep Zellij session) |
| `midtown restart [--force]` | Restart the daemon (waits for active reviewers to go on break unless `--force`) |
| `midtown attach [<project>]` | Attach to the project's Zellij session |
| `midtown status` | Show system status |
| `midtown chat` | Open the IRC-style chat TUI |
| `midtown log [--hooks] [--path] [-f] [-n <lines>]` | View daemon or hook logs |

## Channel

| Command | Description |
|---------|-------------|
| `midtown channel post <message>` | Post a message to the channel |
| `midtown channel read [--all]` | Read recent channel messages |

## Coworker Management

| Command | Description |
|---------|-------------|
| `midtown coworker call-in [--resume] [--prompt <msg>]` | Call in a new coworker |
| `midtown coworker break <name>` | Send a coworker on a break |
| `midtown coworker list` | List all coworkers |
| `midtown coworker view <name>` | View a coworker's terminal output (headless sessions) |

## Session Management (Attach/Detach)

Attach to a headless coworker's session in an interactive terminal pane for debugging or guidance, then detach to resume headless execution.

| Command | Description |
|---------|-------------|
| `midtown session attach name/<coworker>` | Attach to a coworker by name |
| `midtown session attach task/<id>` | Attach to the coworker working on a task |
| `midtown session attach pr/<number>` | Attach to the coworker working on a PR |
| `midtown session attach claude/<session_id>` | Attach by Claude platform session ID |
| `midtown session attach codex/<session_id>` | Attach by Codex platform session ID |
| `midtown session detach <name>` | Detach and resume headless execution |
| `midtown session list` | List headless sessions with status |

**How it works:** Attach stops the headless process (the Claude session persists on disk), then opens an interactive terminal pane with `claude --resume`. When you detach, the daemon re-spawns the headless session, picking up where it left off. In Zellij, the plugin handles attach/detach via the dashboard sidebar.

## Task Management

| Command | Description |
|---------|-------------|
| `midtown task create <subject> --description <desc> [--blocked-by <ids>] [--channel <name>] [--model <provider/model>] [--plan <path>] [--execution-skill <skill>] [--thread-id <message-id>]` | Create a new task (optionally blocked by task IDs, routed to a channel, assigned a model, given a plan file, assigned an execution skill, or bound to a thread). When run inside a forked session, the CLI auto-populates `--thread-id` from `$MIDTOWN_BOUND_THREAD_ID` so spawned coworkers report back to that thread. |
| `midtown task list [--all]` | List tasks (pending/in-progress by default) |
| `midtown task view <id>` | View task details |
| `midtown task update <id> [--owner <name>] [--status <status>] [--channel <name>] [--model <provider/model>]` | Update a task (use `--model ""` to clear) |

## Pull Requests

| Command | Description |
|---------|-------------|
| `midtown pr list` | List pull requests |
| `midtown pr merge --pr <N>` | Merge a PR after daemon-gated checks pass |

`midtown pr merge` enforces three gates before allowing the merge:

1. **Gate 1 — Review exists**: A completed code review comment must be present on the PR
2. **Gate 2 — CI passes**: All GitHub status checks must be passing. Also rejects PRs with `reviewDecision: CHANGES_REQUESTED`
3. **Gate 3 — Feedback addressed**: Every review comment must have a matching `<!-- addresses-review: {id} -->` tag in a reply

If any gate fails, the command returns a clear error listing which gates failed. Coworkers should never run `gh pr merge` directly — always use `midtown pr merge` so gate checks are enforced.

## Project Management

| Command | Description |
|---------|-------------|
| `midtown project list` | List all known projects and their status |

## One-shot Execution

Run Claude Code sessions non-interactively with JSON streaming output:

```bash
midtown oneshot "Summarize this codebase" --model sonnet
midtown oneshot "Generate a report" --json-schema '{"type": "object", ...}'
midtown oneshot "Fix the bug" --allow-tools --max-budget-usd 0.50
```

| Flag | Description |
|------|-------------|
| `--model <name>` | Model to use (default: `sonnet`) |
| `--system-prompt <text>` | System prompt for the session |
| `--json-schema <json>` | JSON schema for structured output |
| `--max-budget-usd <float>` | Maximum budget in USD |
| `--allow-tools` | Allow tool use (default: no tools) |

## Authentication

| Command | Description |
|---------|-------------|
| `midtown auth login <email>` | Create a new profile or re-authenticate (launches Claude for OAuth) |
| `midtown auth list` | List all profiles and interactively switch |
| `midtown auth switch <profile> [--project]` | Switch to a different profile (global by default; use `--project` for current project only) |
| `midtown auth remove <profile>` | Remove a profile and its stored credentials |

See [Authentication Profiles](authentication.md) for details.

## Lead Commands

| Command | Description |
|---------|-------------|
| `midtown lead remind all-work-merged <message>` | Set a reminder for when all work is merged |
| `midtown lead remind list` | List active reminders |
| `midtown lead remind cancel <id>` | Cancel a reminder |

## Webserver

The multi-project webserver serves the web UI and proxies to per-project daemons.

| Command | Description |
|---------|-------------|
| `midtown webserver run [--port 47022] [--foreground]` | Start the webserver |
| `midtown webserver stop` | Stop the webserver |
| `midtown webserver restart` | Restart the webserver |

## E2E Testing

| Command | Description |
|---------|-------------|
| `midtown e2e auth` | One-time auth setup for container testing |
| `midtown e2e run coordination` | Run coordination E2E tests (fast, no auth) |
| `midtown e2e run full` | Run full E2E tests (requires auth) |
| `midtown e2e capture [--label <name>]` | Capture daemon state snapshot for test fixtures |

## Claude Passthrough

| Command | Description |
|---------|-------------|
| `midtown claude [args...]` | Run Claude Code using the current midtown auth profile |

This passes all arguments through to the `claude` CLI with `CLAUDE_CONFIG_DIR` set to the active profile's directory. Useful for running Claude commands with your midtown-managed auth.

---

## Agent-Internal Commands

The commands below are used by the lead agent, coworkers, and daemon internally. They are not typically run by humans.

| Command | Description |
|---------|-------------|
| `midtown daemon` | Run the daemon server (hidden; spawned by `start`) |
| `midtown lead register-session` | Register Lead's Claude session for task sharing |
| `midtown task claim <id>` | Claim a task (used by coworkers) |
| `midtown task done <id>` | Mark a task as completed (used by coworkers) |
| `midtown task request <description>` | Request new work (posts to channel for lead) |
| `midtown coworker nudge <name> [--message <msg>]` | Nudge a coworker to check in (daemon-internal) |
| `midtown state <phase> [--task <id>]` | Report coworker workflow phase |
| `midtown hook insight\|idle\|lead-stop\|task\|ask` | Claude Code hook handlers |
| `midtown diagram validate` | Diagram validation (chat TUI internal) |
