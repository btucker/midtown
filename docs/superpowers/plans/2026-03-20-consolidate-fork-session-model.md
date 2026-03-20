# Consolidate Fork Session Model

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate fork-specific infrastructure. A fork is just a session with `bound_thread_id` set — it should resume, crash-recover, and get nudged through the same paths as any other session.

**Architecture:** Sessions are either **task-bound** (has `task_id`, no `bound_thread_id`) or **thread-bound** (has `bound_thread_id`, no `task_id`). Both types go through the same dispatch/recovery loop in `dispatch_via_sessions`. Replace 4 in-memory maps, `RespawnFork` effect, and `respawn_fork()` with `SessionRecord` queries. The dispatch loop is extended to recover stopped thread-bound sessions the same way it recovers stopped task-bound sessions.

**Tech Stack:** Rust, serde, tokio

---

## Core Principle

**Two session types, one recovery path:**

| Type | Identifier | Recovery trigger | Resume mode |
|------|-----------|-----------------|-------------|
| Task-bound | `task_id: Some(...)` | Task in_progress + session stopped | `ResumeSession(session_id)` |
| Thread-bound | `bound_thread_id: Some(...)` | Thread exists + session stopped | `ResumeSession(session_id)` |

**Before:** Forks have a parallel state system — 4 in-memory maps maintained alongside SessionRecord, a dedicated `RespawnFork` effect, and fork-specific crash recovery code in the session drain handler.

**After:** SessionRecord is the single source of truth. `dispatch_via_sessions` handles both session types. Fork crash recovery is just "session stopped → dispatch recovers it next tick" — same as workers.

---

## File Structure

### Files to modify

| File | Changes |
|------|---------|
| `src/daemon/mod.rs` | Remove `fork_bound_threads`, `fork_bound_channels`, `topic_sessions`, `fork_respawn_counts` fields. Remove startup rebuild of these maps. Replace fork crash detection with session-based recovery. |
| `src/daemon/effects.rs` | Remove `RespawnFork` effect variant and `respawn_fork()` function (~170 lines). |
| `src/daemon/rpc_channel.rs` | Replace `topic_sessions` lookups with `SessionRecord.bound_thread_id` queries. Remove `try_lazy_fork_respawn()`. Use session resume for dead forks. |
| `src/daemon/rpc_session.rs` | Simplify `create_fork_session()` — stop writing to in-memory maps, only write SessionRecord. |
| `src/daemon/state.rs` | Add `session_by_thread(thread_id)` helper on DaemonPersistentState. |
| `src/daemon/pr.rs` | Remove `effect_variant_name` entry for `RespawnFork`. |
| `tests/multi_tick_harness.rs` | Remove `RespawnFork` handling. |

---

## Chunk 1: Add SessionRecord query helpers

### Task 1: Add `session_by_thread` helper

**Files:** `src/daemon/state.rs`, `src/daemon/state_tests.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn test_session_by_thread() {
    let mut ps = DaemonPersistentState::default();
    ps.sessions.insert("sess-1".into(), SessionRecord {
        session_id: "sess-1".into(),
        name: "ghost-town".into(),
        bound_thread_id: Some("thread-abc".into()),
        is_running: true,
        ..Default::default()
    });
    assert_eq!(ps.session_by_thread("thread-abc").unwrap().session_id, "sess-1");
    assert!(ps.session_by_thread("thread-xyz").is_none());
}

#[test]
fn test_session_by_thread_prefers_running() {
    let mut ps = DaemonPersistentState::default();
    // Stopped session
    ps.sessions.insert("old".into(), SessionRecord {
        session_id: "old".into(),
        name: "fork-old".into(),
        bound_thread_id: Some("thread-1".into()),
        is_running: false,
        ..Default::default()
    });
    // Running session
    ps.sessions.insert("new".into(), SessionRecord {
        session_id: "new".into(),
        name: "fork-new".into(),
        bound_thread_id: Some("thread-1".into()),
        is_running: true,
        ..Default::default()
    });
    // Should return the running one
    assert_eq!(ps.session_by_thread("thread-1").unwrap().session_id, "new");
}
```

