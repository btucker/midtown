# Research: Claude Code Agent Teams Mailbox System

## Task #836: Explore replacing tmux send-keys nudges with Claude agent teams sendMessage

### Executive Summary

The Claude Code agent teams system includes a **filesystem-based mailbox** at `~/.claude/teams/{team-name}/inboxes/{agent-name}.json` that can deliver messages to running Claude Code instances without using tmux send-keys. This document describes the mailbox format, delivery mechanism, and feasibility of using it as a replacement or complement to midtown's current nudge system.

**Verdict: Feasible as a complement, but cannot fully replace tmux send-keys today.** The mailbox can deliver structured messages to running Claude Code instances from external processes (the daemon), but it requires the coworker to be launched with agent-teams CLI flags (`--agent-id`, `--agent-name`, `--team-name`) that midtown doesn't currently use.

---

### 1. How the Current Nudge System Works

Midtown's current nudge system (`src/tmux.rs`, `src/coworker.rs`):

1. **`tmux send-keys -l`** sends literal text to the coworker's tmux pane
2. **Waits 500ms** for paste to complete
3. **Sends `Enter`** with up to 3 retry attempts
4. **Verifies** nudge was submitted by checking if text is still on the input line

**Known problems:**
- Can collide with agent tool calls (text appears mid-execution)
- Can get lost if the agent is busy processing
- Can corrupt terminal state
- Requires a `wait_for_nudge_safe()` pre-check that polls pane content for input stability
- Has a retry loop for stuck nudges (Enter not processed)
- Blocking thread sleeps (500ms + retries)

### 2. Claude Code Agent Teams Mailbox Architecture

#### 2.1 Filesystem Layout

```
~/.claude/teams/{team-name}/
├── config.json          # Team config (members array)
└── inboxes/
    ├── {agent-name}.json   # Per-agent inbox (JSON array of messages)
    └── {agent-name}.json.lock  # File lock for concurrent access
```

#### 2.2 Agent ID Format

Agent IDs follow the pattern: `{agent-name}@{team-name}`

- Example: `lexington@midtown-project`
- The `lDT()` function parses this format back into `{agentName, teamName}`
- The `QU()` function constructs it from name + team

#### 2.3 Inbox File Format

Each inbox file (`{agent-name}.json`) is a JSON array of message objects:

```json
[
  {
    "text": "The message content",
    "from": "team-lead",
    "color": "blue",
    "timestamp": "2025-02-06T00:00:00.000Z",
    "read": false,
    "summary": "Brief summary for UI"
  }
]
```

Fields:
- **`text`** (string): The message content. Can be plain text or JSON-stringified structured protocol messages.
- **`from`** (string): Sender's agent name
- **`color`** (string, optional): Sender's display color
- **`timestamp`** (string): ISO 8601 timestamp
- **`read`** (boolean): Whether the message has been consumed by the recipient
- **`summary`** (string, optional): Brief summary for UI preview

#### 2.4 Structured Protocol Messages

The `text` field can contain JSON-encoded protocol messages for special operations:

```json
// Shutdown request
{
  "type": "shutdown_request",
  "requestId": "shutdown-agent@team-1234567890",
  "from": "team-lead",
  "reason": "Task complete"
}

// Idle notification
{
  "type": "idle_notification",
  "from": "agent-name",
  "idleReason": "available",
  "summary": "..."
}

// Task assignment
{
  "type": "task_assignment",
  ...
}
```

#### 2.5 Concurrency Control

- Uses **file-level locking** via the `lockfile` npm package (`.lock` files)
- Lock acquisition is synchronous (`lockSync()`)
- Lock is always released in a `finally` block
- Reads and writes are atomic within the lock scope

#### 2.6 Message Polling / Delivery

Claude Code polls its inbox for unread messages during **attachment collection** — the phase between API responses where the system gathers context for the next turn. The `readUnreadMessages()` function filters for `read: false` messages. After processing, messages are marked as read via `markMessagesAsRead()`.

Key function: `iDT()` (readUnreadMessages) → filters inbox for unread → returns them as attachments → they appear as teammate messages in the conversation.

The polling happens on the main event loop, checking at each turn boundary. This means:
- Messages are delivered **between turns** (after each model response)
- Messages are NOT delivered **during** a tool execution
- When idle (waiting for input), the polling happens on a separate interval

