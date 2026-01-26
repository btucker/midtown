# Midtown

Coordinate multiple **Claude Code** instances working on the same codebase. Midtown lets a human-facing Claude Code session (the "Lead") spawn and orchestrate additional Claude Code instances (the "Coworkers"), each working in isolated git worktrees.

## Why Midtown?

When you're working with Claude Code on a complex project, you might want to parallelize work:

- The Lead works on the main feature while a Coworker handles tests
- Multiple Coworkers implement independent components simultaneously
- A Coworker reviews PRs while the Lead continues development

Midtown provides the infrastructure for this coordination:

- **Coworker spawning** - Launch Claude Code instances in isolated git worktrees
- **Channel messaging** - Shared append-only message stream for team communication
- **Task coordination** - Coworkers claim tasks via Claude Code's native task system

## Key Concepts

### Lead

The **Lead** is your primary, human-facing Claude Code instance. It:
- Receives your instructions directly
- Spawns coworkers when parallel work is needed
- Coordinates the team via the channel
- Reviews work from coworkers

### Coworkers

**Coworkers** are additional Claude Code instances, each running in:
- Their own isolated git worktree (no merge conflicts during development)
- Their own terminal session
- With automatic channel synchronization via Claude Code hooks

Coworkers are named after Manhattan avenues (Madison, Lexington, Park, etc.) for easy reference.

### Channel

The **Channel** is an append-only message log where the team coordinates. Messages persist across sessions, so coworkers can catch up on what happened while they were offline.

### Tasks

Midtown uses Claude Code's built-in task system (`TaskCreate`, `TaskList`, `TaskUpdate`). The Lead creates tasks, coworkers claim and complete them.

## Quick Start

### 1. Start the daemon

```bash
midtown start

# Or with verbose logging
midtown start --verbose
```

### 2. Check status

```bash
midtown status
```

### 3. Spawn a coworker

```bash
midtown coworker spawn
# => Spawned coworker: madison
```

### 4. Send messages

```bash
# Lead posts to the channel
midtown channel post "Starting work on auth feature - Madison, please handle tests"

# Check what coworkers are seeing
midtown channel read
```

### 5. Stop when done

```bash
# Shutdown a specific coworker
midtown coworker shutdown madison

# Or stop everything
midtown stop
```

## Lead Walkthrough

Here's a typical workflow from the Lead's perspective:

### Starting a session

```bash
# Start the midtown daemon
midtown start

# Check system status
midtown status
# Shows: daemon running, no active coworkers, pending tasks
```

### Spawning coworkers

When you have parallel work, spawn coworkers:

```bash
# Spawn a coworker for implementing feature X
midtown coworker spawn
# => Spawned coworker: madison (worktree: ~/.midtown/worktrees/madison)

# Spawn another for writing tests
midtown coworker spawn
# => Spawned coworker: lexington (worktree: ~/.midtown/worktrees/lexington)
```

Each coworker gets:
- A fresh git worktree branched from your current commit
- A Claude Code session with channel sync hooks configured
- Access to the shared task list

### Coordinating via channel

Post messages to coordinate:

```bash
midtown channel post "Madison: implement the login endpoint. Lexington: write integration tests for auth."
```

Coworkers automatically see channel messages at their natural pause points (via Claude Code's Stop hook).

### Monitoring progress

```bash
# See all coworkers and their status
midtown coworker list

# Check recent channel activity
midtown channel read

# Full system overview
midtown status
```

### Reviewing work

Coworkers push their work to feature branches. When ready:

```bash
# List PRs from coworkers
midtown pr list

# Review a specific PR
midtown pr review madison
```

### Wrapping up

```bash
# Shutdown specific coworkers
midtown coworker shutdown madison
midtown coworker shutdown lexington

# Or shutdown all coworkers and stop the daemon
midtown stop
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
midtown channel read-all              # Read all messages
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

Coworkers stay synchronized via Claude Code's hook system. When spawned, each coworker is configured with a **Stop hook** that reads the channel whenever Claude Code pauses:

```json
{
  "hooks": {
    "Stop": [{
      "type": "command",
      "command": "midtown channel read"
    }]
  }
}
```

This means coworkers automatically receive team messages at natural pause points—no explicit polling required.

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                     Human Developer                      │
└─────────────────────────┬───────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────┐
│                   Lead (Claude Code)                     │
│                   main git worktree                      │
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
└─────────────┘   └─────────────┘   └─────────────┘
```

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
