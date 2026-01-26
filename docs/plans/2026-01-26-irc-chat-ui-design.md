# IRC-Style Chat UI for Midtown

## Overview

Add a TUI chat pane to the Lead's tmux window showing team activity and coworker status. Coworkers use IRC conventions (especially `/me`) to communicate what they're doing. The chat provides real-time visibility into team coordination.

## Goals

- Give the human a persistent view of team activity without switching windows
- Enable coworkers to broadcast their current status via `/me`
- Surface task updates automatically via Claude Code hooks
- Display GitHub events (CI, PRs, reviews) as chat messages

## Non-Goals

- Interactive input in the chat TUI (Lead posts via `midtown channel post`)
- Chat panes in coworker windows (only Lead gets the chat)
- Historical message search or filtering

## Design

### Message Type: Action

Add `MessageType::Action` to represent `/me` messages:

```rust
pub enum MessageType {
    Text,      // Regular message: <name> message
    System,    // System notification: <system> message
    Command,   // Command/instruction
    Status,    // Status update
    Error,     // Error notification
    Action,    // /me action: * name action
}
```

When posting `/me something`, the CLI:
1. Detects the `/me ` prefix
2. Strips the prefix
3. Stores the message with `MessageType::Action`

Display in chat uses IRC convention:
```
10:33 * lexington investigating the auth bug
```

### TUI Layout

```
┌─ Team ─────────────┬─ #midtown ─────────────────────────────┐
│ lexington          │ 10:32 <park> pushed PR #42             │
│  investigating bug │ 10:33 * lexington investigating bug    │
│ park               │ 10:35 <github> CI passed on PR #42     │
│  pushed PR #42     │ 10:36 <lexington> found the issue,     │
│ madison            │       it's in the auth middleware      │
│  (idle)            │ 10:38 <Lead> nice work, claim task 3   │
│                    │       when you're done                 │
│                    │                                        │
└────────────────────┴────────────────────────────────────────┘
```

**Left panel (~30% width): "Team"**
- Lists active coworkers from daemon
- Shows each coworker's name and last `/me` action
- Two-line format: name on first line, action (indented) on second
- Shows "(idle)" if no recent action

**Right panel (~70% width): "#midtown"**
- Scrolling chat log from channel.jsonl
- Auto-scrolls to newest messages at bottom
- Message formats:
  - Regular: `HH:MM <name> message`
  - Action: `HH:MM * name action`
  - System: `HH:MM <system> message`

**Colors** (assigned by position in avenue list):
- lexington → cyan
- park → green
- madison → yellow
- broadway → magenta
- amsterdam → blue
- columbus → red
- (continue for overflow names)
- Lead → white/bold
- github → gray
- system → dim white

### Tmux Integration

The chat TUI runs in a pane on the right side of the Lead window only.

During `midtown start`, after creating the Lead window:
```bash
# Split Lead window, new pane on right at 30% width
tmux split-window -h -t midtown-{project}:Lead -p 30

# Start chat TUI in the new pane
tmux send-keys -t midtown-{project}:Lead.1 "midtown chat" Enter

# Keep focus on the main pane (Claude Code)
tmux select-pane -t midtown-{project}:Lead.0
```

### Claude Code Hooks for Task Updates

Add PostToolUse hooks to coworker settings so task activity auto-posts to channel:

```json
{
    "hooks": {
        "Stop": [{
            "hooks": [{"type": "command", "command": "midtown --format json coworker stop-hook"}]
        }],
        "PostToolUse": [{
            "matcher": {"toolName": "TaskUpdate"},
            "hooks": [{"type": "command", "command": "midtown coworker task-hook"}]
        }, {
            "matcher": {"toolName": "TaskCreate"},
            "hooks": [{"type": "command", "command": "midtown coworker task-hook"}]
        }]
    }
}
```

The `task-hook` command:
1. Receives tool use context via stdin (JSON with tool name and result)
2. Parses the task operation (claim, complete, create)
3. Posts appropriate message to channel as Action type

Example outputs:
- `* lexington claimed task 5: Fix auth middleware`
- `* lexington completed task 5`
- `<Lead> created task 7: Add unit tests`

### Coworker System Prompt Updates

Update the prompt to encourage IRC-style usage:

```markdown
## Channel Usage
The channel works like IRC. Post updates to keep the team informed:
```bash
midtown channel post "your message here"
```

Use `/me` to indicate what you're currently doing:
```bash
midtown channel post "/me investigating the auth bug"
midtown channel post "/me running test suite"
midtown channel post "/me opening PR for task 3"
```

Your `/me` status appears in the team sidebar, so keep it current.

Post when:
- Starting work: `/me claiming task 5`
- Making progress: `/me found the issue in auth.rs`
- Finishing: `/me opened PR #42 for review`
- Blocked: `blocked on task 3, need API spec clarified`
- Questions: `@Lead should this handle the edge case?`
```

### GitHub as Chat Participant

Webhook messages use `from: "github"` instead of system messages:

```rust
// In webhook.rs, when creating messages for GitHub events:
Message::new("github", content, MessageType::Text)
```

This gives GitHub its own color in the chat and makes it feel like a team member.

### TUI Binary

New binary: `midtown chat` (or `midtown-chat`)

**Dependencies:**
- `ratatui` - TUI framework
- `crossterm` - Terminal backend
- `notify` or polling - Watch channel file for changes

**Behavior:**
1. Connect to daemon via Unix socket to get coworker list
2. Read channel.jsonl and render messages
3. Watch for file changes, re-render on updates
4. Poll daemon periodically for coworker list updates
5. Handle terminal resize gracefully
6. Exit cleanly on Ctrl+C or when pane is closed

## File Changes

### New Files
- `src/bin/midtown-chat/main.rs` - Entry point, terminal setup
- `src/bin/midtown-chat/app.rs` - Application state, event loop
- `src/bin/midtown-chat/ui.rs` - Ratatui rendering

### Modified Files
- `Cargo.toml` - Add ratatui, crossterm, notify dependencies; add midtown-chat binary
- `src/message.rs` - Add `MessageType::Action`
- `src/bin/midtown/cli/channel.rs` - Parse `/me` prefix when posting
- `src/bin/midtown/cli/coworker.rs` - Add `task-hook` subcommand
- `src/tmux.rs` - Update `coworker_settings_json()` and `coworker_system_prompt()`
- `src/webhook.rs` - Use `from: "github"` for webhook messages
- `src/daemon.rs` - Split Lead window and launch chat on startup

## Testing

- Unit tests for `/me` parsing in channel post
- Unit tests for Action message serialization
- Manual testing of TUI layout and colors
- Integration test: post message, verify appears in chat
- Integration test: task hook posts to channel

## Future Enhancements (Out of Scope)

- Input line for human to type directly in chat
- Message filtering/search
- Notifications/alerts for @mentions
- Chat pane in coworker windows
- Persistent scroll position
