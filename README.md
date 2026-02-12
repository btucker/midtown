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

## CLI Overview

| Command | Description |
|---------|-------------|
| `midtown start` | Start the daemon and tmux session |
| `midtown stop` | Stop everything |
| `midtown restart` | Restart the daemon |
| `midtown attach` | Attach to the tmux session |
| `midtown status` | Show system status |
| `midtown chat` | Open the IRC-style chat TUI |
| `midtown log [-f]` | View daemon logs |
| `midtown channel post <msg>` | Post to the team channel |
| `midtown channel read` | Read recent messages |
| `midtown coworker call-in` | Spawn a new coworker |
| `midtown coworker break <name>` | Send a coworker on break |
| `midtown coworker list` | List active coworkers |
| `midtown coworker view <name>` | View a coworker's output |
| `midtown session attach name <n>` | Attach to a headless session |
| `midtown session detach <name>` | Resume headless execution |
| `midtown task create <subject> [...]` | Create a task (see [CLI reference](docs/cli.md) for all options) |
| `midtown task list` | List tasks |
| `midtown task view <id>` | View task details |
| `midtown pr list` | List pull requests |
| `midtown auth` | Manage [auth profiles](docs/authentication.md) (multiple accounts supported) |
| `midtown headless "<prompt>"` | Run a headless Claude session |

See the [full CLI reference](docs/cli.md) for all flags and options.

## Documentation

| Topic | Description |
|-------|-------------|
| [CLI Reference](docs/cli.md) | Full command reference with all flags and agent-internal commands |
| [Configuration](docs/configuration.md) | Global and per-project config files, environment variables, custom prompts |
| [Authentication](docs/authentication.md) | Multi-account auth profiles, switching, storage |
| [Architecture](docs/architecture.md) | Daemon, coworkers, channel sync, GitHub integration, web UI |
| [Docker](docs/docker.md) | Docker images, running in containers |

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

Midtown supports provider-scoped auth profiles (`claude`, `codex`) so you can keep separate personal/work accounts per provider and switch quickly.

### Profile Names

Profile names can contain alphanumeric characters, hyphens, underscores, `@`, and `.`. If no profile is set, Midtown uses `default`.

### Profile Storage

Profiles are stored under `~/.midtown/auth/`:

```text
~/.midtown/auth/
├── current                             # Active Claude profile
├── <claude-profile>/                   # Claude profiles (legacy layout)
└── providers/
    └── codex/
        ├── current                     # Active Codex profile
        └── profiles/
            └── <codex-profile>/        # Codex profiles
```

When Midtown launches a session, it sets provider-specific env vars (`CLAUDE_CONFIG_DIR` for Claude, `CODEX_HOME` for Codex) to the active profile directory.

### Commands

| Command | Description |
|---------|-------------|
| `midtown auth list` | List profiles for the default provider (`claude`). |
| `midtown auth --provider codex list` | List profiles for a specific provider. |
| `midtown auth --all-providers list` | Show profile status for all supported providers. |
| `midtown auth --provider <provider> login <email>` | Create/re-authenticate a profile for a provider. |
| `midtown auth --provider <provider> switch <profile> [--project]` | Switch active profile (global by default; use `--project` for current repo only). |
| `midtown auth --provider <provider> remove <profile>` | Remove a profile for that provider. |

### Example Workflow

