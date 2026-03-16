# Mention Routing Consolidation Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Consolidate scattered @mention routing into a single pure resolution function that uses thread/channel context from `TaskSessionSpan` history, replacing the current name-to-live-session approach.

**Architecture:** New pure functions `resolve_mention()` and `resolve_all_mention()` in `rules.rs` replace `decide_mention_action()` and `mention_action_to_effects()`. All mention paths emit `TaskPrompt` effects for delivery. The existing `route_mentions()` in `chat.rs` is simplified to extract → resolve → emit.

**Tech Stack:** Rust, existing effect system, `TaskSessionSpan` data in `DaemonPersistentState`

**Spec:** `docs/superpowers/specs/2026-03-16-mention-routing-consolidation-design.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `src/rules.rs` | Modify | Add `MentionTarget`, `resolve_mention()`, `resolve_all_mention()`; remove `MentionAction`, `decide_mention_action()` |
| `src/rules_mention_tests.rs` | Create | Tests for `resolve_mention()` and `resolve_all_mention()` |
| `src/daemon/chat.rs` | Modify | Simplify `route_mentions()` and `route_at_all()`; remove `mention_action_to_effects()`, `ReviewerSessionInfo` |
| `src/daemon/chat_tests.rs` | Modify | Replace old `mention_action_to_effects` tests with `resolve_mention` integration |
| `src/daemon/chat.rs` (chat_monitor_loop) | Modify | Remove `@lead`/`@ops` special cases; route system messages through unified path |
| `src/tasks.rs` | Modify | Add `get_in_progress_task_ids_for_repo()` helper |
| `src/daemon/rpc_channel.rs` | Modify | Remove `@lead`/`@{project_name}` special case (lines 552-598) |

---

## Chunk 1: Pure Resolution Functions

### Task 1: `resolve_mention()` — thread-scoped resolution

**Files:**
- Create: `src/rules_mention_tests.rs`
- Modify: `src/rules.rs`

- [ ] **Step 1: Write the failing test for thread-scoped resolution**

Create `src/rules_mention_tests.rs`:

```rust
use super::*;
use crate::daemon::state::TaskSessionSpan;
use chrono::Utc;
use std::collections::HashMap;

#[test]
fn resolve_mention_thread_scoped_finds_session_for_task() {
    // Thread "thread-1" is tied to task "42".
    // Span: "amsterdam" worked on task "42" in session "sess-ams-1".
    let task_thread_id: HashMap<String, String> =
        [("42".into(), "thread-1".into())].into_iter().collect();
    let task_channel: HashMap<String, String> =
        [("42".into(), "dev".into())].into_iter().collect();
    let spans = vec![TaskSessionSpan {
        task_id: "42".into(),
        agent_name: "amsterdam".into(),
        agent_type: "dev".into(),
        session_id: "sess-ams-1".into(),
        start_time: Utc::now(),
        end_time: None,
    }];

    let result = resolve_mention(
        "amsterdam",
        Some("thread-1"),
        "dev",
        &task_thread_id,
        &task_channel,
        &spans,
    );

    assert_eq!(result.as_ref().map(|t| t.task_id.as_str()), Some("42"));
    assert_eq!(result.as_ref().map(|t| t.session_id.as_str()), Some("sess-ams-1"));
}
```

- [ ] **Step 2: Wire test module into `rules.rs`**

Add at the bottom of `src/rules.rs`, near the other test module declarations:

```rust
#[path = "rules_mention_tests.rs"]
#[cfg(test)]
mod rules_mention_tests;
```

- [ ] **Step 3: Add `MentionTarget` struct and stub `resolve_mention`**

Add to `src/rules.rs`:

```rust
/// Resolved target for an @mention — the task and session to deliver to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionTarget {
    pub task_id: String,
    pub session_id: String,
}