- [ ] **Step 2: Implement helper**

```rust
/// Find the session bound to a thread (for fork routing).
/// Prefers running sessions over stopped ones.
pub fn session_by_thread(&self, thread_id: &str) -> Option<&SessionRecord> {
    let mut best: Option<&SessionRecord> = None;
    for s in self.sessions.values() {
        if s.bound_thread_id.as_deref() == Some(thread_id) {
            if s.is_running {
                return Some(s); // Running session is always preferred
            }
            if best.is_none() {
                best = Some(s); // Fall back to stopped session
            }
        }
    }
    best
}
```

- [ ] **Step 3: Run tests, commit**

---

## Chunk 2: Replace `topic_sessions` with `session_by_thread`

### Task 2: Replace topic_sessions reads in rpc_channel.rs

**Files:** `src/daemon/rpc_channel.rs`

The two places that read `topic_sessions` (lines 368 and 489) need to query `SessionRecord` instead.

- [ ] **Step 1: Replace fork lookup in handle_channel_post (line 366-373)**

Replace:
```rust
let mut fork_session_id = state.topic_sessions.lock().unwrap()
    .get(parent_id).filter(|s| s.as_str() != "pending").cloned();
```

With:
```rust
let mut fork_session_id = {
    let ps = state.persistent_state.lock().await;
    ps.session_by_thread(parent_id).map(|s| s.session_id.clone())
};
```

- [ ] **Step 2: Replace the second lookup (line ~489) — same pattern**

- [ ] **Step 3: Replace `topic_sessions` write in try_lazy_fork_respawn aftermath**

After a fork respawn, instead of updating `topic_sessions`, the respawn path should update the SessionRecord's `bound_thread_id` (which it already does).

- [ ] **Step 4: Run tests, commit**

### Task 3: Remove topic_sessions field from DaemonState

**Files:** `src/daemon/mod.rs`

- [ ] **Step 1: Remove the field declaration**
- [ ] **Step 2: Remove initialization in `DaemonState::new()`**
- [ ] **Step 3: Remove startup rebuild of `topic_sessions` from SessionRecord**
- [ ] **Step 4: Remove `topic_sessions` cleanup from `cleanup_coworker_state_internal()`**
- [ ] **Step 5: Fix compilation errors (remove all `.topic_sessions.lock()` calls)**
- [ ] **Step 6: Run tests, commit**

---

## Chunk 3: Replace fork crash recovery with session resume

### Task 4: Extend dispatch_via_sessions to recover thread-bound sessions

**Files:** `src/daemon/dispatch.rs`, `src/daemon/events.rs`

Currently `dispatch_via_sessions` iterates `tick_in_progress_tasks` and recovers stopped task-bound sessions. Extend it to also iterate stopped thread-bound sessions (forks) and recover them the same way.

- [ ] **Step 1: Add thread-bound session recovery to dispatch_via_sessions_inner**

After the existing task-bound recovery loop, add:

```rust
// Recover stopped thread-bound sessions (forks).
// These have bound_thread_id but no task — they're research/investigation sessions
// bound to channel threads.
for record in ps.sessions.values() {
    if record.bound_thread_id.is_some()
        && !record.is_running
        && record.resume_on_startup  // Only resume sessions that want to be resumed
    {
        let thread_id = record.bound_thread_id.as_deref().unwrap();

        // Cooldown: don't retry too fast
        if ps.tick_spawn_failure_cooldown_names.contains(&record.name.to_lowercase()) {
            continue;
        }

        // Already recovered recently?
        if ps.tick_recently_recovered_session_ids.contains(&record.session_id) {
            continue;
        }

        info!(
            "Session dispatch: recovering thread-bound session {} (thread {})",
            record.name, thread_id
        );

        let decision = SpawnDecision {
            task_id: String::new(),
            session_mode: crate::launch::SessionMode::ResumeSession(record.session_id.clone()),
            preferred_name: Some(record.name.clone()),
            cooldown_category: "session_dispatch".to_string(),
        };
        effects.extend(build_spawn_effects(&decision, ps, tasks));
    }
}
```

