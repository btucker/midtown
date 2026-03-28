# Daemon v2 Phase 5: Chat + Channels

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add channel operations (post, read, list) via RPC, and wire the Post command in the executor to write real messages to channel JSONL files. Reuse existing `Channel` and `Message` types from `src/channel.rs` and `src/message.rs`.

**Architecture:** The executor's `Post` and `PostSystem` commands use the existing `Channel::send()` API to write messages. RPC endpoints `channel.post`, `channel.read`, and `channel.list` query the filesystem directly (channels are JSONL files, not projection state). The ChannelIndex projection tracks metadata only.

**Tech Stack:** Rust, existing `Channel`/`Message` types

**Depends on:** Phase 4

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `src/daemon_v2/executor/channel_io.rs` | Create | Channel read/write via existing Channel API |
| `src/daemon_v2/executor/channel_io_tests.rs` | Create | Tests for channel I/O |
| `src/daemon_v2/executor/mod.rs` | Modify | Wire Post/PostSystem commands |
| `src/daemon_v2/rpc/handlers.rs` | Modify | Add channel.post, channel.read, channel.list |
| `src/daemon_v2/rpc/mod.rs` | Modify | Route new endpoints |
| `src/daemon_v2/rpc/rpc_tests.rs` | Modify | Tests for channel RPC |
| `src/daemon_v2/daemon.rs` | Modify | Pass channels_dir to executor |
| `tests/daemon_v2_e2e.rs` | Modify | E2E test for channel post and read |

---

### Task 1: Channel I/O executor functions

**Files:**
- Create: `src/daemon_v2/executor/channel_io.rs`
- Create: `src/daemon_v2/executor/channel_io_tests.rs`
- Modify: `src/daemon_v2/executor/mod.rs`

- [ ] **Step 1: Create channel_io_tests.rs**

```rust
use super::*;
use tempfile::TempDir;

#[test]
fn post_and_read_message() {
    let dir = TempDir::new().unwrap();
    let channels_dir = dir.path().to_path_buf();

    post_message(&channels_dir, "main", "ghost-town", "hello world", None).unwrap();
    post_message(&channels_dir, "main", "lead", "hi back", None).unwrap();

    let messages = read_messages(&channels_dir, "main", None).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["from"], "ghost-town");
    assert_eq!(messages[1]["content"], "hi back");
}

#[test]
fn post_system_message() {
    let dir = TempDir::new().unwrap();
    let channels_dir = dir.path().to_path_buf();

    post_system_message(&channels_dir, "main", "daemon started").unwrap();

    let messages = read_messages(&channels_dir, "main", None).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["from"], "midtown");
    assert_eq!(messages[0]["message_type"], "system");
}

#[test]
fn list_channels() {
    let dir = TempDir::new().unwrap();
    let channels_dir = dir.path().to_path_buf();

    post_message(&channels_dir, "main", "user", "msg1", None).unwrap();
    post_message(&channels_dir, "feature", "user", "msg2", None).unwrap();

    let channels = list_channels(&channels_dir).unwrap();
    assert!(channels.len() >= 2);
    assert!(channels.iter().any(|c| c.name == "main"));
    assert!(channels.iter().any(|c| c.name == "feature"));
}

#[test]
fn read_with_limit() {
    let dir = TempDir::new().unwrap();
    let channels_dir = dir.path().to_path_buf();

    for i in 0..5 {
        post_message(&channels_dir, "main", "user", &format!("msg {i}"), None).unwrap();
    }

    let messages = read_messages(&channels_dir, "main", Some(2)).unwrap();
    assert_eq!(messages.len(), 2);
    // Should return the last 2 messages
    assert_eq!(messages[0]["content"], "msg 3");
    assert_eq!(messages[1]["content"], "msg 4");
}
```

- [ ] **Step 2: Create channel_io.rs**