#### 2.7 Two Backend Types

Claude Code has two backend implementations for teammate communication:

1. **InProcessBackend** (`SOB` class): For in-process subagents. Messages go through `writeToMailbox()` → inbox file → polled by the agent.

2. **PaneBackendExecutor** (`nOB` class): For tmux/iTerm2 pane-based agents. Also uses `writeToMailbox()` for messages, but spawns agents in separate tmux panes or iTerm2 panes.

Both backends share the same mailbox infrastructure. The `PaneBackendExecutor.sendMessage()` method is:
```javascript
async sendMessage(agentId, message) {
    const { agentName, teamName } = parseAgentId(agentId);
    writeToMailbox(agentName, {
        text: message.text,
        from: message.from,
        color: message.color,
        timestamp: message.timestamp ?? new Date().toISOString()
    }, teamName);
}
```

### 3. Requirements for External Message Delivery

To write to a running Claude Code instance's mailbox from the daemon:

1. **Coworker must be launched with agent-teams flags:**
   - `--agent-id {name}@{team-name}`
   - `--agent-name {name}`
   - `--team-name {team-name}`
   - `--agent-color {color}` (optional)
   - `--parent-session-id {uuid}`

2. **Team config must exist** at `~/.claude/teams/{team-name}/config.json` with a `members` array.

3. **Inbox directory must exist:** `~/.claude/teams/{team-name}/inboxes/`

4. **The `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS` env var must be `"1"`** (exported as a shell env var in the tmux launch command — cannot be set via settings.json as it is blocklisted).

5. **Message format must match** the expected inbox JSON schema.

### 4. What the Daemon Would Need to Do

To deliver a message to a coworker:

```rust
// Pseudocode for daemon-side mailbox write
fn write_to_mailbox(agent_name: &str, team_name: &str, message: MailboxMessage) -> Result<()> {
    let inbox_dir = home_dir().join(".claude/teams").join(team_name).join("inboxes");
    fs::create_dir_all(&inbox_dir)?;

    let inbox_path = inbox_dir.join(format!("{}.json", agent_name));
    let lock_path = inbox_dir.join(format!("{}.json.lock", agent_name));

    // Acquire lock using lockfile protocol (mkdir-based, matching Claude Code's `lockfile` npm package)
    let _lock = lockfile_lock(&lock_path)?;

    // Read existing messages
    let mut messages: Vec<MailboxMessage> = if inbox_path.exists() {
        serde_json::from_str(&fs::read_to_string(&inbox_path)?)?
    } else {
        vec![]
    };

    // Append new message
    messages.push(MailboxMessage {
        text: message.text,
        from: message.from,
        color: message.color,
        timestamp: chrono::Utc::now().to_rfc3339(),
        read: false,
        summary: message.summary,
    });

    // Write back atomically
    fs::write(&inbox_path, serde_json::to_string_pretty(&messages)?)?;
    Ok(())
}
```

### 5. Changes Required in Midtown

#### 5.1 Launch Config Changes (src/tmux.rs)

Add agent-teams CLI flags to `to_shell_command()`:

```rust
// In ClaudeLaunchConfig::to_shell_command()
// After existing args...
let team_name = format!("midtown-{}", repo_name);
args.push("--agent-id".to_string());
args.push(format!("{}@{}", self.name, team_name));
args.push("--agent-name".to_string());
args.push(self.name.clone());
args.push("--team-name".to_string());
args.push(team_name);
args.push("--agent-color".to_string());
args.push(color_for_name(&self.name));
```

#### 5.2 Team Config Setup

Before spawning any coworker, create `~/.claude/teams/{team-name}/config.json`:

```json
{
  "members": [
    { "name": "lexington", "agentId": "lexington@midtown-repo", "agentType": "coworker" },
    { "name": "park", "agentId": "park@midtown-repo", "agentType": "coworker" }
  ]
}
```

#### 5.3 Mailbox Writer Module (new: src/mailbox.rs)

A new module implementing the mailbox write protocol in Rust. **Must use the `lockfile` npm package's locking protocol** (mkdir-based `.lock` directory creation), NOT `fs2` OS-level file locks. Claude Code's reader uses `lockfile`, so the daemon writer must match. See Section 6.2 for details on the two locking mechanisms.

