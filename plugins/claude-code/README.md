# Midtown Claude Code Plugin

MCP (Model Context Protocol) plugin for Claude Code that provides tools for coordinating AI coworkers via the midtown daemon.

## Installation

### Prerequisites

1. Midtown daemon running (`midtownd`)
2. `midtown` CLI in PATH
3. Node.js 18+

### Build

```bash
cd plugins/claude-code
npm install
npm run build
```

### Configure Claude Code

Add to your Claude Code MCP settings (`~/.claude/settings.json` or project `.claude/settings.local.json`):

```json
{
  "mcpServers": {
    "midtown": {
      "command": "node",
      "args": ["/path/to/midtown/plugins/claude-code/dist/index.js"],
      "env": {
        "MIDTOWN_BIN": "/path/to/midtown"
      }
    }
  }
}
```

Or if installed globally:

```json
{
  "mcpServers": {
    "midtown": {
      "command": "midtown-mcp"
    }
  }
}
```

## Tools

### Lead Tools

These tools are intended for the lead/coordinator agent:

| Tool | Description |
|------|-------------|
| `spawn_coworker` | Spawn a new coworker agent |
| `shutdown_coworker` | Gracefully shutdown a coworker by name |
| `broadcast` | Post an announcement to all coworkers |

### Coworker Tools

These tools are for individual coworker agents:

| Tool | Description |
|------|-------------|
| `post_message` | Post a message to the team channel |
| `read_channel` | Read recent/unread messages |
| `claim_task` | Claim a task by ID |
| `request_review` | Request PR review from another coworker |

### Shared Tools

Available to both lead and coworkers:

| Tool | Description |
|------|-------------|
| `list_coworkers` | List all active coworkers |
| `check_pr_status` | Check PR CI/review status |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `MIDTOWN_BIN` | `midtown` | Path to midtown CLI binary |

## Example Usage

### Lead spawning a coworker

```
Use spawn_coworker to add a new team member
```

### Coworker claiming work

```
Use claim_task with task_id "mt-123" to claim the auth feature task
```

### Requesting PR review

```
Use request_review for PR #42 asking to focus on the error handling changes
```

## Architecture

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│   Claude Code   │────▶│  MCP Plugin     │────▶│  midtown CLI    │
│                 │     │  (this package) │     │                 │
└─────────────────┘     └─────────────────┘     └────────┬────────┘
                                                         │
                                                         ▼
                                                ┌─────────────────┐
                                                │ midtown daemon  │
                                                │  (midtownd)     │
                                                └─────────────────┘
```

The plugin acts as a bridge between Claude Code's MCP protocol and the midtown CLI, which communicates with the daemon over Unix socket.

## Development

```bash
# Watch mode for development
npm run dev

# Test the server manually
echo '{"jsonrpc":"2.0","method":"tools/list","id":1}' | node dist/index.js
```

## License

MIT