```rust
#[path = "channel_io_tests.rs"]
#[cfg(test)]
mod tests;

use crate::channel::{Channel, ChannelInfo};
use crate::message::Message;
use serde_json::Value;
use std::path::Path;

/// Post a text message to a channel.
pub fn post_message(
    channels_dir: &Path,
    channel: &str,
    sender: &str,
    content: &str,
    thread_id: Option<String>,
) -> Result<(), String> {
    let mut msg = Message::for_channel(
        channel,
        sender,
        content,
        crate::message::MessageType::Text,
    );
    msg.thread_parent_id = thread_id;

    let mut ch = Channel::new(channels_dir, channel)
        .map_err(|e| format!("failed to open channel {channel}: {e}"))?;
    ch.send(&msg)
        .map_err(|e| format!("failed to send message to {channel}: {e}"))?;
    Ok(())
}

/// Post a system message to a channel.
pub fn post_system_message(
    channels_dir: &Path,
    channel: &str,
    content: &str,
) -> Result<(), String> {
    let msg = Message::system(content);

    let mut ch = Channel::new(channels_dir, channel)
        .map_err(|e| format!("failed to open channel {channel}: {e}"))?;
    ch.send(&msg)
        .map_err(|e| format!("failed to send system message to {channel}: {e}"))?;
    Ok(())
}

/// Read messages from a channel, optionally limited to the last N.
pub fn read_messages(
    channels_dir: &Path,
    channel: &str,
    limit: Option<usize>,
) -> Result<Vec<Value>, String> {
    let ch = Channel::new(channels_dir, channel)
        .map_err(|e| format!("failed to open channel {channel}: {e}"))?;
    let all_messages = ch.read_all()
        .map_err(|e| format!("failed to read channel {channel}: {e}"))?;

    let messages: Vec<Value> = if let Some(n) = limit {
        all_messages.iter().rev().take(n).rev()
            .map(|m| m.to_json())
            .collect()
    } else {
        all_messages.iter().map(|m| m.to_json()).collect()
    };

    Ok(messages)
}

/// List all channels.
pub fn list_channels(channels_dir: &Path) -> Result<Vec<ChannelInfo>, String> {
    Channel::list(channels_dir)
        .map_err(|e| format!("failed to list channels: {e}"))
}
```

- [ ] **Step 3: Wire Post/PostSystem in executor/mod.rs**

Add `pub mod channel_io;` and handle the commands:

```rust
Command::Post { channel, sender, content, thread_id } => {
    if let Err(e) = channel_io::post_message(channels_dir, &channel, &sender, &content, thread_id) {
        tracing::error!(%e, "failed to post message");
    }
    vec![DomainEvent::MessagePosted {
        id: uuid::Uuid::new_v4().to_string(),
        channel, sender, content,
        thread_id: None,
    }]
}
Command::PostSystem { channel, content } => {
    if let Err(e) = channel_io::post_system_message(channels_dir, &channel, &content) {
        tracing::error!(%e, "failed to post system message");
    }
    vec![DomainEvent::MessagePosted {
        id: uuid::Uuid::new_v4().to_string(),
        channel, sender: "midtown".into(), content,
        thread_id: None,
    }]
}
```

The executor needs a `channels_dir: &Path` parameter. Add it to the execute() signature and pass from daemon.rs.

- [ ] **Step 4: Run tests and commit**

```bash
cargo test --lib daemon_v2
git commit -m "feat(daemon-v2): add channel I/O executor with post and read"
```

---

### Task 2: Channel RPC endpoints

**Files:**
- Modify: `src/daemon_v2/rpc/handlers.rs`
- Modify: `src/daemon_v2/rpc/mod.rs`
- Modify: `src/daemon_v2/rpc/rpc_tests.rs`

- [ ] **Step 1: Add handlers**

In handlers.rs, add:

