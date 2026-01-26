# Midtown

Coordinate multiple **Claude Code** instances working on the same codebase. Midtown lets a human-facing Claude Code session (the "Lead") spawn and orchestrate additional Claude Code instances (the "Coworkers"), each working in isolated git worktrees.

## Why Midtown?

Midtown is inspired by [Gas Town](https://github.com/anthropics/gastown), Anthropic's full-featured multi-agent orchestration system. But where Gas Town is a sophisticated engine with beads, molecules, refineries, witnesses, mail systems, and complex workflows, Midtown takes a deliberately **simpler approach**.

### The "Mid" Philosophy

Midtown is intentionally "mid"—not trying to achieve everything Gas Town does. Instead, it:

- **Leans into Claude Code's native features** - Tasks, hooks, and the agent system are already built into Claude Code. Midtown uses them rather than reinventing them.
- **Keeps the coordination model simple** - One shared channel vs. Gas Town's complex mail routing and molecule orchestration.
- **Minimizes moving parts** - Easier to understand, debug, and operate.

### Slack-Like Messaging at the Core

At its heart, Midtown is built around a **Slack-like channel**: a single shared message stream where team members (both the human-facing Lead and autonomous Coworkers) post updates, coordinate handoffs, and stay in sync. Each Claude Code instance reads the channel at natural pause points—just like checking a team chat.

| Gas Town | Midtown |
|----------|---------|
| Beads (work items with complex lifecycle) | Claude Code's native Tasks |
| Molecules (workflow templates) | Simple channel coordination |
| Refineries (orchestration engines) | Daemon (spawn/track coworkers) |
| Witnesses (supervisors) | Lead (human-facing session) |
| Mail system (routed messaging) | Channel (shared message log) |

### When to Use Midtown

Choose Midtown when you want multi-agent coordination without the operational complexity of Gas Town:

- The Lead works on the main feature while a Coworker handles tests
- Multiple Coworkers implement independent components simultaneously
- A Coworker reviews PRs while the Lead continues development

Midtown provides just enough infrastructure:

- **Channel messaging** - Slack-like append-only message stream for team communication
- **Coworker spawning** - Launch Claude Code instances in isolated git worktrees
- **Task coordination** - Coworkers claim tasks via Claude Code's native task system

For teams that need the full power of cross-repo orchestration, complex approval workflows, or enterprise-scale agent coordination, Gas Town remains the right tool.

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