/// Resolve an @mention to the correct session using thread and channel context.
///
/// Resolution order:
/// 1. Thread-scoped: if the mention is in a thread, find the task tied to that
///    thread and the session that used this name for that task.
/// 2. Channel-scoped: find the most recent task in this channel where this name
///    was used, preferring in-progress tasks over completed ones.
/// 3. No match → None.
pub fn resolve_mention(
    mentioned_name: &str,
    thread_id: Option<&str>,
    channel: &str,
    task_thread_id: &std::collections::HashMap<String, String>,
    task_channel: &std::collections::HashMap<String, String>,
    spans: &[crate::daemon::state::TaskSessionSpan],
) -> Option<MentionTarget> {
    // Step 1: Thread-scoped resolution
    if let Some(tid) = thread_id {
        // Reverse-lookup: find task_id whose thread matches
        let task_id = task_thread_id.iter()
            .find(|(_, v)| v.as_str() == tid)
            .map(|(k, _)| k.as_str());

        if let Some(task_id) = task_id {
            let best = spans.iter()
                .filter(|s| s.task_id == task_id
                    && s.agent_name.eq_ignore_ascii_case(mentioned_name))
                .max_by_key(|s| s.start_time);

            if let Some(span) = best {
                return Some(MentionTarget {
                    task_id: span.task_id.clone(),
                    session_id: span.session_id.clone(),
                });
            }
        }
    }

    // Step 2: Channel-scoped resolution (fallback)
    // TODO: implement in next task
    None
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test resolve_mention_thread_scoped_finds_session`
Expected: PASS

- [ ] **Step 5: Write test for thread-scoped with multiple spans (picks most recent)**

Add to `src/rules_mention_tests.rs`:

```rust
#[test]
fn resolve_mention_thread_scoped_picks_most_recent_span() {
    let task_thread_id: HashMap<String, String> =
        [("42".into(), "thread-1".into())].into_iter().collect();
    let task_channel: HashMap<String, String> =
        [("42".into(), "dev".into())].into_iter().collect();
    let earlier = Utc::now() - chrono::Duration::hours(2);
    let later = Utc::now();
    let spans = vec![
        TaskSessionSpan {
            task_id: "42".into(),
            agent_name: "amsterdam".into(),
            agent_type: "dev".into(),
            session_id: "sess-old".into(),
            start_time: earlier,
            end_time: Some(earlier + chrono::Duration::hours(1)),
        },
        TaskSessionSpan {
            task_id: "42".into(),
            agent_name: "amsterdam".into(),
            agent_type: "dev".into(),
            session_id: "sess-new".into(),
            start_time: later,
            end_time: None,
        },
    ];

    let result = resolve_mention(
        "amsterdam",
        Some("thread-1"),
        "dev",
        &task_thread_id,
        &task_channel,
        &spans,
    );

    assert_eq!(result.as_ref().map(|t| t.session_id.as_str()), Some("sess-new"));
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test resolve_mention_thread_scoped_picks_most_recent`
Expected: PASS

- [ ] **Step 7: Write test for thread with no matching task (falls through)**

Add to `src/rules_mention_tests.rs`:

```rust
#[test]
fn resolve_mention_thread_with_no_task_falls_through_to_channel() {
    // Thread "orphan-thread" is not in task_thread_id
    let task_thread_id: HashMap<String, String> = HashMap::new();
    let task_channel: HashMap<String, String> = HashMap::new();
    let spans = vec![];

    let result = resolve_mention(
        "amsterdam",
        Some("orphan-thread"),
        "dev",
        &task_thread_id,
        &task_channel,
        &spans,
    );

    assert!(result.is_none(), "No task for thread → should return None (until channel fallback is implemented)");
}
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test resolve_mention_thread_with_no_task`
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add src/rules.rs src/rules_mention_tests.rs
git commit -m "feat: add resolve_mention() with thread-scoped resolution"
```

---

### Task 2: `resolve_mention()` — channel-scoped fallback

**Files:**
- Modify: `src/rules.rs`
- Modify: `src/rules_mention_tests.rs`

- [ ] **Step 1: Write failing test for channel-scoped resolution**

Add to `src/rules_mention_tests.rs`:

```rust
#[test]
fn resolve_mention_channel_scoped_finds_most_recent_in_channel() {
    let task_thread_id: HashMap<String, String> = HashMap::new(); // no thread context
    let task_channel: HashMap<String, String> = [
        ("10".into(), "dev".into()),
        ("20".into(), "dev".into()),
    ].into_iter().collect();
    let earlier = Utc::now() - chrono::Duration::hours(2);
    let later = Utc::now();
    let spans = vec![
        TaskSessionSpan {
            task_id: "10".into(),
            agent_name: "broadway".into(),
            agent_type: "dev".into(),
            session_id: "sess-old".into(),
            start_time: earlier,
            end_time: Some(earlier + chrono::Duration::hours(1)),
        },
        TaskSessionSpan {
            task_id: "20".into(),
            agent_name: "broadway".into(),
            agent_type: "dev".into(),
            session_id: "sess-new".into(),
            start_time: later,
            end_time: None,
        },
    ];

    let result = resolve_mention(
        "broadway",
        None, // no thread
        "dev",
        &task_thread_id,
        &task_channel,
        &spans,
    );

    assert_eq!(result.as_ref().map(|t| t.session_id.as_str()), Some("sess-new"));
    assert_eq!(result.as_ref().map(|t| t.task_id.as_str()), Some("20"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test resolve_mention_channel_scoped_finds_most_recent`
Expected: FAIL (channel fallback is a TODO returning None)

- [ ] **Step 3: Write test for in-progress tasks preferred over completed**

To test this, we need task status. The resolution function needs to know which tasks are in-progress. Add a `in_progress_task_ids` parameter, or we can derive it from spans (open spans = in-progress). Let's use the simpler approach: pass a `&HashSet<String>` of in-progress task IDs.

Add to `src/rules_mention_tests.rs`:

```rust
#[test]
fn resolve_mention_channel_scoped_prefers_in_progress_over_completed() {
    let task_thread_id: HashMap<String, String> = HashMap::new();
    let task_channel: HashMap<String, String> = [
        ("old-completed".into(), "dev".into()),
        ("current-wip".into(), "dev".into()),
    ].into_iter().collect();
    // The completed task has a MORE RECENT span, but the in-progress task should win.
    let spans = vec![
        TaskSessionSpan {
            task_id: "current-wip".into(),
            agent_name: "park".into(),
            agent_type: "dev".into(),
            session_id: "sess-wip".into(),
            start_time: Utc::now() - chrono::Duration::hours(5),
            end_time: None,
        },
        TaskSessionSpan {
            task_id: "old-completed".into(),
            agent_name: "park".into(),
            agent_type: "dev".into(),
            session_id: "sess-done".into(),
            start_time: Utc::now() - chrono::Duration::hours(1),
            end_time: Some(Utc::now()),
        },
    ];
    let in_progress: std::collections::HashSet<String> =
        ["current-wip".to_string()].into_iter().collect();

    let result = resolve_mention(
        "park",
        None,
        "dev",
        &task_thread_id,
        &task_channel,
        &spans,
        &in_progress,
    );

    assert_eq!(result.as_ref().map(|t| t.task_id.as_str()), Some("current-wip"));
    assert_eq!(result.as_ref().map(|t| t.session_id.as_str()), Some("sess-wip"));
}
```

**Note:** This test adds an `in_progress_task_ids: &HashSet<String>` parameter. Update the function signature and all existing tests to include this parameter. For thread-scoped tests, pass an empty set (thread resolution doesn't use it).

- [ ] **Step 4: Update `resolve_mention()` signature and implement channel-scoped fallback**

Update `resolve_mention` in `src/rules.rs` — add `in_progress_task_ids` parameter and implement the channel-scoped fallback after the thread-scoped block:

```rust
pub fn resolve_mention(
    mentioned_name: &str,
    thread_id: Option<&str>,
    channel: &str,
    task_thread_id: &std::collections::HashMap<String, String>,
    task_channel: &std::collections::HashMap<String, String>,
    spans: &[crate::daemon::state::TaskSessionSpan],
    in_progress_task_ids: &std::collections::HashSet<String>,
) -> Option<MentionTarget> {
    // Step 1: Thread-scoped resolution (unchanged)
    // ...

    // Step 2: Channel-scoped resolution
    // Find tasks in this channel where the mentioned name has a span.
    let channel_spans: Vec<&crate::daemon::state::TaskSessionSpan> = spans.iter()
        .filter(|s| s.agent_name.eq_ignore_ascii_case(mentioned_name))
        .filter(|s| task_channel.get(&s.task_id).is_some_and(|ch| ch == channel))
        .collect();

    if channel_spans.is_empty() {
        return None;
    }

    // Prefer in-progress tasks over completed ones.
    // Among same priority, pick the most recent by start_time.
    let best = channel_spans.iter()
        .max_by(|a, b| {
            let a_in_progress = in_progress_task_ids.contains(&a.task_id);
            let b_in_progress = in_progress_task_ids.contains(&b.task_id);
            a_in_progress.cmp(&b_in_progress)
                .then(a.start_time.cmp(&b.start_time))
        });

    best.map(|span| MentionTarget {
        task_id: span.task_id.clone(),
        session_id: span.session_id.clone(),
    })
}
```

- [ ] **Step 5: Update all existing tests to include the new `in_progress_task_ids` parameter**

Add `&std::collections::HashSet::new()` as the last argument to all `resolve_mention()` calls in the thread-scoped tests (thread resolution doesn't use it, so empty is correct).

- [ ] **Step 6: Run all tests to verify they pass**

Run: `cargo test resolve_mention`
Expected: ALL PASS

- [ ] **Step 7: Write test for no match in channel**

Add to `src/rules_mention_tests.rs`:

```rust
#[test]
fn resolve_mention_no_match_returns_none() {
    let task_thread_id: HashMap<String, String> = HashMap::new();
    let task_channel: HashMap<String, String> =
        [("42".into(), "dev".into())].into_iter().collect();
    // "broadway" has a span, but we're looking for "amsterdam"
    let spans = vec![TaskSessionSpan {
        task_id: "42".into(),
        agent_name: "broadway".into(),
        agent_type: "dev".into(),
        session_id: "sess-bway".into(),
        start_time: Utc::now(),
        end_time: None,
    }];

    let result = resolve_mention(
        "amsterdam",
        None,
        "dev",
        &task_thread_id,
        &task_channel,
        &spans,
        &std::collections::HashSet::new(),
    );

    assert!(result.is_none());
}
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test resolve_mention_no_match`
Expected: PASS

- [ ] **Step 9: Write test for thread miss falling through to channel**

Add to `src/rules_mention_tests.rs`:

```rust
#[test]
fn resolve_mention_thread_miss_falls_through_to_channel() {
    // Thread "thread-1" maps to task "42", but "amsterdam" didn't work on task "42".
    // However "amsterdam" did work on task "99" in the same channel.
    let task_thread_id: HashMap<String, String> =
        [("42".into(), "thread-1".into())].into_iter().collect();
    let task_channel: HashMap<String, String> = [
        ("42".into(), "dev".into()),
        ("99".into(), "dev".into()),
    ].into_iter().collect();
    let spans = vec![TaskSessionSpan {
        task_id: "99".into(),
        agent_name: "amsterdam".into(),
        agent_type: "dev".into(),
        session_id: "sess-ams-99".into(),
        start_time: Utc::now(),
        end_time: None,
    }];

    let result = resolve_mention(
        "amsterdam",
        Some("thread-1"), // thread context, but no match for amsterdam on task 42
        "dev",
        &task_thread_id,
        &task_channel,
        &spans,
        &std::collections::HashSet::new(),
    );

    // Should fall through to channel-scoped and find task 99
    assert_eq!(result.as_ref().map(|t| t.task_id.as_str()), Some("99"));
    assert_eq!(result.as_ref().map(|t| t.session_id.as_str()), Some("sess-ams-99"));
}
```

- [ ] **Step 10: Run test to verify it passes**

Run: `cargo test resolve_mention_thread_miss_falls_through`
Expected: PASS

- [ ] **Step 11: Commit**

```bash
git add src/rules.rs src/rules_mention_tests.rs
git commit -m "feat: add channel-scoped fallback to resolve_mention()"
```

---

### Task 3: `resolve_all_mention()`

**Files:**
- Modify: `src/rules.rs`
- Modify: `src/rules_mention_tests.rs`

- [ ] **Step 1: Write failing test**

Add to `src/rules_mention_tests.rs`:

```rust
#[test]
fn resolve_all_mention_returns_targets_for_in_progress_tasks() {
    let spans = vec![
        TaskSessionSpan {
            task_id: "10".into(),
            agent_name: "amsterdam".into(),
            agent_type: "dev".into(),
            session_id: "sess-ams".into(),
            start_time: Utc::now(),
            end_time: None,
        },
        TaskSessionSpan {
            task_id: "20".into(),
            agent_name: "broadway".into(),
            agent_type: "dev".into(),
            session_id: "sess-bway".into(),
            start_time: Utc::now(),
            end_time: None,
        },
    ];
    let in_progress_task_ids: std::collections::HashSet<String> =
        ["10".to_string(), "20".to_string()].into_iter().collect();

    let targets = resolve_all_mention(&in_progress_task_ids, &spans);

    assert_eq!(targets.len(), 2);
    assert!(targets.iter().any(|t| t.task_id == "10" && t.session_id == "sess-ams"));
    assert!(targets.iter().any(|t| t.task_id == "20" && t.session_id == "sess-bway"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test resolve_all_mention_returns_targets`
Expected: FAIL (function doesn't exist yet)

- [ ] **Step 3: Implement `resolve_all_mention()`**

Add to `src/rules.rs`:

```rust
/// Resolve @all — find all sessions with in-progress tasks.
///
/// Returns one `MentionTarget` per in-progress task (most recent open span).
/// Deduplicates by session_id so a session working on multiple tasks is only
/// targeted once.
pub fn resolve_all_mention(
    in_progress_task_ids: &std::collections::HashSet<String>,
    spans: &[crate::daemon::state::TaskSessionSpan],
) -> Vec<MentionTarget> {
    let mut seen_sessions = std::collections::HashSet::new();
    let mut targets = Vec::new();

    for task_id in in_progress_task_ids {
        // Find the most recent span for this task.
        // Prefer open spans (end_time is None), but fall back to the most
        // recent closed span if no open span exists (e.g., task was reassigned
        // and the current session's span was closed but the task is still
        // in-progress).
        let best = spans.iter()
            .filter(|s| s.task_id == *task_id)
            .max_by(|a, b| {
                let a_open = a.end_time.is_none();
                let b_open = b.end_time.is_none();
                a_open.cmp(&b_open).then(a.start_time.cmp(&b.start_time))
            });

        if let Some(span) = best {
            if seen_sessions.insert(span.session_id.clone()) {
                targets.push(MentionTarget {
                    task_id: span.task_id.clone(),
                    session_id: span.session_id.clone(),
                });
            }
        }
    }

    targets
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test resolve_all_mention`
Expected: PASS

- [ ] **Step 5: Write test for session deduplication**

Add to `src/rules_mention_tests.rs`:

```rust
#[test]
fn resolve_all_mention_deduplicates_by_session_id() {
    // Same session working on two tasks — should only appear once
    let spans = vec![
        TaskSessionSpan {
            task_id: "10".into(),
            agent_name: "amsterdam".into(),
            agent_type: "dev".into(),
            session_id: "sess-shared".into(),
            start_time: Utc::now(),
            end_time: None,
        },
        TaskSessionSpan {
            task_id: "20".into(),
            agent_name: "amsterdam".into(),
            agent_type: "dev".into(),
            session_id: "sess-shared".into(),
            start_time: Utc::now(),
            end_time: None,
        },
    ];
    let in_progress: std::collections::HashSet<String> =
        ["10".to_string(), "20".to_string()].into_iter().collect();

    let targets = resolve_all_mention(&in_progress, &spans);

    assert_eq!(targets.len(), 1, "Same session should only appear once");
    assert_eq!(targets[0].session_id, "sess-shared");
}
```

- [ ] **Step 6: Write test for empty in-progress tasks**

Add to `src/rules_mention_tests.rs`:

```rust
#[test]
fn resolve_all_mention_empty_when_no_in_progress_tasks() {
    let spans = vec![TaskSessionSpan {
        task_id: "10".into(),
        agent_name: "amsterdam".into(),
        agent_type: "dev".into(),
        session_id: "sess-ams".into(),
        start_time: Utc::now(),
        end_time: Some(Utc::now()),
    }];
    let in_progress: std::collections::HashSet<String> = std::collections::HashSet::new();

    let targets = resolve_all_mention(&in_progress, &spans);

    assert!(targets.is_empty());
}
```

- [ ] **Step 7: Run all tests**

Run: `cargo test resolve_all_mention`
Expected: ALL PASS

- [ ] **Step 8: Commit**

```bash
git add src/rules.rs src/rules_mention_tests.rs
git commit -m "feat: add resolve_all_mention() for @all broadcast"
```

---

## Chunk 2: Simplify `route_mentions()` and Remove Old Code

### Task 4: Simplify `route_mentions()` to use `resolve_mention()`

**Files:**
- Modify: `src/daemon/chat.rs`

- [ ] **Step 1: Rewrite `route_mentions()` to use new resolution functions**

Replace the body of `route_mentions()` in `src/daemon/chat.rs` (lines 143-274). The new implementation:

```rust
pub(super) async fn route_mentions(state: &DaemonState, msg: &Message) {
    // Check for @all broadcast first
    if contains_at_all(&msg.content) {
        route_at_all(state, msg).await;
        return;
    }

    let mentions = extract_mentions(&msg.content);
    if mentions.is_empty() {
        return;
    }

    debug!(
        "Found {} @mention(s) in message from {}: {:?}",
        mentions.len(),
        msg.from,
        mentions
    );

    let channel_lead_names = {
        let ps = state.persistent_state.lock().await;
        ps.channel_lead_names()
    };

    // Gather resolution data from persistent state
    let (task_thread_id, task_channel, spans, in_progress_task_ids) = {
        let ps = state.persistent_state.lock().await;
        let tids = ps.task_thread_id.clone();
        let tchan = ps.task_channel.clone();
        let sp = ps.task_session_spans.clone();
        let ip: std::collections::HashSet<String> =
            crate::tasks::get_in_progress_task_ids_for_repo(state.paths.dir_key());
        (tids, tchan, sp, ip)
    };

    let channel = msg.channel_name();
    let nudge_text = render_thread_context(msg);

    for name in mentions {
        // Skip self-mentions
        if name.eq_ignore_ascii_case(&msg.from) {
            debug!("{} mentioned themselves, skipping", name);
            continue;
        }

        // Deduplicate
        let should_nudge = state.cooldowns.lock().unwrap().check_and_record(
            &format!("chat_mention_{}", name),
            &msg.id,
            Duration::from_secs(3600),
        );
        if !should_nudge {
            debug!("Skipping duplicate @mention nudge for {} (msg {})", name, msg.id);
            continue;
        }

        // Channel lead mentions route directly
        if channel_lead_names.contains(&name) {
            let thread_ctx = msg.thread_parent_id.as_ref().map(|pid| {
                super::wake_reason::ThreadContext {
                    parent_id: pid.clone(),
                    channel_name: msg.channel_name().to_string(),
                }
            });
            let effect = super::effects::Effect::NudgeChannelLead {
                channel_name: channel.to_string(),
                reason: super::wake_reason::WakeReason::Mention {
                    from: msg.from.clone(),
                    content: msg.content.clone(),
                    msg_id: msg.id.clone(),
                    thread_ctx,
                },
            };
            super::effects::execute_effects(vec![effect], state).await;
            continue;
        }

        // Resolve via context
        match crate::rules::resolve_mention(
            &name,
            msg.thread_parent_id.as_deref(),
            channel,
            &task_thread_id,
            &task_channel,
            &spans,
            &in_progress_task_ids,
        ) {
            Some(target) => {
                // Set preferred_name on the session record before delivering
                {
                    let mut ps = state.persistent_state.lock().await;
                    if let Some(record) = ps.sessions.get_mut(&target.session_id) {
                        record.preferred_name = Some(name.clone());
                        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                            warn!("Failed to save preferred_name: {}", e);
                        }
                    }
                }
                let effect = super::effects::Effect::TaskPrompt {
                    task_id: target.task_id,
                    message: nudge_text.clone(),
                    model: None,
                    pr_context: None,
                };
                super::effects::execute_effects(vec![effect], state).await;
            }
            None => {
                info!("Could not resolve @{} to a session in this context", name);
                let effect = super::effects::Effect::PostSystemMessage {
                    channel: Some(channel.to_string()),
                    message: format!(
                        "Could not resolve @{} to a session in this context (channel: {}, thread: {:?})",
                        name, channel, msg.thread_parent_id
                    ),
                    thread_parent_id: msg.thread_parent_id.clone(),
                };
                super::effects::execute_effects(vec![effect], state).await;
            }
        }
    }
}
```

- [ ] **Step 2: Add `get_in_progress_task_ids_for_repo()` helper to `src/tasks.rs`**

This function does not exist yet. Add it near `get_in_progress_tasks_with_subjects_for_repo()`:

```rust
/// Returns the set of in-progress task IDs for the given repo.
pub fn get_in_progress_task_ids_for_repo(dir_key: &str) -> std::collections::HashSet<String> {
    get_in_progress_tasks_with_subjects_for_repo(dir_key)
        .into_iter()
        .map(|(id, _, _)| id)
        .collect()
}
```

- [ ] **Step 3: Run `cargo clippy` and `cargo test`**

Run: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`
Expected: Clippy clean, tests pass (some old chat_tests may fail — that's expected and addressed in Task 7)

- [ ] **Step 4: Commit**

```bash
git add src/daemon/chat.rs src/tasks.rs
git commit -m "refactor: simplify route_mentions() to use resolve_mention()"
```

---

### Task 5: Simplify `route_at_all()` to use `resolve_all_mention()`

**Files:**
- Modify: `src/daemon/chat.rs`

- [ ] **Step 1: Rewrite `route_at_all()`**

Replace `route_at_all()` in `src/daemon/chat.rs`:

```rust
async fn route_at_all(state: &DaemonState, msg: &Message) {
    let nudge_text = render_thread_context(msg);

    // Gather resolution data
    let (spans, in_progress_task_ids, channel_lead_sessions) = {
        let ps = state.persistent_state.lock().await;
        let sp = ps.task_session_spans.clone();
        let ip: std::collections::HashSet<String> =
            crate::tasks::get_in_progress_task_ids_for_repo(state.paths.dir_key());
        let cls = ps.channel_lead_sessions.clone();
        (sp, ip, cls)
    };

    // Resolve all task-bearing sessions
    let targets = crate::rules::resolve_all_mention(&in_progress_task_ids, &spans);

    info!(
        "@all broadcast from {} to {} task session(s) + channel leads",
        msg.from,
        targets.len()
    );

    // Emit TaskPrompt for each task target
    for target in &targets {
        let should_nudge = state.cooldowns.lock().unwrap().check_and_record(
            &format!("chat_at_all_{}", target.session_id),
            &msg.id,
            Duration::from_secs(3600),
        );
        if !should_nudge {
            continue;
        }
        let effect = super::effects::Effect::TaskPrompt {
            task_id: target.task_id.clone(),
            message: nudge_text.clone(),
            model: None,
            pr_context: None,
        };
        super::effects::execute_effects(vec![effect], state).await;
    }

    // Nudge all channel leads
    // Note: channel_lead_sessions maps channel_name → session_id.
    // We skip nudging the lead of the sender's own channel to avoid
    // echoing back. The sender is identified by msg.from (a coworker name),
    // not a channel name, so we skip based on whether the sender IS the
    // channel lead (session_id match) rather than name comparison.
    let sender_session_id = state.name_to_session.lock().unwrap()
        .get(&msg.from.to_lowercase()).cloned();
    for (channel_name, lead_session_id) in &channel_lead_sessions {
        // Don't nudge the channel lead if the sender IS that channel lead
        if sender_session_id.as_deref() == Some(lead_session_id.as_str()) {
            continue;
        }
        let should_nudge = state.cooldowns.lock().unwrap().check_and_record(
            &format!("chat_at_all_lead_{}", channel_name),
            &msg.id,
            Duration::from_secs(3600),
        );
        if !should_nudge {
            continue;
        }
        let effect = super::effects::Effect::NudgeChannelLead {
            channel_name: channel_name.clone(),
            reason: super::wake_reason::WakeReason::Nudge {
                message: nudge_text.clone(),
            },
        };
        super::effects::execute_effects(vec![effect], state).await;
    }
}
```

- [ ] **Step 2: Run `cargo clippy` and `cargo test`**

Run: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`
Expected: Clippy clean, tests pass

- [ ] **Step 3: Commit**

```bash
git add src/daemon/chat.rs
git commit -m "refactor: simplify route_at_all() to use resolve_all_mention()"
```

---

### Task 6: Remove `@lead`/`@ops` special cases from `chat_monitor_loop`

**Files:**
- Modify: `src/daemon/chat.rs`

- [ ] **Step 1: Remove `@lead`/`@ops` special-casing in `chat_monitor_loop()`**

In `src/daemon/chat.rs`, lines 60-107 contain special detection for `@lead` and `@ops` in system messages. Replace this block so that system messages go through the same `route_mentions()` path.

The key change: instead of skipping system messages entirely after the `@lead`/`@ops` check, call `route_mentions()` for system messages before the `continue`:

```rust
// Where the current code has:
//   if SKIP_SENDERS.iter().any(...) || state.is_user_sender(...) {
//       // @lead/@ops special cases
//       continue;
//   }
// Replace with:
if SKIP_SENDERS.iter().any(|&s| s.eq_ignore_ascii_case(&msg.from))
    || state.is_user_sender(&msg.from)
{
    // System messages may contain @mentions that still need routing
    // (e.g., stuck PR warnings mentioning a channel lead).
    // User messages are handled in handle_channel_post to avoid double-nudging.
    if !state.is_user_sender(&msg.from) {
        route_mentions(&state, &msg).await;
    }
    continue;
}
```

This removes all `@lead` and `@ops` string matching. Channel lead names (including "ops") are resolved by the unified `route_mentions()` → channel lead check.

- [ ] **Step 2: Remove the `@lead` pattern import if unused**

Check if `state.project_name` formatting for `@lead` detection is still referenced anywhere else in this function. Remove unused variables.

- [ ] **Step 3: Run `cargo clippy` and `cargo test`**

Run: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`
Expected: Clippy clean, tests pass

- [ ] **Step 4: Commit**

```bash
git add src/daemon/chat.rs
git commit -m "refactor: remove @lead/@ops special cases from chat_monitor_loop"
```

---

### Task 7: Remove old mention code and update tests

**Files:**
- Modify: `src/rules.rs`
- Modify: `src/daemon/chat.rs`
- Modify: `src/daemon/chat_tests.rs`

- [ ] **Step 1: Remove `MentionAction` enum and `decide_mention_action()` from `rules.rs`**

Search for `pub(crate) enum MentionAction` and delete the entire enum definition (3 variants: Nudge, Spawn, Skip). Then search for `pub(crate) fn decide_mention_action` and delete the entire function body through its closing brace.

- [ ] **Step 2: Remove `mention_action_to_effects()` and `ReviewerSessionInfo` from `chat.rs`**

Search for `pub(crate) struct ReviewerSessionInfo` in `src/daemon/chat.rs` and delete the struct. Then search for `fn mention_action_to_effects` and delete the entire function body through its closing brace.

Also remove `extract_task_id` from the `use super::helpers::{...}` import if it is no longer referenced in the file. Search for `extract_task_id` to confirm.

- [ ] **Step 3: Update `chat_tests.rs`**

Remove tests that reference the deleted code:
- `mention_nudge_produces_nudge_effect` — tests `mention_action_to_effects`
- `mention_spawn_produces_spawn_with_callbacks` — tests `mention_action_to_effects`
- `mention_skip_produces_no_effects` — tests `mention_action_to_effects`
- `mention_skip_dev_limit_posts_to_ops_channel` — tests `mention_action_to_effects`
- `mention_spawn_for_reviewer_produces_resume_coworker_effect` — tests `mention_action_to_effects`
- `mention_spawn_without_reviewer_info_produces_spawn_with_callbacks` — tests `mention_action_to_effects`

Keep the tests that are still valid:
- `render_thread_context_*` tests — `render_thread_context` is still used
- `mention_dedup_wiring_*` tests — cooldown dedup is still used
- `chat_mention_cooldown_*` tests — cooldown behavior is still used
- `chat_at_all_*` tests — cooldown behavior is still used

Update imports in `chat_tests.rs` to remove `MentionAction` and `ReviewerSessionInfo`.

- [ ] **Step 4: Run all tests**

Run: `cargo test`
Expected: ALL PASS

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: Clean

- [ ] **Step 6: Commit**

```bash
git add src/rules.rs src/daemon/chat.rs src/daemon/chat_tests.rs
git commit -m "refactor: remove MentionAction, decide_mention_action, mention_action_to_effects"
```

---

## Chunk 3: Handle `handle_channel_post` @lead Cleanup

### Task 8: Remove `@lead` handling from `handle_channel_post`

**Files:**
- Modify: `src/daemon/rpc_channel.rs`

- [ ] **Step 1: Remove `@lead`/`@{project_name}` detection block from `handle_channel_post()`**

In `src/daemon/rpc_channel.rs`, search for the comment `"Nudge the Lead when a coworker explicitly mentions @lead"`. Delete the entire block from that comment through the closing brace of the `if should_nudge` block (approximately lines 552-598). This block:
- Checks `content_lower.contains("@lead") || content_lower.contains(&project_mention)`
- Uses cooldown key `"lead_mention"`
- Constructs a `NudgeChannelLead` effect with `WakeReason::Nudge`

This is now handled by the unified `route_mentions()` call that `handle_channel_post` already invokes via `super::chat::route_mentions()`.

- [ ] **Step 2: Remove unused variables**

After deleting the block, check if `content_lower` and `project_mention` are still referenced elsewhere in the function. If not, remove them. Also check for the `truncate_str` import if it becomes unused.

- [ ] **Step 3: Run `cargo clippy` and `cargo test`**

Run: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`
Expected: Clippy clean, tests pass

- [ ] **Step 4: Commit**

```bash
git add src/daemon/rpc_channel.rs
git commit -m "refactor: remove @lead special case from handle_channel_post"
```

---

### Task 9: Final verification

**Files:** None (verification only)

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: ALL PASS

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: Clean

- [ ] **Step 3: Check for dead code warnings**

Look for any unused import warnings or dead code warnings from clippy. Clean up any references to removed functions (`decide_mention_action`, `MentionAction`, `mention_action_to_effects`, `ReviewerSessionInfo`, `extract_task_id`).

- [ ] **Step 4: Run coverage diff**

Run: `./scripts/coverage-diff.sh`
Review: Ensure new `resolve_mention` and `resolve_all_mention` code has good coverage from the tests in `rules_mention_tests.rs`.

- [ ] **Step 5: Final commit if any cleanup was needed**

```bash
git add -A
git commit -m "chore: clean up dead code from mention routing consolidation"
```
