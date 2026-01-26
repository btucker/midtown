# Midtown

Coordinate multiple **Claude Code** instances working on the same codebase. Midtown lets a human-facing Claude Code session (the "Lead") spawn and orchestrate additional Claude Code instances (the "Coworkers"), each working in isolated git worktrees. The Coworkers collaborate, open PRs, review them, and merge them.

## Why Midtown?

Midtown is inspired by [Gastown](https://github.com/steveyegge/gastown), but a bit simpler, less exciting, and more mid.

At its core, Midtown is built around a **Slack-like messaging model**: a shared channel where team members (both the human-facing Lead and autonomous Coworkers) post updates, coordinate handoffs, and stay in sync. This append-only message stream is the backbone of multi-agent collaboration—each Claude Code instance reads the channel at natural pause points, just like checking a team chat.

When you're working with Claude Code on a complex project, you might want to parallelize work:

- The Lead collaborates with the human to create a plan
- Multiple Coworkers implement independent components simultaneously
- A Coworker reviews PRs while the Lead & human collaborate on what's next

Midtown provides the infrastructure for this coordination:

- **Channel messaging** - Slack-like append-only message stream for team communication
- **Coworker spawning** - Launch Claude Code instances in isolated git worktrees
- **Task coordination** - Coworkers claim tasks via Claude Code's native task system

## Architecture

Midtown consists of two components:

1. **The Daemon** (`midtownd`) - A Rust-based background service that manages:
   - Channel message storage and delivery
   - Coworker lifecycle (spawn, track, shutdown)
   - Git worktree management
   - Task coordination

2. **The Claude Code Plugin** - A native plugin that provides:
   - Tools for Lead and Coworker agents
   - Stop hooks for automatic channel synchronization
   - Commands for status and channel interaction
   - Agent definitions with role-specific capabilities

```
┌─────────────────────────────────────────────────────────┐
│                     Human Developer                      │
└─────────────────────────┬───────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────┐
│                   Lead (Claude Code)                     │
│                   main git worktree                      │
│                   + midtown plugin                       │
└─────────────────────────┬───────────────────────────────┘
                          │ midtown CLI
                          ▼
┌─────────────────────────────────────────────────────────┐
│                   Midtown Daemon                         │
│              (Unix socket: ~/.local/state/midtown/)      │
├─────────────────────────┬───────────────────────────────┤
│     Channel             │         Coworker Manager       │
│  (append-only log)      │      (spawn/track/shutdown)    │
└─────────────────────────┴───────────────────────────────┘
                          │
          ┌───────────────┼───────────────┐
          ▼               ▼               ▼
┌─────────────┐   ┌─────────────┐   ┌─────────────┐
│   Madison   │   │  Lexington  │   │    Park     │
│ (worktree)  │   │ (worktree)  │   │ (worktree)  │
│ Claude Code │   │ Claude Code │   │ Claude Code │
│ + plugin    │   │ + plugin    │   │ + plugin    │
└─────────────┘   └─────────────┘   └─────────────┘
```

## Key Concepts

### Lead

The **Lead** is your primary, human-facing Claude Code instance. It:
- Collaborates with you to plan & design what to build
- Gives you insight into the process
- Spawns coworkers when parallel work is needed
- Coordinates the team via the channel
- Reviews work from coworkers

The Lead has access to tools like `spawn_coworker`, `shutdown_coworker`, and `broadcast`.

### Coworkers

**Coworkers** are additional Claude Code instances, each running in:
- Their own isolated git worktree (no merge conflicts during development)
- Their own terminal session
- With automatic channel synchronization via the Stop hook

Coworkers:
- Grab open tasks & work them to completion
- Keep an eye on the channel for coordination
- Review open PRs when needed
- Merge their own PRs once approved

Coworkers have access to tools like `post_message`, `read_channel`, `claim_task`, and `request_review`.

Coworkers are named after Manhattan avenues (Madison, Lexington, Park, etc.) for easy reference.

### Channel

The **Channel** is an append-only message log where the team coordinates. Messages persist across sessions, so coworkers can catch up on what happened while they were offline.

The Stop hook automatically reads the channel at natural pause points, keeping coworkers in sync without explicit polling.

### Tasks

Midtown uses Claude Code's built-in task system (`TaskCreate`, `TaskList`, `TaskUpdate`). The Lead creates tasks, coworkers claim and complete them.

## Installation

### 1. Build and install the daemon

```bash
cargo build --release
cargo install --path .
```

### 2. Install the Claude Code plugin

The plugin is included in this repository. To use it:

```bash
# Option 1: Symlink to your plugins directory
ln -s $(pwd) ~/.claude/plugins/midtown

# Option 2: Copy the plugin
cp -r . ~/.claude/plugins/midtown
```

### 3. Enable the plugin in Claude Code

Add to your Claude Code settings:
```json
{
  "plugins": ["midtown"]
}
```

## Quick Start

### 1. Start the daemon

```bash
midtown start

# Or with verbose logging
midtown start --verbose
```

### 2. Check status

Use the `/status` command in Claude Code, or:

```bash
midtown status
```

### 3. Spawn a coworker

The Lead can use the `spawn_coworker` tool, or via CLI:

```bash
midtown coworker spawn
# => Spawned coworker: madison
```

### 4. Coordinate via channel

```bash
# Post a message
midtown channel post "Madison, please work on the auth tests"

# Read messages
midtown channel read
```

### 5. Stop when done

```bash
# Shutdown a specific coworker
midtown coworker shutdown madison

# Or stop everything
midtown stop
```

## Plugin Structure

```
midtown/
├── .claude-plugin/
│   └── plugin.json          # Plugin manifest
├── tools/
│   ├── lead/                # Lead-only tools
│   │   ├── spawn_coworker.md
│   │   ├── shutdown_coworker.md
│   │   └── broadcast.md
│   ├── coworker/            # Coworker-only tools
│   │   ├── post_message.md
│   │   ├── read_channel.md
│   │   ├── claim_task.md
│   │   └── request_review.md
│   └── shared/              # Tools for both roles
│       ├── list_coworkers.md
│       └── check_pr_status.md
├── hooks/
│   ├── hooks.json           # Hook configuration
│   └── scripts/
│       └── channel-sync.sh  # Stop hook for channel sync
├── commands/
│   ├── status.md            # /status command
│   └── channel.md           # /channel command
└── agents/
    ├── lead.md              # Lead agent definition
    └── coworker.md          # Coworker agent definition
```

## CLI Reference

```
midtown <COMMAND>

Commands:
    start     Start the midtown daemon
    stop      Stop the daemon and all coworkers
    status    Show system status (daemon, coworkers, tasks, PRs)
    channel   Channel messaging
    coworker  Coworker management
    task      Task operations
    pr        Pull request operations

Global Options:
    --format <FORMAT>  Output format: json or pretty [default: pretty]
```

### Daemon Commands

```bash
midtown start [--verbose]     # Start the daemon
midtown stop                  # Stop daemon and all coworkers
midtown status                # System overview
```

### Channel Commands

```bash
midtown channel post <MESSAGE>        # Send a message
midtown channel read                  # Read new messages (advances cursor)
midtown channel read --all            # Read all messages
midtown channel reset                 # Reset read cursor to beginning
```

### Coworker Commands

```bash
midtown coworker spawn                # Spawn a new coworker
midtown coworker list                 # List active coworkers
midtown coworker shutdown <NAME>      # Shutdown a coworker
midtown coworker nudge <NAME> <MSG>   # Send urgent message to coworker
```

### Task Commands

```bash
midtown task list                     # List tasks
midtown task create <SUBJECT>         # Create a task
midtown task claim <ID>               # Claim a task
midtown task complete <ID>            # Mark task complete
```

### PR Commands

```bash
midtown pr list                       # List open PRs
midtown pr review <COWORKER>          # Review PR from coworker
```

## How Coworker Sync Works

Coworkers stay synchronized via the Claude Code plugin's Stop hook. When Claude Code pauses, the hook automatically reads the channel:

```json
{
  "Stop": [{
    "matcher": "*",
    "hooks": [{
      "type": "command",
      "command": "bash ${CLAUDE_PLUGIN_ROOT}/hooks/scripts/channel-sync.sh",
      "timeout": 30
    }]
  }]
}
```

This means coworkers automatically receive team messages at natural pause points—no explicit polling required. The hook also checks for unclaimed tasks.

## Development

```bash
# Build
cargo build

# Run tests
cargo test

# Run daemon in development
cargo run -- start --verbose
```

### Project Structure

```
src/
├── main.rs               # Unified CLI entry point
├── lib.rs                # Library exports
├── daemon/               # Daemon implementation
│   ├── mod.rs
│   ├── server.rs         # Unix socket server
│   └── handlers.rs       # RPC request handlers
├── cli/                  # CLI subcommands
│   ├── mod.rs
│   ├── channel.rs
│   ├── coworker.rs
│   └── task.rs
├── channel.rs            # Append-only message log
├── coworker.rs           # Coworker lifecycle management
├── cursor.rs             # Per-agent read position
└── rpc.rs                # JSON-RPC 2.0 types
```

## License

MIT License - see [LICENSE](LICENSE) for details.
