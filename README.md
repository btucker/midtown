# Midtown

Coordinate multiple **Claude Code** instances working on the same codebase using native Claude Code tasks.

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

### GitHub Webhooks

Midtown automatically receives GitHub webhooks for PR events, CI status, and review comments. Events appear in the channel so coworkers see them at their next sync.

```bash
gh auth login  # if not already logged in
midtown start  # webhooks work automatically
```

## License

MIT
