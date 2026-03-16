# Mention Routing Consolidation

## Problem

Mention routing is scattered across multiple code paths with different resolution strategies. The current implementation resolves `@name` by checking which session currently holds that name (`name_to_session`), which can route to an unrelated session that happens to have been assigned the same reusable name. There is no thread or channel context awareness.

## Goals

1. **Context-based resolution**: Resolve `@name` to the correct historical session using thread and channel context, not whoever currently holds the name.
2. **Consolidate decisioning**: Replace scattered mention logic with a single pure resolution function.
3. **Always resume**: Stopped sessions are always resumed when mentioned — mentions are never dropped.
4. **Never misroute**: Never send a mention to an unrelated session that happens to hold the name.

## Design

### Core Resolution Function

A new pure function in `rules.rs`:

```rust
pub struct MentionTarget {
    pub task_id: String,
    pub session_id: String,
}

pub fn resolve_mention(
    mentioned_name: &str,
    thread_id: Option<&str>,
    channel: &str,
    task_thread_map: &HashMap<String, String>,   // task_id → thread_id
    task_channel_map: &HashMap<String, String>,   // task_id → channel
    spans: &[TaskSessionSpan],                    // historical name→session per task
) -> Option<MentionTarget>
```

Resolution order:

1. **Thread-scoped** (when `thread_id` is `Some`):
   - Reverse-lookup `task_thread_map` to find the task tied to this thread.
   - Search `spans` for a span where `agent_name == mentioned_name` and `task_id` matches.
   - Return that span's `session_id`.

2. **Channel-scoped** (fallback when no thread or no thread match):
   - Filter `task_channel_map` to tasks in this channel.
   - Search `spans` for spans where `agent_name == mentioned_name` and task is in this channel.
   - Pick the most recent by `start_time`.
   - Return that span's `session_id`.

3. **No match** → return `None`.

### `@all` Handling

A separate pure function in `rules.rs`:

```rust
pub fn resolve_all_mention(
    in_progress_tasks: &[Task],
    spans: &[TaskSessionSpan],
) -> Vec<MentionTarget>
```

- For each in-progress task, find the most recent open span.
- Deduplicate by `session_id`.
- Returns all targets; caller emits a `TaskPrompt` effect for each.

### Simplified `route_mentions()` in `chat.rs`

The existing `route_mentions()` collapses to:

```
route_mentions(state, msg):
  1. if contains_at_all → resolve_all_mention() → emit TaskPrompt for each
  2. extract_mentions(msg.content)
  3. for each mentioned name:
     - if name is a channel lead → NudgeChannelLead (existing, unchanged)
     - else → resolve_mention(name, msg.thread_parent_id, channel, ...)
       - if Some(target) → emit TaskPrompt { task_id, message }
       - if None → post system message ("couldn't resolve @name")
```

### Delivery via `TaskPrompt`

No changes to the delivery mechanism. `deliver_task_prompt()` in `rpc_task.rs` already handles:
- Session running → nudge via stdin.
- Session stopped → resume with the prompt as initial message.

### Session Name Preference on Resume

When `TaskPrompt` resumes a stopped session, accept an optional `preferred_name: Option<String>`. If the preferred name is available in the `NamePool`, use it. Otherwise, fall back to normal pool allocation.

`route_mentions()` passes the mentioned name as the preference. Non-mention callers pass `None`.

### Edge Cases

| Scenario | Behavior |
|----------|----------|
| Mention in a thread with no task | Falls through to channel-scoped resolution |
| Mention of a name never used | `resolve_mention()` returns `None`, system message posted |
| Multiple mentions in one message | Each resolved independently, each gets own `TaskPrompt` |
| Self-mention | Skipped before calling `resolve_mention()` |
| Duplicate mention (same name, same message) | Existing cooldown-based dedup, checked before resolution |
| `@all` with no in-progress tasks | Empty vec returned, no effects emitted |

## Code Changes

### New

- `resolve_mention()` pure function in `rules.rs`
- `resolve_all_mention()` pure function in `rules.rs`
- `MentionTarget` struct in `rules.rs`
- `preferred_name` parameter on session resume path in `deliver_task_prompt()`

### Simplified

- `route_mentions()` in `chat.rs` — collapses to extract → resolve → `TaskPrompt`

### Removed

- `decide_mention_action()` in `rules.rs`
- `mention_action_to_effects()` in `chat.rs`
- `!N` task-based rerouting in `route_mentions()`
- `@lead` special-casing in `chat_monitor_loop()`
- `@ops` special-casing in `chat_monitor_loop()` (becomes normal channel lead mention)

### Unchanged

- Entry points (`chat_monitor_loop()`, `handle_channel_post()`, webhook handler)
- `TaskPrompt` effect and `deliver_task_prompt()` delivery logic
- `extract_mentions()` helper
- Effect execution system

## Key Invariant

Resolution is always by **session ID** (from `TaskSessionSpan` history), never by name lookup on live sessions. The mentioned name is only used as a cosmetic preference when assigning a name from the pool during resume.
