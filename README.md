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

Midtown provides to UIs:

1. A tmux-based TUI
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
| `midtown auth login <profile>` | Create a new profile or re-authenticate an existing one. Launches Claude's OAuth flow. |
| `midtown auth list` | List all available profiles, marking the currently active one. |
| `midtown auth switch <profile>` | Switch to a different profile. The new profile becomes active for all future Claude sessions. |
| `midtown auth status` | Show the current profile and its authentication status. |
| `midtown auth remove <profile>` | Remove a profile and its stored credentials. |

### Example Workflow

```bash
# Set up a work profile
midtown auth login work
# Claude opens for OAuth...

# Set up a personal profile
midtown auth login personal
# Claude opens for OAuth...

# List profiles
midtown auth list
#   work
# * personal (current)

# Switch to work account
midtown auth switch work

# Check current status
midtown auth status
# Current profile: work
# Status: authenticated
```

## How It Works

### Coworkers

Each coworker runs in:

- An isolated git worktree (no merge conflicts during development)
- A tmux window within the project session
- With a Stop hook that syncs the channel at natural pause points
- With `--add-dir` worktrees for additional repos in multi-repo projects

### Channel Sync

Coworkers stay synchronized via a Claude Code Stop hook. When Claude pauses, the hook reads new channel messages and checks for unclaimed tasks. This means coworkers automatically receive updates at natural pause points.

### Worktree Lifecycle

When a coworker is called in, midtown creates a detached git worktree at the current HEAD. The coworker creates a feature branch and works independently. When the coworker shuts down, worktrees with no commits and no uncommitted changes are automatically cleaned up along with their branches. Worktrees with work in progress are preserved.

### Webhook Ports

Each project daemon runs its own webhook server for GitHub integration. Port 47022 is reserved for the shared multi-project webserver. Per-project daemons auto-assign ports starting at 47023, persisting the assignment in the project's `config.toml` for stability across restarts.

## License

MIT
