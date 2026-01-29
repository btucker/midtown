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

### 3. Attach to the session

```bash
midtown attach
```

You're now in the Lead's Claude Code instance.

### 4. Spawn coworkers

The Lead can spawn coworkers to parallelize work:

```bash
midtown coworker spawn
# => Spawned coworker: lexington
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

- The Lead collaborates with the human to create a plan & split up the work into tasks & depedendencies.
- Multiple Coworkers implement independent components simultaneously
- The Coworker review & merge PRs while the Lead & human collaborate on what's next

Midtown provides the infrastructure for this coordination:

- **Channel messaging** - IRC-like append-only message stream for team communication
- **Coworker spawning** - Launch Claude Code instances in isolated git worktrees
- **Task coordination** - Coworkers claim tasks via Claude Code's native task system

## Features

### Web App

Access Midtown remotely via the built-in web interface:

```bash
midtown start  # Web server starts on port 47022
```

The web app provides:
- **Real-time chat** with WebSocket updates and markdown rendering
- **Kanban board** showing tasks and PRs at a glance
- **Tmux tab** for streaming any tmux window
- **Mobile-friendly** Svelte interface for on-the-go monitoring

### Kanban Board

A visual task board appears in both the terminal (chat pane) and web UI:

- **In Progress** column shows active tasks with duration
- **Review** column shows open PRs with CI status dots (✓ green, ✗ red, ○ pending)
- **Done** column shows recently merged PRs
- Clickable GitHub hyperlinks to PRs

### Automation

The daemon handles routine coordination automatically:

- **Auto-spawn** - Spawns coworkers when pending tasks are available
- **Auto-shutdown** - Shuts down idle coworkers after 5 minutes
- **Auto-review** - Spawns reviewers for open PRs
- **CI failure handling** - Notifies PR owners of CI failures and merge conflicts
- **Plugin sync** - Automatically installs required plugins for coworkers
- **Duplicate detection** - Kills duplicate workers claiming the same task

### Chat TUI

The terminal chat pane includes:

- **Selection mode** (`s` key) - Toggle for copying text
- **Coworker colors** - Each coworker has a distinct color
- **Grouped messages** - Messages from the same author are grouped
- **Live updates** - Async tail-based updates for instant message delivery
- **Scrollable history** - Mouse wheel scrolling through message history

### Notifications & Nudges

Stay informed about important events:

- **@mention routing** - Messages mentioning `@coworker` are delivered to that coworker
- **PR activity nudges** - Coworkers are nudged when their PRs receive comments or reviews
- **Feedback detection** - Lead is nudged when coworkers request input
- **Mobile nudges** - Lead receives nudges for messages sent from the mobile web app
- **Interrupted coworker detection** - Detects and restarts stuck coworkers

### GitHub Integration

Automatic webhook integration with GitHub events (requires `gh auth login`):

- PR opened, merged, closed events appear in channel
- CI status updates (checks passing/failing)
- Review comments and approvals
- Merge conflict detection

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
│   (append-only log)     │       (spawn/track/shutdown)       │
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

## CLI Reference

```
midtown <COMMAND>

Commands:
  start       Start the daemon and tmux session
  stop        Stop daemon and all coworkers
  attach      Attach to the tmux session
  status      Show system status

  channel     Channel messaging
    post <MSG>    Post a message
    read          Read recent messages

  coworker    Coworker management
    spawn         Spawn a new coworker
    list          List active coworkers
    shutdown <N>  Shutdown coworker by name
```

## How It Works

### Coworkers

Each coworker runs in:
- An isolated git worktree (no merge conflicts during development)
- A tmux window within the project session
- With a Stop hook that syncs the channel at natural pause points

### Channel Sync

Coworkers stay synchronized via a Claude Code Stop hook. When Claude pauses, the hook reads new channel messages and checks for unclaimed tasks. This means coworkers automatically receive updates at natural pause points.

## License

MIT