```bash
# Create/login Claude work profile
midtown auth --provider claude login work@example.com

# Create/login Codex work profile
midtown auth --provider codex login work@example.com

# Show both providers
midtown auth --all-providers list

# List only Claude profiles
midtown auth list

# Switch Claude profile globally (default)
midtown auth --provider claude switch work@example.com

# Switch Codex profile for current project only
midtown auth --provider codex switch work@example.com --project
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
- In a [filesystem sandbox](#sandboxing) restricting write access to project directories
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

The `midtown chat` command opens a split-panel interface with:

**Layout**:
- **Board panel** (left 40%): Channel swimlanes showing in-progress (●) and pending (○) tasks per channel
- **Chat panel** (right 60%): Real-time message display with mermaid diagram rendering
- **Input bar** (bottom): Text input for posting messages (Tab to focus, Enter to send)

**Features**:
- Real-time channel message display
- Mermaid diagram detection and rendering (via `selkie-rs` with content-hash caching)
- Inline ASCII art for flowchart diagrams (press number keys to open SVG in browser)
- **Type-anywhere UX**: Character keys auto-focus the input bar (like Slack/Discord)
- Tab-based focus navigation (Board → Chat → InputBar)
- Arrow keys, PageUp/PageDown, Home/End for scrolling
- Mouse support for scrolling and navigation
- Clickable hyperlinks via OSC 8 escape sequences
- Real-time token usage and cost tracking

### Web UI

The web interface is a Svelte 5 + Vite SPA served on port 47022:

- Installable as a PWA for mobile use
- Real-time updates via WebSocket
- Kanban board for task visualization
- Multi-channel support with split-panel layout (channel list sidebar + message pane)
- Channel list with task counts (in progress, pending) and CI status badges
- Channel header displays channel-specific stats (PR count, in-progress tasks, pending tasks) that update when switching channels
- Create new channels directly from the sidebar (+ button) with inline validation
- Clickable channel (`#name`), task (`!N`), and PR (`#N`) references in messages
- Insight cross-post highlighting with source channel attribution
- Mermaid diagram rendering in chat messages
- Image and document paste support (clipboard → inline preview → upload to lead)
- Coworker status monitoring
- Auth profile switching
- Push notifications (W3C Push API with VAPID)
- Responsive layout with three breakpoints:
  - **Mobile (≤768px)**: Tab navigation, hamburger menu with slide-out sidebar, modal popups for task/PR details
  - **Tablet (769–1024px)**: Permanent sidebar replaces tab navigation, two-column grid layout
  - **Desktop (≥1025px)**: Three-column Slack-inspired layout with sidebar, main channel, and toggleable detail panel for tasks, PRs, and coworker info
- Clickable `@coworker` mentions in messages open coworker detail panel on desktop

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

### Sandboxing

Midtown uses platform-native filesystem sandboxing to restrict write access for both the Lead session and all Coworkers, while preserving full read access:

- **macOS**: `sandbox-exec` with SBPL profiles
- **Linux**: `bwrap` (bubblewrap) when available; falls back to unsandboxed execution if bwrap is not installed
- **Zero overhead**: Same-host sandboxing — no Docker, no containers, same binaries and auth tokens

**Fallback behavior:** If sandbox setup fails (e.g., missing `bwrap` on Linux, profile creation errors on macOS, or sandbox nesting detected after `midtown restart`), Midtown logs a warning and falls back to running without the extra sandbox wrapper. When nesting is detected, coworkers still inherit the Lead's sandbox through the process chain, so write restrictions remain in effect. This ensures sessions aren't blocked by sandbox issues.

**How it works:**

The sandbox allows reads from anywhere but restricts writes to explicitly permitted directories:

- Project repositories (primary + additional repos in multi-repo projects)
- `~/.midtown` (daemon state, channel logs, worktrees)
- `~/.claude` (Claude Code config, sessions, tasks)
- `~/.codex` (Codex config)
- `~/.local/state/midtown` (daemon socket, runtime state)
- `/tmp` and platform-specific temp directories

**Git worktree support:** When the primary repo is a git worktree, Midtown detects this and automatically adds the main repository's `.git/` directory to the writable list, ensuring git operations (commits, refs, objects) work correctly.

**macOS SBPL profile structure:**
```scheme
(allow default)                          ; Allow all operations by default
(deny file-write* (subpath "$HOME"))    ; Block writes under home directory
(allow file-write*                       ; Re-allow writes to specific paths
  (subpath "/path/to/project")
  (subpath "~/.midtown")
  ...)
```

This approach means Claude Code can access any file for context (reading documentation, analyzing dependencies) but can only modify files within the project workspace and configuration directories.

## License

MIT