#### 5.4 Dual Delivery Strategy

The daemon could use both channels:
- **Mailbox** for routine messages (task assignments, PR review requests, status queries)
- **tmux send-keys** retained for edge cases where immediate interrupt is needed (e.g., session recovery, urgent shutdown)

### 6. Limitations and Risks

#### 6.1 Delivery Latency
- Messages are polled between turns, not pushed in real-time
- If the agent is in a long tool execution, message delivery is delayed until the next turn boundary
- When idle, there's a separate polling interval (implementation-specific, likely 500ms-1s)

#### 6.2 Lock Contention
- **The daemon must use the `lockfile` npm package's locking protocol**, not `fs2` OS-level locks. These are fundamentally different mechanisms:
  - `lockfile` (Claude Code's reader): Creates a `.lock` **directory** via `mkdir` (atomic on POSIX). Lock is held while the directory exists. Stale lock detection uses mtime.
  - `fs2` (midtown's channel.rs): Uses OS-level `flock()`/`fcntl()` file locks. Invisible to `lockfile`-based readers.
- Using `fs2` would provide **no mutual exclusion** with Claude Code — both sides would read/write simultaneously, risking data corruption.
- Implementation: Create `{path}.lock` directory with `mkdir`, remove on unlock. Handle stale locks (check mtime, remove if older than threshold).

#### 6.3 Agent Name Sanitization
- The `HGT()` function sanitizes agent names for use as filenames
- We'd need to replicate this sanitization in Rust
- From the code: it appears to be a simple slug-like transformation

#### 6.4 Message Accumulation
- The inbox file grows as messages accumulate
- `clearMailbox()` resets it to `[]`, but only on explicit clear
- `markMessagesAsRead()` sets `read: true` but doesn't remove messages
- Long-running coworkers could accumulate many messages

#### 6.5 No Delivery Guarantee
- Writing to the inbox doesn't guarantee the message will be processed
- If the Claude Code instance crashes before polling, messages are lost
- No acknowledgment protocol for regular messages

#### 6.6 Compatibility Risk
- The mailbox format is part of an experimental feature (`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS`)
- Format could change in future Claude Code releases
- No public API contract — we're relying on implementation details

### 7. Comparison: tmux send-keys vs Mailbox

| Aspect | tmux send-keys | Mailbox |
|--------|---------------|---------|
| Delivery latency | Immediate (terminal injection) | Polled (turn boundary) |
| Reliability during tool use | Fragile (can collide) | Safe (queued) |
| Reliability when idle | Good (prompt available) | Good (polling active) |
| Terminal corruption risk | Yes | No |
| Retry complexity | High (stuck detection) | None needed |
| Structured messages | No (plain text only) | Yes (JSON protocol) |
| Shutdown protocol | N/A (ad-hoc text) | Built-in (shutdown_request/response) |
| Implementation effort | Existing | New Rust module needed |
| External process support | N/A | Yes (filesystem-based) |
| Claude Code version coupling | Low (tmux is stable) | High (experimental feature) |

### 8. Recommendation

**Phase 1: Complement, not replace.** Add mailbox-based messaging as an alternative delivery channel alongside tmux send-keys:

1. Add `--agent-id`, `--agent-name`, `--team-name` flags to coworker launch
2. Implement `src/mailbox.rs` for writing to the inbox from the daemon
3. Use mailbox for non-urgent messages (task assignments, PR feedback notifications)
4. Keep tmux send-keys for session recovery interrupts and immediate-priority nudges

**Phase 2: Evaluate migration.** After observing the mailbox system in production:
- Monitor delivery latency and reliability
- Track Claude Code version changes to the mailbox format
- If stable and reliable, gradually migrate more nudge types to mailbox
- tmux send-keys may still be needed for nudging the Lead (human-facing, not an agent-teams member)

### 9. Prototype Verification

To verify the mailbox approach works, a minimal test:

1. Launch a coworker with agent-teams flags
2. From the daemon, write a message to their inbox
3. Observe whether the coworker receives it as a new conversation turn
4. Measure delivery latency

This can be tested manually before implementing the full Rust module.

---

*Research conducted by broadway, 2026-02-06. Based on reverse engineering Claude Code binary strings and source analysis of the agent teams messaging implementation.*
