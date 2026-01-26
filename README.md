# Midtown

Coordinate multiple AI agents working on the same codebase. Midtown provides durable message channels so agents can communicate, share status, and hand off work without losing messages—even when they start and stop at different times.

## Why Midtown?

When multiple AI agents work on related tasks (code review, implementation, testing), they need to coordinate:

- **Agent A** starts implementing a feature, then pauses
- **Agent B** picks up the work and needs to know where Agent A left off
- **Agent C** is reviewing PRs and needs to flag blocking issues

Midtown solves this with append-only message channels. Each agent maintains its own read cursor, so messages are never lost and agents can catch up on what happened while they were offline.

## Quick Start

### 1. Start the daemon

```bash
# Build and run
cargo build --release
./target/release/midtownd

# Or with verbose logging
./target/release/midtownd --verbose
```

### 2. Use the CLI

```bash
# Check daemon status
./target/release/midtown status

# Send a message to a channel
./target/release/midtown channel send --repo myproject --message "Starting work on auth feature"

# Read messages (uses cursor - only shows unread)
./target/release/midtown channel read --repo myproject --agent agent-1
```

### 3. Programmatic access (via Unix socket)

```bash
# Health check
echo '{"jsonrpc":"2.0","method":"ping","id":1}' | socat - UNIX-CONNECT:/tmp/midtown.sock
# Returns: {"jsonrpc":"2.0","result":"pong","id":1}
```

## Concepts

### Channels

A channel is an append-only log of messages, stored as JSONL at `~/.midtown/<repo>/channel.jsonl`. Messages have:

- **id** - Unique identifier (UUID)
- **timestamp** - When the message was sent
- **from** - Who sent it (agent name or "system")
- **content** - The message body
- **type** - One of: `text`, `system`, `command`, `status`, `error`

### Cursors

Each agent has a cursor tracking its read position. When an agent reads from a channel, it only sees messages after its cursor—then the cursor advances. Cursors are stored in `~/.midtown/<repo>/cursors/<agent>.json`.

This enables the "catch up" pattern: an agent that was offline can read all messages it missed, while an agent that's been continuously reading sees only new messages.

## CLI Reference

The `midtown` CLI communicates with the daemon for all operations.

```
midtown [OPTIONS] <COMMAND>

Options:
    --format <FORMAT>  Output format: json or pretty [default: pretty]

Commands:
    channel   Channel messaging commands
    coworker  Coworker management commands
    task      Task management commands
    status    Show system status
    pr        Pull request commands
```

### Channel Commands

```bash
# Send a message
midtown channel send --repo <REPO> --message "your message"

# Read new messages (advances cursor)
midtown channel read --repo <REPO> --agent <AGENT_NAME>

# Read all messages (ignores cursor)
midtown channel read-all --repo <REPO>

# Reset cursor to beginning
midtown channel reset --repo <REPO> --agent <AGENT_NAME>
```

## Daemon Reference

### Starting

```bash
midtownd [OPTIONS]

Options:
    -s, --socket <PATH>  Unix socket path [default: /tmp/midtown.sock]
    -v, --verbose        Enable debug logging
```

### Stopping

- `SIGTERM` or `SIGINT` (Ctrl+C) - Graceful shutdown
- RPC: `{"jsonrpc":"2.0","method":"shutdown","id":1}`

### RPC Methods

| Method | Description |
|--------|-------------|
| `ping` | Health check, returns `"pong"` |
| `version` | Returns daemon name and version |
| `shutdown` | Initiates graceful shutdown |

### Error Codes

| Code | Description |
|------|-------------|
| -32700 | Parse error - Invalid JSON |
| -32600 | Invalid request |
| -32601 | Method not found |
| -32602 | Invalid params |
| -32603 | Internal error |

## Coworker Channel Synchronization

When coworkers are spawned via `midtown coworker spawn`, they are automatically configured with a Claude Code **Stop hook** that reads the channel whenever the agent pauses. This provides natural synchronization points without explicit polling.

### How It Works

1. Lead spawns a coworker via CLI or MCP plugin
2. The coworker's Claude Code session starts with a Stop hook configured
3. Whenever the agent finishes responding and waits for input, the hook runs `midtown channel read`
4. Any new messages from teammates are injected into the agent's context

This means coworkers automatically stay in sync with team communication at their natural pause points—no manual coordination required.

### Hook Configuration

The Stop hook is automatically injected when spawning coworkers:

```json
{
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "midtown channel read"
          }
        ]
      }
    ]
  }
}
```

### Manual Configuration

If running Claude Code manually (not via `midtown coworker spawn`), add the hook to your settings:

- `~/.claude/settings.json` (user-level)
- `.claude/settings.json` (project-level)
- `.claude/settings.local.json` (local, not committed)

## Example: Agent Handoff Workflow

```bash
# Agent 1 starts work
midtown channel send --repo myproject --message "Starting auth implementation"
midtown channel send --repo myproject --message "Completed login endpoint, pausing"

# Agent 2 comes online later and catches up
midtown channel read --repo myproject --agent agent-2
# Shows both messages from Agent 1

# Agent 2 continues work
midtown channel send --repo myproject --message "Resuming auth work, adding logout endpoint"

# Agent 1 comes back and sees only the new message
midtown channel read --repo myproject --agent agent-1
# Shows only Agent 2's message
```

## Development

```bash
# Build
cargo build

# Run tests
cargo test

# Run daemon in development
cargo run --bin midtownd -- --verbose
```

### Project Structure

```
src/
├── bin/
│   ├── midtownd/main.rs  # Daemon entry point
│   └── midtown/          # CLI binary
│       ├── main.rs       # CLI entry point
│       ├── cli/          # Subcommand handlers
│       └── client.rs     # Daemon RPC client
├── lib.rs                # Library exports
├── rpc.rs                # JSON-RPC 2.0 types
├── channel.rs            # Append-only message log
├── cursor.rs             # Per-agent read position
└── message.rs            # Message types
```

## License

MIT License - see [LICENSE](LICENSE) for details.