- [ ] **Step 2: Remove fork crash detection from session drain handler in mod.rs**

The session drain handler (mod.rs ~lines 3948-4142) currently detects dead forks via `fork_bound_threads` and emits `RespawnFork`. Remove this fork-specific block — the dispatch tick will handle recovery on the next cycle.

Keep `cleanup_dead_coworker_state()` which marks `is_running = false` — this is what the dispatch loop checks.

- [ ] **Step 3: Set `resume_on_startup = false` for forks**

Fork sessions should NOT auto-resume on daemon restart (they're ephemeral research sessions). Verify `create_fork_session` sets `resume_on_startup: false` — it already does (line ~1494). This means the dispatch loop only recovers forks that crash during a running daemon, not across restarts. That's correct behavior.

- [ ] **Step 4: Add cooldown protection**

Use `CooldownTracker` with `"fork_respawn"` category keyed by `session_id`. This replaces `fork_respawn_counts`.

- [ ] **Step 5: Run tests, commit**

### Task 5: Remove RespawnFork effect and respawn_fork function

**Files:** `src/daemon/effects.rs`, `src/daemon/pr.rs`, `tests/multi_tick_harness.rs`

- [ ] **Step 1: Remove `Effect::RespawnFork` variant from the enum**
- [ ] **Step 2: Remove `respawn_fork()` function (~170 lines)**
- [ ] **Step 3: Remove `RespawnFork` from `effect_variant_name` in pr.rs**
- [ ] **Step 4: Remove `RespawnFork` handling from multi_tick_harness.rs**
- [ ] **Step 5: Run tests, commit**

---

## Chunk 4: Replace fork_bound_threads with SessionRecord

### Task 6: Replace fork_bound_threads reads

**Files:** `src/daemon/rpc_channel.rs`, `src/daemon/effects.rs`, `src/daemon/mod.rs`

The main performance-sensitive read is in `handle_channel_post` (line 255) for auto-threading output. This uses a sync Mutex to avoid the async persistent_state lock.

Two options:
- **Option A**: Keep a derived cache (like `tick_*` fields) that's rebuilt from SessionRecord. Simpler but adds another cache.
- **Option B**: Use `persistent_state.lock().await` for the thread lookup. Marginally slower but eliminates the cache entirely.

Prefer **Option B** — the channel post path already does multiple async locks. One more won't matter for <20 sessions.

- [ ] **Step 1: Replace `fork_bound_threads` read in handle_channel_post (line 255)**

Replace:
```rust
let bound_thread: Option<String> = if thread_parent_id.is_none() && !is_dm_channel {
    state.fork_bound_threads.lock().unwrap().get(from).cloned()
} else { None };
```

With:
```rust
let bound_thread: Option<String> = if thread_parent_id.is_none() && !is_dm_channel {
    let ps = state.persistent_state.lock().await;
    ps.session_by_name(from).and_then(|s| s.bound_thread_id.clone())
} else { None };
```

- [ ] **Step 2: Replace `fork_bound_threads` read in PostToChannel effect handler**

Same pattern — use SessionRecord lookup instead of cache.

- [ ] **Step 3: Replace `fork_bound_threads` read in dm_mirror_agent_names**

- [ ] **Step 4: Remove fork_bound_threads field from DaemonState**
- [ ] **Step 5: Remove startup rebuild**
- [ ] **Step 6: Remove cleanup in cleanup_coworker_state_internal**
- [ ] **Step 7: Run tests, commit**

### Task 7: Replace fork_bound_channels with SessionRecord

**Files:** `src/daemon/mod.rs`, `src/daemon/effects.rs`

Same approach — use SessionRecord.channel + is_fork_session() instead of a dedicated cache.

- [ ] **Step 1: Replace all fork_bound_channels reads with SessionRecord queries**
- [ ] **Step 2: Remove the field, initialization, and cleanup**
- [ ] **Step 3: Run tests, commit**

---

## Chunk 5: Remove try_lazy_fork_respawn and fork_respawn_counts

### Task 8: Replace try_lazy_fork_respawn with session resume

**Files:** `src/daemon/rpc_channel.rs`

`try_lazy_fork_respawn` is called when a thread reply arrives for a dead fork. Replace with: check if the session is alive, and if not, emit a session resume effect (same as worker recovery).

- [ ] **Step 1: Inline the fork-alive check**

Replace:
```rust
fork_session_id = try_lazy_fork_respawn(state, fork_sid, parent_id).await;
```

With:
```rust
if !state.session_manager.is_alive(&fork_name).await {
    // Fork is dead — trigger resume via session dispatch
    // The next tick's session recovery will pick it up
    // For immediate delivery, nudge after recovery completes
}
```

- [ ] **Step 2: Delete `try_lazy_fork_respawn()` function**
- [ ] **Step 3: Remove `fork_respawn_counts` field from DaemonState**
- [ ] **Step 4: Run tests, commit**

---

## Chunk 6: Simplify create_fork_session

### Task 9: Remove in-memory map writes from create_fork_session

**Files:** `src/daemon/rpc_session.rs`

`create_fork_session` currently writes to `topic_sessions`, `fork_bound_threads`, and `fork_bound_channels`. After consolidation, it only needs to write SessionRecord (which it already does).

- [ ] **Step 1: Remove `topic_sessions` insert (line ~1495)**
- [ ] **Step 2: Remove `fork_bound_threads` insert (line ~1482-1486)**
- [ ] **Step 3: Remove `fork_bound_channels` insert (line ~1487-1492)**
- [ ] **Step 4: Verify SessionRecord is written with correct `bound_thread_id` and `channel`**
- [ ] **Step 5: Run tests, commit**

---

## Chunk 7: Cleanup and docs

### Task 10: Update AGENTS.md with session architecture principles

**Files:** `AGENTS.md`

Add a "Session Architecture" section to AGENTS.md that establishes core principles. This prevents future contributors from re-introducing fork-specific infrastructure.

- [ ] **Step 1: Add session architecture section to AGENTS.md**

Add after the "Session Taxonomy" section:

```markdown
## Session Architecture

### Core Principle: SessionRecord is the single source of truth

All session state lives in `DaemonPersistentState.sessions: HashMap<String, SessionRecord>`. There are no separate in-memory maps for session routing, thread binding, or crash recovery. Every session lookup goes through SessionRecord.

### Two session types, one recovery path

Sessions are either **task-bound** or **thread-bound**:

| Type | Has `task_id` | Has `bound_thread_id` | Example |
|------|:---:|:---:|---------|
| Task-bound | ✅ | ❌ | Workers, reviewers |
| Thread-bound | ❌ | ✅ | Forks (research sessions) |

Both types recover through the same `dispatch_via_sessions` loop:
- Session crashes → `is_running` set to false
- Next dispatch tick detects stopped session → emits resume effect
- No fork-specific crash recovery, effects, or state maps

### No parallel state

Do NOT introduce in-memory maps that shadow SessionRecord fields. If you need fast lookups (e.g., thread_id → session), derive them from SessionRecord at tick preparation time via `prepare_tick()`, not as separately-maintained caches.

### Thread routing

Messages posted to a thread are routed to the session whose `bound_thread_id` matches the thread. Use `ps.session_by_thread(thread_id)` — do not maintain a separate thread→session map.
```

- [ ] **Step 2: Commit**

### Task 11: Final cleanup

- [ ] **Step 1: Search for any remaining references to removed fields/functions**
- [ ] **Step 2: Update `docs/architecture.md`** — document the unified session model
- [ ] **Step 3: Remove stale comments mentioning fork-specific maps**
- [ ] **Step 4: Run full test suite**
- [ ] **Step 5: Run clippy**
- [ ] **Step 6: Commit**
