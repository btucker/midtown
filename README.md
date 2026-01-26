# Midtown

Coordinate multiple **Claude Code** instances working on the same codebase. Midtown lets a human-facing Claude Code session (the "Lead") spawn and orchestrate additional Claude Code instances (the "Coworkers"), each working in isolated git worktrees.

## The Workflow

Midtown is designed for a specific collaboration pattern:

1. **You and the Lead collaborate on design** - Talk through the problem, explore the codebase, sketch out an approach
2. **The Lead creates a plan and tasks** - Breaks down the work into independent pieces using Claude Code's task system
3. **Coworkers claim tasks and work autonomously** - Each coworker grabs a task, works it to completion, opens a PR
4. **Coworkers review each other's PRs** - While waiting or between tasks, coworkers review open PRs
5. **The Lead coordinates and merges** - Ensures quality, resolves conflicts, keeps work aligned with the plan

The human stays focused on high-level decisions while the agents handle parallel implementation.

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
│               (Unix socket: ~/.local/state/midtown/)         │
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

## Key Concepts

### Lead

The Lead is your primary Claude Code instance. It:

- Collaborates with you on design and planning
- Creates tasks for the team
- Spawns coworkers when parallel work is needed
- Reviews work and coordinates merges
- Has access to: `midtown coworker spawn`, `midtown coworker shutdown`, `midtown channel post`

### Coworkers

Coworkers are additional Claude Code instances that work autonomously. Each runs in:

- An isolated git worktree (no merge conflicts during development)
- A tmux window within the project session
- With a Stop hook that syncs the channel at natural pause points

Coworkers:

- Check for unclaimed tasks and claim one
- Work the task to completion and open a PR
- Look for PRs to review when between tasks
- Post updates to the channel

Coworkers are named after Manhattan avenues: lexington, park, madison, broadway, amsterdam, columbus, riverside, york, pleasant, vernon.

### Channel

The channel is an append-only message log where the team coordinates. It works like Slack - post updates, ask questions, share status. The Stop hook reads the channel automatically, so coworkers stay in sync without explicit polling.

### Tasks

Midtown uses Claude Code's built-in task system. The Lead creates tasks, coworkers claim and complete them. The Stop hook checks for unclaimed tasks, nudging idle coworkers to pick up work.

## Installation

### 1. Build and install

```bash
cargo build --release
cargo install --path .
```

### 2. Verify installation

```bash
midtown --version
```

## Quick Start

### 1. Start midtown

From your project directory:

```bash
midtown start
```

This starts the daemon and creates a tmux session with the Lead window.

### 2. Attach to the session

```bash
midtown attach
```

You're now in the Lead's Claude Code instance.

### 3. Collaborate with the Lead

Work with the Lead to understand the problem and design a solution. Once you have a plan:

```
You: Let's break this into tasks for the team.

Lead: I'll create tasks for each component:
- Task 1: Implement the authentication middleware
- Task 2: Add the user profile endpoints
- Task 3: Write integration tests

Now let me spawn some coworkers to help.
```

### 4. Spawn coworkers

The Lead can spawn coworkers:

```bash
midtown coworker spawn
# => Spawned coworker: lexington
```

Or via the channel:

```bash
midtown channel post "lexington, please claim task 1 and get started"
```

### 5. Monitor progress

Check status anytime:

```bash
midtown status
```

Shows: active coworkers, open tasks, open PRs, recent channel activity.

### 6. Navigate the session

- `Ctrl-b w` - List all windows (Lead + coworkers)
- `Ctrl-b n` - Next window
- `Ctrl-b p` - Previous window
- `Ctrl-b 0-9` - Jump to window by number

### 7. Stop when done

```bash
midtown stop
```

## CLI Reference

```
midtown <COMMAND>

Commands:
  start       Start the daemon and tmux session
  stop        Stop daemon and all coworkers
  restart     Restart midtown
  attach      Attach to the tmux session
  status      Show system status

  channel     Channel messaging
    post <MSG>    Post a message
    read          Read new messages
    read --all    Read all messages

  coworker    Coworker management
    spawn         Spawn a new coworker
    list          List active coworkers
    shutdown <N>  Shutdown coworker by name
    nudge <N>     Send reminder to coworker

  task        Task operations
    create <SUBJECT>  Create a task
    claim <ID>        Claim a task
    done <ID>         Mark complete

Options:
  --format <FORMAT>   Output: json or pretty [default: pretty]
```

## How Sync Works

Coworkers stay synchronized via the Claude Code Stop hook. When Claude pauses, the hook:

1. Reads new channel messages
2. Checks for unclaimed tasks
3. Reports a summary

This means coworkers automatically receive updates at natural pause points. If there are unclaimed tasks, they're reminded to grab one.

## GitHub Webhook Integration

Midtown automatically receives GitHub webhooks for PR events, CI status, and review comments. Events appear in the channel so coworkers see them at their next sync.

This works out of the box - just be logged into `gh`:

```bash
gh auth login  # if not already logged in
midtown start
```

Midtown detects the repo, installs the `gh-webhook` extension if needed, and starts forwarding events automatically.

### Supported events

- **Pull requests**: opened, merged, closed, ready for review
- **Reviews**: approved, changes requested, commented
- **CI status**: check runs completed (pass/fail)
- **Comments**: PR comments and review comments

### Configuration

```bash
# Disable webhooks
export MIDTOWN_WEBHOOK_PORT=0

# Use a different port (default: 47022)
export MIDTOWN_WEBHOOK_PORT=8080
```

### Server deployment

For servers with a public IP, configure GitHub webhooks directly instead of using `gh webhook forward`:

```bash
export MIDTOWN_WEBHOOK_SECRET=$(openssl rand -hex 32)
midtown start
```

Add a webhook in your repo's GitHub settings pointing to `http://your-server:47022/webhook` with the same secret.

## Development

```bash
# Build
cargo build

# Run tests
cargo test

# Start in development
cargo run -- start
```

## Project Structure

```
src/
├── lib.rs              # Library exports
├── daemon.rs           # Unix socket server
├── channel.rs          # Append-only message log
├── coworker.rs         # Coworker lifecycle
├── tmux.rs             # Tmux session/window management
├── webhook.rs          # GitHub webhook handling
├── nudge/              # Coworker reminder system
└── bin/midtown/        # CLI implementation
```

## License

MIT
