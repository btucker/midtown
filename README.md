<div align="center"><img src="docs/images/logo.png" width="100" alt="Midtown logo"></div>

# Midtown

Work with a "lead" to manage your team of **Claude Code** "coworkers" to accomplish tasks following a github PR kanban workflow.

## Why Midtown?

Midtown is inspired by [Gastown](https://github.com/anthropics/claude-code/tree/main/.claude/docs/gastown.md), but a bit simpler, less exciting, and, well, more mid.

At its core, Midtown is built around an IRC-like messaging model: a shared channel where team members (both the human-facing "Lead" and autonomous "Coworkers") post updates, coordinate handoffs, and stay in sync. This append-only message stream is the backbone of multi-agent collaboration—each Claude Code instance reads the channel at natural pause points, just like checking a team chat. At anytime the "lead" or a "coworker" can be @mentioned and then receive that into their context context immediately.

When you're working with Claude Code on a complex project, you might want to parallelize work:

- The Lead collaborates with the human to create a plan & split up the work into tasks & dependencies.
- Multiple "coworkers" implement independent components simultaneously but with context of the full project
- The "coworkers"" review & merge PRs while the "lead"" & human collaborate on what's next

Midtown provides two UIs:

1. A tmux-based TUI with an IRC-style chat pane (includes mermaid diagram rendering with inline ASCII art)
2. A web interface (meant to be run as a PWA) so you can collaborate with the lead (and the team) while on the go.

Midtown makes extensive use of the new Claude Code Tasks system to manage the state of all work, create dependencies, and assign ownership.

## Quick Start

### 0. Prereqs

1. Install the [GitHub CLI](https://cli.github.com/).
2. Install [Rust & Cargo](https://doc.rust-lang.org/cargo/getting-started/installation.html).

### 1. Install

From [crates.io](https://crates.io/crates/midtown):

```bash
cargo install midtown
```

Or from source:

```bash
cargo install --path .
```

### 2. Start midtown

From your project directory:

```bash
midtown start
```

This starts the daemon and creates a tmux session with the Lead window.

For multi-repo projects, specify a project name and additional repos:

```bash
midtown start --project myapp --add-repo /path/to/frontend --add-repo /path/to/shared-lib
```

### 3. Attach to the session

If you're in a repo that's part of a project:

```bash
midtown attach
```

You're now in the Lead's Claude Code instance.

To attach to a named project from any directory:

```bash
midtown attach myapp
```

### 4. Work with the lead as you typically would work with Claude Code

The lead is just a claude code session, but it's been booted with some a [special system prompt](agents/lead.md). The system prompt instructs the lead how to execute in the midtown environment-- mostly to not take on work itself (unless it's trivial) and to instead make Claude Code tasks.

## CLI Reference

### Core Commands

| Command | Description |
|---------|-------------|
| `midtown start [--project <name>] [--add-repo <path>]` | Start the daemon and tmux session |
| `midtown stop [--keep-session]` | Stop the daemon (optionally keep tmux session) |
| `midtown restart` | Restart the daemon |
| `midtown attach [<project>]` | Attach to the project's tmux session |
| `midtown status` | Show system status |
| `midtown chat` | Open the IRC-style chat TUI |
| `midtown log [--hooks] [--path] [-f] [-n <lines>]` | View daemon or hook logs |

### Channel

| Command | Description |
|---------|-------------|
| `midtown channel post <message>` | Post a message to the channel |
| `midtown channel read [--all]` | Read recent channel messages |

### Coworker Management

| Command | Description |
|---------|-------------|
| `midtown coworker call-in [--resume] [--prompt <msg>]` | Call in a new coworker |
| `midtown coworker break <name>` | Send a coworker on a break |
| `midtown coworker list` | List all coworkers |
| `midtown coworker view <name>` | View a coworker's terminal output |
| `midtown coworker nudge <name> [--message <msg>]` | Nudge a coworker to check in |

### Session Management (Attach/Detach)

Attach to a headless coworker's session in an interactive tmux window for debugging or guidance, then detach to resume headless execution.

| Command | Description |
|---------|-------------|
| `midtown session attach name <coworker>` | Attach to a coworker by name |
| `midtown session attach task <id>` | Attach to the coworker working on a task |
| `midtown session attach pr <number>` | Attach to the coworker working on a PR |
| `midtown session detach <name>` | Detach and resume headless execution |
| `midtown session list` | List headless sessions with status |

**How it works:** Attach kills the headless process (the Claude session persists on disk), then opens an interactive tmux window with `claude --resume`. When the window closes (or you run `detach`), the daemon re-spawns the headless session, picking up where it left off.

### Task Management

| Command | Description |
|---------|-------------|
| `midtown task create <subject> --description <desc> [--blocked-by <ids>]` | Create a new task (optionally blocked by comma-separated task IDs) |
| `midtown task list [--all]` | List tasks (pending/in-progress by default) |
| `midtown task view <id>` | View task details |
| `midtown task update <id> [--owner <name>] [--status <status>]` | Update a task |
| `midtown task claim <id>` | Claim a task |
| `midtown task done <id>` | Mark a task as completed |
| `midtown task request <description>` | Request new work (posts to channel for lead) |

### Pull Requests

| Command | Description |
|---------|-------------|
| `midtown pr list` | List pull requests |

### Project Management

| Command | Description |
|---------|-------------|
| `midtown project list` | List all known projects and their status |

### Headless Execution

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

### Lead Commands

| Command | Description |
|---------|-------------|
| `midtown lead register-session` | Register Lead's Claude session for task sharing |
| `midtown lead remind all-work-merged <message>` | Set a reminder for when all work is merged |
| `midtown lead remind list` | List active reminders |
| `midtown lead remind cancel <id>` | Cancel a reminder |

### Webserver

The multi-project webserver serves the web UI and proxies to per-project daemons.

| Command | Description |
|---------|-------------|
| `midtown webserver run [--port 47022] [--foreground]` | Start the webserver |
| `midtown webserver stop` | Stop the webserver |
| `midtown webserver restart` | Restart the webserver |

### E2E Testing

| Command | Description |
|---------|-------------|
| `midtown e2e auth` | One-time auth setup for container testing |
| `midtown e2e run coordination` | Run coordination E2E tests (fast, no auth) |
| `midtown e2e run full` | Run full E2E tests (requires auth) |
| `midtown e2e capture [--label <name>]` | Capture daemon state snapshot for test fixtures |

## Configuration

Midtown uses two levels of config files:

1. **Global config** at `~/.midtown/config.toml` — applies to all projects
2. **Project config** at `~/.midtown/projects/<project>/config.toml` — overrides per project

Project settings take precedence over global defaults. All fields are optional.

### Global `config.toml`

```toml
# ~/.midtown/config.toml

[default]
bin_command = "midtown"         # CLI command to invoke midtown
chat_layout = "auto"            # "auto", "split", or "window"
chat_min_width = 160            # Min terminal width for split layout (auto mode)
max_coworkers = 10              # Maximum concurrent coworkers

[daemon]
webhook_port = 47022                  # Web UI & webhook port (0 to disable)
webhook_secret = "your-secret"        # GitHub webhook signature secret
webhook_restart_interval_secs = 300   # Webhook forwarder restart interval
pr_poll_interval_secs = 30            # PR polling interval
chat_monitor_enabled = true           # Enable @mention routing
```

### Project `config.toml`

Project configs support all global settings plus project metadata:

```toml
# ~/.midtown/projects/myapp/config.toml

[project]
name = "myapp"
repos = ["/path/to/backend", "/path/to/frontend"]
primary_repo = "/path/to/backend"

[default]
bin_command = "cargo run --release --"
max_coworkers = 4

[daemon]
webhook_port = 47023              # Auto-assigned if not set
```

The `[project]` section defines:

- `name` - Project name used for tmux sessions, paths, etc.
- `repos` - List of repository paths belonging to the project
- `primary_repo` - The main repo used for the daemon socket and channel

For single-repo projects, only `name` is needed; `repos` and `primary_repo` are inferred from the working directory. This config is auto-created on first `midtown start`.

### Environment Variable Overrides

Daemon settings can be overridden with environment variables:

| Variable | Overrides |
|----------|-----------|
| `MIDTOWN_WEBHOOK_PORT` | `webhook_port` |
| `MIDTOWN_WEBHOOK_SECRET` | `webhook_secret` |
| `MIDTOWN_WEBHOOK_RESTART_INTERVAL` | `webhook_restart_interval_secs` |
| `MIDTOWN_PR_POLL_INTERVAL` | `pr_poll_interval_secs` |
| `MIDTOWN_CHAT_MONITOR` | `chat_monitor_enabled` (set to `0` to disable) |
| `MIDTOWN_MAX_COWORKERS` | `max_coworkers` |

### Custom System Prompts

Customize the system prompts for Lead and Coworkers with markdown files:

- `~/.midtown/LEAD.md` / `~/.midtown/COWORKER.md` - Global custom prompts
- `~/.midtown/projects/<project>/LEAD.md` / `COWORKER.md` - Per-project custom prompts

Content from these files is appended to the built-in system prompts. Project-level files supplement global ones.

## Docker

Midtown is available as a Docker image for containerized deployments. The image includes all runtime dependencies (tmux, git, gh CLI, Claude CLI).

### Pull from Docker Hub

```bash
docker pull btucker/midtown:latest
# Or a specific version:
docker pull btucker/midtown:0.4.5
```

### Running with Docker

Mount your repository and optionally your midtown config:

```bash
# Basic usage
docker run -it --rm \
  -v /path/to/repo:/repo \
  -w /repo \
  btucker/midtown start

# With persistent config
docker run -it --rm \
  -v ~/.midtown:/home/midtown/.midtown \
  -v /path/to/repo:/repo \
  -w /repo \
  btucker/midtown start

# Status check
docker run --rm \
  -v ~/.midtown:/home/midtown/.midtown \
  -v /path/to/repo:/repo \
  -w /repo \
  btucker/midtown status
```

### Building Locally

```bash
docker build -t midtown .
docker run -it --rm -v /path/to/repo:/repo -w /repo midtown start
```

## Authentication Profiles

Midtown supports multiple Claude authentication profiles, allowing you to switch between different Claude accounts (e.g., personal and work accounts) without re-authenticating each time.

### Profile Names

Profile names can only contain alphanumeric characters, hyphens, and underscores. If no profile is specified, commands default to a profile named `default`.

### Profile Storage

Profiles are stored in `~/.midtown/auth/`:

```
~/.midtown/auth/
├── current              # Text file containing the active profile name
└── <profile>/           # Per-profile directory
    └── .claude.json     # Claude config with auth tokens
```

When midtown spawns Claude sessions (Lead or Coworkers), it sets `CLAUDE_CONFIG_DIR` to the active profile's directory, isolating authentication between profiles.

### Commands

| Command | Description |
|---------|-------------|
| `midtown auth login --profile <name>` | Create a new profile or re-authenticate an existing one. Launches a Claude session where you run `/login` to complete OAuth. |
| `midtown auth list` | List all available profiles, marking the currently active one. |
| `midtown auth switch <profile>` | Switch to a different profile. The new profile becomes active for all future Claude sessions. |
| `midtown auth status` | Show the current profile, config directory, and authentication status. |
| `midtown auth remove <profile>` | Remove a profile and its stored credentials. |

### Example Workflow

```bash
# Set up a work profile
midtown auth login --profile work
# Claude opens — run /login inside to authenticate

# Set up a personal profile
midtown auth login --profile personal
# Claude opens — run /login inside to authenticate

# List profiles
midtown auth list
# Profiles:
#   work - authenticated
#   personal (active) - authenticated

# Switch to work account
midtown auth switch work

# Check current status
midtown auth status
# Current profile: work
# Config dir: /home/user/.midtown/auth/work
# Status: authenticated
```

## How It Works

### Daemon

The daemon is the central coordinator. It runs an event-driven state machine that collects an immutable snapshot of the world each tick, makes pure decisions about what should happen, and then executes the resulting effects. This strict separation between decision logic and side effects keeps the core testable.

The daemon handles:
- Coworker lifecycle (spawning, health checks, stuck detection, shutdown)
- Task assignment and dispatch
- GitHub webhook processing (PR events, CI status, reviews)
- PR polling for merge conflicts and stuck conditions
- @mention routing between team members

### Coworkers

Each coworker runs as:

- A headless Claude Code process (`claude -p --output-format stream-json`) managed by the daemon's `SessionManager`
- In an isolated git worktree (no merge conflicts during development)
- With `--add-dir` worktrees for additional repos in multi-repo projects
- Nudges are delivered via stdin JSON, and health is monitored via stdout stream events

Coworkers are named after Manhattan avenues: lexington, park, madison, broadway, amsterdam, columbus, riverside, york, pleasant, vernon.

### Channel Sync

Coworkers stay synchronized via a Claude Code Stop hook. When Claude pauses, the hook reads new channel messages and checks for unclaimed tasks. This means coworkers automatically receive updates at natural pause points.

### Mailbox Messaging

In addition to the shared channel, the daemon can deliver targeted messages to individual coworkers via the Claude Code agent teams mailbox protocol. Messages are written as JSON to `~/.claude/teams/{team-name}/inboxes/{agent-name}.json` using atomic file operations with mkdir-based locking for safe concurrent access.

### Worktree Lifecycle

When a coworker is called in, midtown creates a detached git worktree at the current HEAD. The coworker creates a feature branch and works independently. When the coworker shuts down, worktrees with no commits and no uncommitted changes are automatically cleaned up along with their branches. Worktrees with work in progress are preserved.

### GitHub Integration

The daemon receives real-time GitHub events via webhooks (PR creation, reviews, check runs) verified with HMAC-SHA256 signatures. PR polling runs as a backstop for missed webhook deliveries and handles time-based concerns like merge conflict detection and stuck PR identification.

### Webhook Ports

Each project daemon runs its own webhook server for GitHub integration. Port 47022 is reserved for the shared multi-project webserver. Per-project daemons auto-assign ports starting at 47023, persisting the assignment in the project's `config.toml` for stability across restarts.

### Chat TUI

The `midtown chat` command opens an IRC-style chat interface with:

- Real-time channel message display
- Mermaid diagram detection and rendering (via `selkie-rs` with content-hash caching)
- Inline ASCII art for flowchart diagrams (press number keys to open SVG in browser)
- Mouse support for scrolling and navigation
- Clickable hyperlinks via OSC 8 escape sequences
- Real-time token usage and cost tracking

### Web UI

The web interface is a Svelte 5 + Vite SPA served on port 47022:

- Installable as a PWA for mobile use
- Real-time updates via WebSocket
- Kanban board for task visualization
- Channel chat interface with mermaid diagram rendering
- Coworker status monitoring
- Auth profile switching
- Push notifications (W3C Push API with VAPID)

### Reminders

The Lead can set reminders that trigger on specific conditions:

```bash
# Remind me when all tasks are done and PRs merged
midtown lead remind all-work-merged "Time to deploy!"

# List active reminders
midtown lead remind list

# Cancel a reminder
midtown lead remind cancel <id>
```

Reminders are stored in `~/.midtown/projects/<repo>/reminders.json` and evaluated by the daemon each tick.

## License

MIT