```rust
pub fn handle_channel_list(channels_dir: &Path) -> Result<Value, RpcError> {
    let channels = channel_io::list_channels(channels_dir)
        .map_err(|e| RpcError { code: -32000, message: e })?;
    let result: Vec<Value> = channels.iter().map(|c| json!({
        "name": c.name,
        "archived": c.is_archived,
    })).collect();
    Ok(json!(result))
}

pub fn handle_channel_post(params: Option<&Value>, channels_dir: &Path) -> Result<(Value, Vec<DomainEvent>), RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let channel = params.get("channel").and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing channel"))?;
    let sender = params.get("sender").and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing sender"))?;
    let content = params.get("content").and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing content"))?;

    channel_io::post_message(channels_dir, channel, sender, content, None)
        .map_err(|e| RpcError { code: -32000, message: e })?;

    let event = DomainEvent::MessagePosted {
        id: uuid::Uuid::new_v4().to_string(),
        channel: channel.to_string(),
        sender: sender.to_string(),
        content: content.to_string(),
        thread_id: None,
    };

    Ok((json!({"ok": true}), vec![event]))
}

pub fn handle_channel_read(params: Option<&Value>, channels_dir: &Path) -> Result<Value, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let channel = params.get("channel").and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing channel"))?;
    let limit = params.get("limit").and_then(|v| v.as_u64()).map(|n| n as usize);

    let messages = channel_io::read_messages(channels_dir, channel, limit)
        .map_err(|e| RpcError { code: -32000, message: e })?;

    Ok(json!(messages))
}
```

- [ ] **Step 2: Route in rpc/mod.rs**

The dispatch_request() function needs channels_dir. Update signature to take `channels_dir: &Path`:

```rust
"channel.list" => {
    let result = handlers::handle_channel_list(channels_dir);
    (to_response(&id, result), vec![])
}
"channel.post" => {
    match handlers::handle_channel_post(params, channels_dir) {
        Ok((value, events)) => (json!({"jsonrpc": "2.0", "result": value, "id": id}), events),
        Err(err) => (err.to_json(&id), vec![]),
    }
}
"channel.read" => {
    let result = handlers::handle_channel_read(params, channels_dir);
    (to_response(&id, result), vec![])
}
```

- [ ] **Step 3: Pass channels_dir from daemon.rs**

Update daemon.rs to compute channels_dir and pass it through to dispatch_request().

- [ ] **Step 4: Add tests and commit**

```bash
cargo test --lib daemon_v2
git commit -m "feat(daemon-v2): add channel.post, channel.read, channel.list RPC endpoints"
```

---

### Task 3: E2E test — channel post and read

**Files:**
- Modify: `tests/daemon_v2_e2e.rs`

- [ ] **Step 1: Add E2E test**

```rust
#[test]
#[ignore]
fn test_daemon_v2_channel_post_and_read() {
    let harness = V2Harness::start();

    // Post a message
    let resp = harness.rpc_call("channel.post", Some(serde_json::json!({
        "channel": "main",
        "sender": "test-user",
        "content": "hello from e2e test",
    })));
    assert!(resp["error"].is_null(), "channel.post error: {resp}");

    // Read it back
    let resp = harness.rpc_call("channel.read", Some(serde_json::json!({
        "channel": "main",
        "limit": 5,
    })));
    assert!(resp["error"].is_null(), "channel.read error: {resp}");
    let messages = resp["result"].as_array().expect("should be array");
    assert!(messages.iter().any(|m| m["content"] == "hello from e2e test"),
        "posted message should appear in read: {messages:?}");
}
```

- [ ] **Step 2: Run ALL E2E tests**

```bash
cargo build && cargo test --test daemon_v2_e2e -- --ignored --test-threads=1
```

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(daemon-v2): add E2E test for channel post and read"
```

---

## Summary

After Phase 5:
- **Channel I/O** — post_message, post_system_message, read_messages, list_channels (reusing existing Channel/Message types)
- **RPC endpoints** — channel.post, channel.read, channel.list
- **Post/PostSystem commands** — wired in executor to write real JSONL files
- **E2E verified** — post a message via RPC, read it back
