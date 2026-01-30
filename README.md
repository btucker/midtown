# Midtown

Coordinate multiple **Claude Code** instances working on the same codebase using native Claude Code tasks.

## Quick Start

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

```bash
midtown attach
```

You're now in the Lead's Claude Code instance.

To attach to a named project from any directory:

```bash
midtown attach myapp
```

### 4. Call in coworkers

The Lead can call in coworkers to parallelize work:

```bash
midtown coworker call-in
# => Called in coworker: lexington
```

Coworkers are named after Manhattan avenues: lexington, park, madison, broadway, amsterdam, columbus, riverside, york, pleasant, vernon.

### 5. Monitor progress

```bash
midtown status
```

Shows: active coworkers, open tasks, open PRs, recent channel activity.

### 6. Stop when done

```bash
midtown stop
```

## Why Midtown?

Midtown is inspired by [Gastown](https://github.com/anthropics/claude-code/tree/main/.claude/docs/gastown.md), but a bit simpler, less exciting, and, well, more mid.

At its core, Midtown is built around an IRC-like messaging model: a shared channel where team members (both the human-facing Lead and autonomous Coworkers) post updates, coordinate handoffs, and stay in sync. This append-only message stream is the backbone of multi-agent collaboration—each Claude Code instance reads the channel at natural pause points, just like checking a team chat.

When you're working with Claude Code on a complex project, you might want to parallelize work:

- The Lead collaborates with the human to create a plan & split up the work into tasks & dependencies.
- Multiple Coworkers implement independent components simultaneously
- The Coworkers review & merge PRs while the Lead & human collaborate on what's next

Midtown provides the infrastructure for this coordination:

- **Channel messaging** - IRC-like append-only message stream for team communication
- **Coworker call-in** - Launch Claude Code instances in isolated git worktrees
- **Task coordination** - Coworkers claim tasks via Claude Code's native task system
- **Multi-project support** - Run multiple projects simultaneously with isolated daemons

## Features

### Multi-Project Support

Midtown can manage multiple projects, each with its own daemon, tmux session, and set of coworkers. Projects are identified by name and can span multiple repositories.

**Starting a project:**

```bash
# Single-repo project (name inferred from directory)
midtown start

# Named project with additional repos
midtown start --project myapp --add-repo /path/to/frontend --add-repo /path/to/shared-lib
```

**Managing projects:**

```bash
# List all known projects and their status
midtown project list

# Attach to a specific project
midtown attach myapp
```

Each project gets:
- Its own daemon process and Unix socket
- A tmux session named `midtown-<project>`
- A dedicated channel log and task list
- An auto-assigned webhook port (47023+)
- Per-project configuration in `~/.midtown/projects/<project>/config.toml`

### Multi-Repo Worktrees

For projects spanning multiple repositories, coworkers automatically get isolated worktrees in every repo. When a coworker is called in, midtown creates a git worktree in each configured repo and passes them to Claude Code via `--add-dir` flags. This gives each coworker access to all repos in the project.

When coworkers shut down, worktrees with no commits and no uncommitted changes are automatically cleaned up, along with their branches.

### Shared Webserver

A standalone webserver provides a unified view across all projects:

```bash
midtown webserver run          # Start on port 47022
midtown webserver stop         # Stop the webserver
midtown webserver restart      # Restart the webserver
```

The shared webserver discovers running project daemons and proxies to their per-project webhook servers. Port 47022 is reserved for the shared webserver; per-project daemons use ports 47023+.

The `midtown start` command auto-launches the shared webserver if it's not already running.

### Web App

Access Midtown remotely via the built-in web interface:

```bash
midtown start  # Web server starts automatically
```

The web app provides:
- **Real-time chat** with WebSocket updates and markdown rendering
- **Kanban board** showing tasks and PRs at a glance
- **Tmux tab** for streaming any tmux window
- **Mobile-friendly** Svelte interface for on-the-go monitoring
- **Project tabs** for switching between projects (shared webserver)

### Kanban Board

A visual task board appears in both the terminal (chat pane) and web UI:

- **Backlog** column shows pending tasks
- **In Progress** column shows active tasks with owner and duration
- **Review** column shows open PRs with CI status dots (● green, ● red, ○ pending), author, and reviewer
- **Done** column shows recently merged PRs
- Clickable GitHub hyperlinks to PRs
- Collapsible column headings
- **Repo badges** in multi-repo projects (e.g., `[frontend] PR#42`)

The terminal also displays a **repo status bar** showing the latest commit, CI status, and tag for each repo. Multi-repo projects show one status line per repo.

### Automation

The daemon handles routine coordination automatically:

- **Auto-call-in** - Calls in coworkers when pending tasks are available
- **Auto-shutdown** - Shuts down idle coworkers after 5 minutes
- **Auto-review** - Calls in reviewers for open PRs
- **CI failure handling** - Notifies PR owners of CI failures and merge conflicts
- **Plugin sync** - Automatically installs required plugins for coworkers
- **Duplicate detection** - Kills duplicate workers claiming the same task
- **Reviewer slot reservation** - Reserves 2 coworker slots for PR reviewers
- **Orphan recovery** - Detects and nudges orphaned coworkers to resume their tasks
- **Channel log rotation** - Auto-rotates channel.jsonl every 24 hours

### Chat TUI

The terminal chat pane includes:

- **Selection mode** (`s` key) - Toggle for copying text
- **Coworker colors** - Each coworker has a distinct color
- **Grouped messages** - Messages from the same author are grouped
- **Live updates** - Async tail-based updates for instant message delivery
- **Scrollable history** - Mouse wheel scrolling through message history
- **Expandable input** - Input box grows vertically with line wrapping
- **Scroll-to-bottom button** - Quick return to latest messages

### Notifications & Nudges

Stay informed about important events:

- **@mention routing** - Messages mentioning `@coworker` are delivered to that coworker
- **PR activity nudges** - Coworkers are nudged when their PRs receive comments or reviews
- **Review completion nudges** - Idle PR authors are nudged when reviews finish
- **Feedback detection** - Lead is nudged when coworkers request input
- **Mobile nudges** - Lead receives nudges for messages sent from the mobile web app
- **Orphan detection** - Detects and restarts stuck or orphaned coworkers

### GitHub Integration

Automatic webhook integration with GitHub events (requires `gh auth login`):

- PR opened, merged, closed events appear in channel
- CI status updates (checks passing/failing)
- Review comments and approvals
- Merge conflict detection

### Custom System Prompts

Customize the system prompts for Lead and Coworkers with markdown files:

- `~/.midtown/LEAD.md` / `~/.midtown/COWORKER.md` - Global custom prompts
- `~/.midtown/projects/<project>/LEAD.md` / `COWORKER.md` - Per-project custom prompts

Content from these files is appended to the built-in system prompts. Project-level files supplement global ones.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      Human Developer                         │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│                    Lead (Claude Code)                        │
│                    main git worktree                         │
└─────────────────────────┬───────────────────────────────────┘
                          │ midtown CLI
                          ▼
┌─────────────────────────────────────────────────────────────┐
│                    Midtown Daemon                            │
├─────────────────────────┬───────────────────────────────────┤
│      Channel            │          Coworker Manager          │
│   (append-only log)     │     (call-in/track/shutdown)       │
└─────────────────────────┴───────────────────────────────────┘
                          │
┌─────────────────────────────────────────────────────────────┐
│              tmux session: midtown-<project>                 │
├─────────────┬─────────────┬─────────────┬───────────────────┤
│    Lead     │  lexington  │    park     │     madison       │
│  (window)   │  (window)   │  (window)   │    (window)       │
│             │  worktree   │  worktree   │    worktree       │
└─────────────┴─────────────┴─────────────┴───────────────────┘
```

All agents run as windows within a single tmux session. Use `Ctrl-b w` to see everyone, `Ctrl-b n/p` to switch between them.

For multi-project setups, each project gets its own daemon and tmux session. A shared webserver on port 47022 provides a unified view:

```
┌─────────────────────────────────────────────────────────────┐
│              Shared Webserver (:47022)                        │
│         discovers and proxies to project daemons             │
└────────┬────────────────────────┬───────────────────────────┘
         │                        │
         ▼                        ▼
┌────────────────────┐   ┌────────────────────┐
│  Project: backend  │   │  Project: frontend │
│  Daemon (:47023)   │   │  Daemon (:47024)   │
│  tmux: midtown-    │   │  tmux: midtown-    │
│        backend     │   │        frontend    │
└────────────────────┘   └────────────────────┘
```

## CLI Reference

```
midtown [OPTIONS] <COMMAND>

Options:
  -C, --repo <PATH>    Path to git repository (defaults to cwd)

Commands:
  start               Start the daemon and tmux session
    --project <NAME>     Project name (overrides auto-detection)
    --add-repo <PATH>    Additional repository paths (repeatable)
    --daemon-only        Start daemon without tmux session
  stop                Stop daemon and all coworkers
    --keep-session       Keep the tmux session running
  restart             Restart midtown (stop + start)
  attach [PROJECT]    Attach to a project's tmux session
  status              Show system status

  project             Project management
    list                 List all projects and their status

  channel             Channel messaging
    post <MSG>           Post a message
    read                 Read recent messages

  coworker            Coworker management
    call-in              Call in a new coworker
    list                 List active coworkers
    shutdown <NAME>      Shutdown coworker by name

  webserver            Standalone multi-project webserver
    run                  Start the webserver (port 47022)
    stop                 Stop the webserver
    restart              Restart the webserver
```

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
max_coworkers = 16              # Maximum concurrent coworkers

[plugins]
required = [
    "superpowers@claude-plugins-official",
]

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
