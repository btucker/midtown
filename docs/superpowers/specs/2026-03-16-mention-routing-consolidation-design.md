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
    task_thread_id: &HashMap<String, String>,    // task_id → thread_id
    task_channel: &HashMap<String, String>,      // task_id → channel
    spans: &[TaskSessionSpan],                   // historical name→session per task
) -> Option<MentionTarget>
```

Resolution order:

1. **Thread-scoped** (when `thread_id` is `Some`):
   - Reverse-lookup `task_thread_id` to find the task tied to this thread.
   - Search `spans` for a span where `agent_name == mentioned_name` and `task_id` matches.
   - If multiple spans match, prefer the most recent by `start_time`.
   - Return that span's `session_id`.

2. **Channel-scoped** (fallback when no thread or no thread match):
   - Filter `task_channel` to tasks in this channel.
   - Search `spans` for spans where `agent_name == mentioned_name` and task is in this channel.
   - **Prefer in-progress tasks** over completed ones. Among same-status tasks, pick the most recent by `start_time`.
   - Return that span's `session_id`.

3. **No match** → return `None`.

Note: Resolution is purely about "who should get this?" — capacity constraints (dev limit, resource exhaustion) are handled at the delivery layer by `TaskPrompt`/`deliver_task_prompt()`, not at the resolution layer.

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

The caller also emits `NudgeChannelLead` for each channel that has a lead, so `@all` reaches both task workers and channel leads.

### Simplified `route_mentions()` in `chat.rs`

The existing `route_mentions()` collapses to:

```
route_mentions(state, msg):
  1. if contains_at_all:
     → resolve_all_mention() → emit TaskPrompt for each target
     → emit NudgeChannelLead for each active channel lead
  2. extract_mentions(msg.content)
  3. for each mentioned name:
     - if name is a channel lead → NudgeChannelLead (existing, unchanged)
     - else → resolve_mention(name, msg.thread_parent_id, channel, ...)
       - if Some(target) → emit TaskPrompt { task_id, message }
       - if None → post system message ("couldn't resolve @name")
```

### System Message Routing

System/daemon-generated messages (from `SKIP_SENDERS`) that mention channel lead names still need routing. The current `chat_monitor_loop()` has special-casing for `@lead` and `@ops` in these messages. After consolidation:

- `@lead` is removed as a special token. The project lead is reachable by its channel lead name.
- `@ops` is a channel lead name like any other — resolved via step 3 above.
- System messages that mention channel lead names are routed through the same `route_mentions()` path. The `chat_monitor_loop` skip logic is adjusted to still call `route_mentions()` for system messages before continuing (preserving the current pattern but using the unified path).

### Mention Message Formatting

Currently, mention messages are formatted through two separate codepaths:
1. `render_thread_context()` in `chat.rs` — produces a freeform string for `Nudge` wake reasons
2. `WakeReason::Mention` in `wake_reason.rs` — structured wake reason for channel lead nudges

Both call `ThreadContext::reply_instructions()` for thread context, but only when the mention is in a thread. Top-level mentions get no instructions at all. Additionally, `reply_instructions()` includes "IMPORTANT: Keep text output brief or omit it — text output auto-posts as a top-level message" which only applies to leads/forks, not to task workers receiving mentions.

After consolidation, all mentions go through a single formatting path that produces context-dependent instructions:

- **Task worker in a thread**: Include thread reply commands (`--thread`, `--channel`), thread read command. No output suppression warning (task workers post via `midtown channel post`, not stdout).
- **Task worker top-level**: Include channel post command. No thread commands needed.
- **Channel lead in a thread**: Include thread reply/read commands. Include output suppression warning (leads' stdout auto-posts to channel).
- **Channel lead top-level**: No thread commands. Include output suppression warning.

The "IMPORTANT: Keep text output brief" line is removed from `ThreadContext::reply_instructions()` and instead added only when the target is a lead/fork session.

### Delivery via `TaskPrompt`

No changes to the core delivery mechanism. `deliver_task_prompt()` in `rpc_task.rs` already handles:
- Session running → nudge via stdin.
- Session stopped → resume with the prompt as initial message.

`deliver_task_prompt()` looks up the session via `SessionRecord` by task ID. This works for both dev and reviewer sessions since `SessionRecord` stores the `session_id` and `coworker_type` — the resume path already uses `SessionMode::ResumeSession` which preserves session type.

### Session Name Preference on Resume

`SessionRecord` already has a `preferred_name: Option<String>` field. When `route_mentions()` resolves a mention and the target session needs resuming, set `preferred_name` on the `SessionRecord` to the mentioned name before emitting the `TaskPrompt`. If the name is available in the `NamePool`, it will be used. Otherwise, normal pool allocation applies.

### Edge Cases

| Scenario | Behavior |
|----------|----------|
| Mention in a thread with no task | Falls through to channel-scoped resolution |
| Mention of a name never used | `resolve_mention()` returns `None`, system message posted |
| Multiple mentions in one message | Each resolved independently, each gets own `TaskPrompt` |
| Self-mention | Skipped before calling `resolve_mention()` |
| Duplicate mention (same name, same message) | Existing cooldown-based dedup, checked before resolution |
| `@all` with no in-progress tasks | Empty vec from `resolve_all_mention()`, channel leads still nudged |
| `@all` resumes stopped sessions | Intentional — `@all` reaches all in-progress task sessions, running or not |
| Channel-scoped with completed and in-progress tasks | In-progress tasks preferred over completed |

## Code Changes

### New

- `resolve_mention()` pure function in `rules.rs`
- `resolve_all_mention()` pure function in `rules.rs`
- `MentionTarget` struct in `rules.rs`

### Simplified

- `route_mentions()` in `chat.rs` — collapses to extract → resolve → `TaskPrompt`
- `chat_monitor_loop()` in `chat.rs` — system message routing uses unified `route_mentions()` instead of `@lead`/`@ops` special cases
- `handle_channel_post()` in `rpc_channel.rs` — `@lead` detection replaced with unified `route_mentions()` path

### Removed

- `decide_mention_action()` in `rules.rs`
- `mention_action_to_effects()` in `chat.rs`
- `render_thread_context()` in `chat.rs` — replaced by unified mention formatting
- `!N` task-based rerouting in `route_mentions()`
- `@lead` special-casing in `chat_monitor_loop()`
- `@ops` special-casing in `chat_monitor_loop()`
- "Keep text output brief" warning from `ThreadContext::reply_instructions()`

### Unchanged

- `TaskPrompt` effect and `deliver_task_prompt()` delivery logic
- `extract_mentions()` helper
- Effect execution system
- Entry points (`chat_monitor_loop()`, `handle_channel_post()`, webhook handler) — still the same callers, just simplified internally

## Key Invariant

Resolution is always by **session ID** (from `TaskSessionSpan` history), never by name lookup on live sessions. The mentioned name is only used as a cosmetic preference when assigning a name from the pool during resume.
