> Back to [README](../README.md)

# CLI Reference

## Core Commands

| Command | Description |
|---------|-------------|
| `midtown start [--project <name>] [--add-repo <path>]` | Start the daemon and tmux session |
| `midtown stop [--keep-session]` | Stop the daemon (optionally keep tmux session) |
| `midtown restart` | Restart the daemon |
| `midtown attach [<project>]` | Attach to the project's tmux session |
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
| `midtown coworker view <name>` | View a coworker's terminal output (supports both tmux and headless sessions) |

## Session Management (Attach/Detach)

Attach to a headless coworker's session in an interactive tmux window for debugging or guidance, then detach to resume headless execution.

| Command | Description |
|---------|-------------|
| `midtown session attach name <coworker>` | Attach to a coworker by name |
| `midtown session attach task <id>` | Attach to the coworker working on a task |
| `midtown session attach pr <number>` | Attach to the coworker working on a PR |
| `midtown session detach <name>` | Detach and resume headless execution |
| `midtown session list` | List headless sessions with status |

**How it works:** Attach kills the headless process (the Claude session persists on disk), then opens an interactive tmux window with `claude --resume`. When the window closes (or you run `detach`), the daemon re-spawns the headless session, picking up where it left off.

## Task Management

| Command | Description |
|---------|-------------|
| `midtown task create <subject> --description <desc> [--blocked-by <ids>] [--channel <name>]` | Create a new task (optionally blocked by task IDs, optionally routed to a channel) |
| `midtown task list [--all]` | List tasks (pending/in-progress by default) |
| `midtown task view <id>` | View task details |
| `midtown task update <id> [--owner <name>] [--status <status>] [--channel <name>]` | Update a task |

## Pull Requests

| Command | Description |
|---------|-------------|
| `midtown pr list` | List pull requests |

## Project Management

| Command | Description |
|---------|-------------|
| `midtown project list` | List all known projects and their status |

## Headless Execution

Run Claude Code sessions non-interactively with JSON streaming output:

```bash
midtown headless "Summarize this codebase" --model sonnet
midtown headless "Generate a report" --json-schema '{"type": "object", ...}'
midtown headless "Fix the bug" --allow-tools --max-budget-usd 0.50
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
| `midtown auth switch <profile> [--all]` | Switch to a different profile (use `--all` for all projects) |
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
