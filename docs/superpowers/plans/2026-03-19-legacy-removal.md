# Legacy Code Removal Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove redundant state tracking (TaskSessionSpan), legacy task storage (tasks.rs), and dead indirection layers (SessionKey, CoworkerManager) that the unified agent sessions refactor made obsolete.

**Architecture:** Four independent removal passes, each resulting in a compilable, test-passing codebase. TaskSessionSpan removal is the most impactful (eliminates a redundant state layer). tasks.rs migration is the largest (104 call sites across 3,175 lines). SessionKey and CoworkerManager removal are mechanical.

**Tech Stack:** Rust, serde JSON

**Spec:** `docs/superpowers/specs/2026-03-18-unified-agent-sessions-design.md`

---

## File Impact Summary

| Task | Files to modify | Files to delete | Estimated net lines removed |
|------|----------------|----------------|-----------------------------|
| 1: TaskSessionSpan removal | ~15 src + ~10 test | `src/daemon/span_tests.rs` | ~400 |
| 2: tasks.rs migration | ~20 src + ~5 test | `src/tasks.rs`, `src/tasks_tests.rs` | ~3,000 |
| 3: SessionKey removal | ~10 src + ~3 test | `src/session_key.rs` | ~300 |
| 4: CoworkerManager → SessionRecord | ~15 src + ~5 test | potentially `src/coworker.rs` | ~500 |
| 5: coworker_state.rs cleanup | ~5 src | possibly rename | ~0 (rename only) |

---

## Task 1: Remove TaskSessionSpan

`TaskSessionSpan` tracks temporal assignment of sessions to tasks. With unique 1:1 task-session naming, `SessionRecord` already has all this information. `pr_has_active_reviewer()` and `clear_reviewer_assignment()` already check sessions directly (added in the last commit).

**Files:**
- Modify: `src/daemon/state.rs` — remove struct, `task_session_spans` field, 6 span methods
- Modify: `src/daemon/effects.rs` — remove `Effect::CreateTaskSessionSpan`, `Effect::CloseTaskSessionSpan`, their handlers
- Modify: `src/daemon/health.rs` — update reviewer respawn to not emit `CreateTaskSessionSpan`
- Modify: `src/daemon/pr.rs` — replace `active_reviewer_for_pr()` span result with session result, remove `close_spans_for_task()` calls
- Modify: `src/daemon/rpc_prs.rs` — update reviewer name extraction from span to session
- Modify: `src/daemon/rpc_session.rs` — update attach target resolution
- Modify: `src/daemon/rpc_status.rs` — replace `active_reviewer_spans()` with `running_reviewer_sessions()`
- Modify: `src/daemon/rpc_coworker.rs` — update reviewer data building
- Modify: `src/daemon/snapshot.rs` — remove span-based reviewer computation
- Modify: `src/daemon/chat.rs` — update mention routing
- Modify: `src/daemon/mod.rs` — remove `close_spans_for_session()` calls
- Modify: `src/daemon/rpc_channel.rs` — remove span cleanup on channel archive
- Modify: `src/web.rs` — update reviewer status lookup
- Modify: `tests/multi_tick_harness.rs` — remove span effect handling
- Delete: `src/daemon/span_tests.rs`
- Modify: ~10 test files that call `create_span()`

- [ ] **Step 1: Remove `TaskSessionSpan` struct and `task_session_spans` field**

In `src/daemon/state.rs`:
- Delete the `TaskSessionSpan` struct (line 58)
- Replace `task_session_spans: Vec<TaskSessionSpan>` with `_task_session_spans: serde_json::Value` (with `#[serde(default, rename = "task_session_spans")]` for deserialization compat, `#[serde(skip_serializing)]` to stop persisting)
- Delete: `create_span()`, `close_span()`, `close_spans_for_session()`, `close_spans_for_task()`, `active_span_for_task()`, `spans_for_task()`
- Delete span GC code from `apply_gc()`
- Update `active_reviewer_for_pr()` to return `Option<&SessionRecord>` (query sessions, not spans)
- Update `active_reviewer_spans()` → rename to `active_reviewer_sessions()` returning `Vec<&SessionRecord>` (query sessions)
- Update `clear_reviewer_assignment()` to only use session-based path (remove span path)

- [ ] **Step 2: Run `cargo check`, fix compilation errors iteratively**

All callers of removed methods need updating. Key patterns:
- `span.agent_name` → `session.name`
- `span.task_id` (String) → `session.task_id` (Option<String>) — use `.as_deref().unwrap_or("")`
- `span.session_id` → `session.session_id`
- `create_span(...)` calls → delete (session creation already handles this)
- `close_span(...)` / `close_spans_for_*()` calls → delete (session shutdown sets `is_running = false`)
- `Effect::CreateTaskSessionSpan { ... }` → delete the effect variant and handler
- `Effect::CloseTaskSessionSpan { ... }` → delete the effect variant and handler

For `SpawnCoworkerWithCallbacks.on_success` in `health.rs`: the callback currently emits `CreateTaskSessionSpan`. Replace with storing `task_pr_number` and `task_restart_count` directly (the session record is already created by `spawn_coworker()`).

- [ ] **Step 3: Delete `src/daemon/span_tests.rs`**, remove its `mod` declaration

- [ ] **Step 4: Update all test files that call `create_span()`**

Replace with inserting `SessionRecord` entries directly. Files: `state_tests.rs`, `pr_tests.rs`, `effects_tests.rs`, `rpc_prs_tests.rs`, `rpc_pr_review_tests.rs`, `rpc_coworker_tests.rs`, `snapshot_tests.rs`, `reviewer_break_clears_assignment.rs`

- [ ] **Step 5: `cargo test --lib`, `cargo clippy`, `cargo fmt`**

- [ ] **Step 6: Commit**
```
refactor: Remove TaskSessionSpan, replace with SessionRecord queries
```

---

## Task 2: Migrate callers from tasks.rs to TaskStore

`src/tasks.rs` (3,175 lines) reads Claude Code's native task format from `~/.claude/tasks/`. With TaskStore now the primary storage, migrate all 104 call sites to use TaskStore instead.

**Files:**
- Modify: `src/daemon/dispatch.rs` — task ownership, blocked_by resolution
- Modify: `src/daemon/chat.rs` — in-progress task queries
- Modify: `src/daemon/pr.rs` — task ID extraction, abandoned PR detection
- Modify: `src/daemon/rpc_coworker.rs` — coworker task lists
- Modify: `src/daemon/rpc_task.rs` — task CRUD (remaining old-format calls)
- Modify: `src/daemon/snapshot.rs` — task data for WorldSnapshot
- Modify: `src/daemon/effects.rs` — task status checks
- Modify: `src/rules.rs` — task status checks
- Modify: `src/web.rs` — task display in web API
- Modify: `src/daemon/rpc_status.rs` — status display
- Delete: `src/tasks.rs`, `src/tasks_tests.rs` (if exists)

**Approach:** This is too large for one pass. Break into sub-steps:
1. Add equivalent query methods to `TaskStore` (e.g., `get_in_progress_tasks()`, `find_by_pr_title()`)
2. Migrate callers file by file, keeping old module alive
3. Once all callers migrated, delete `src/tasks.rs`

- [ ] **Step 1: Add query methods to TaskStore**

Add to `src/task_store.rs`:
- `get_in_progress_tasks(&self) -> Vec<Task>` — filter by `status == InProgress`
- `get_in_progress_with_subjects(&self) -> Vec<(String, String)>` — `(id, subject)` pairs
- `find_task_by_owner(&self, owner: &str) -> Option<Task>` — for backward compat during transition
- `extract_task_id_from_pr_title(title: &str) -> Option<String>` — static helper, move from tasks.rs

- [ ] **Step 2: Migrate callers file by file**

Work through each file, replacing `crate::tasks::*` calls with `state.task_store.*` calls. Use `cargo check` after each file.

- [ ] **Step 3: Remove old `src/tasks.rs` module once all callers migrated**

- [ ] **Step 4: `cargo test`, `cargo clippy`, `cargo fmt`**

- [ ] **Step 5: Commit**
```
refactor: Migrate all task queries to TaskStore, remove legacy tasks.rs
```

---

## Task 3: Remove SessionKey

`SessionKey` is a compound key (display name + session UUID) that was needed when avenue names were reused. With unique 1:1 names, the session ID alone is the identity and `name` on `SessionRecord` is the display name.

**Files:**
- Delete: `src/session_key.rs`
- Modify: `src/lib.rs` — remove `pub mod session_key` and `pub use session_key::SessionKey`
- Modify: all 43 references to `SessionKey` across the codebase

**Approach:** Replace `SessionKey` usages with either `String` (session_id) or `SessionRecord.name` depending on context. Most usages are in `CoworkerManager` which is also being removed (Task 4), so doing Task 3 and 4 together may be more efficient.

- [ ] **Step 1: Audit all 43 SessionKey references**

Categorize each as:
- Uses `.name()` → replace with `SessionRecord.name` or a plain `String`
- Uses `.session_id()` → replace with `String` session_id
- Uses as HashMap key → replace with `String` session_id

- [ ] **Step 2: Replace references, delete module**

- [ ] **Step 3: `cargo test`, `cargo clippy`, `cargo fmt`**

- [ ] **Step 4: Commit**
```
refactor: Remove SessionKey, use plain strings for session identity
```

---

## Task 4: Remove CoworkerManager

`CoworkerManager` (949 lines) is an in-memory cache of active coworkers with name allocation, slot management, and worktree binding. With `SessionRecord` as the authoritative state and names coming from tasks, `CoworkerManager` is redundant.

**Files:**
- Delete: `src/coworker.rs` (if fully removable, or gut it)
- Modify: `src/daemon/mod.rs` — `DaemonState.coworkers` field, `spawn_coworker()` prep
- Modify: `src/daemon/effects.rs` — coworker insert/remove calls
- Modify: `src/daemon/dispatch.rs` — active coworker queries
- Modify: `src/daemon/health.rs` — coworker health checks
- Modify: `src/daemon/snapshot.rs` — coworker snapshot data
- Modify: `src/lib.rs` — remove re-exports

**Approach:** This is the hardest removal because `CoworkerManager` is deeply wired into the daemon loop. Strategy:
1. Identify what `CoworkerManager` provides that `DaemonPersistentState.sessions` doesn't
2. Add any missing capabilities to `DaemonPersistentState` (e.g., `prepare_spawn()` worktree logic)
3. Migrate callers
4. Remove

Key `CoworkerManager` responsibilities to migrate:
- `prepare_spawn()` → worktree creation (move to `WorktreeRegistry`)
- `insert()` / `remove()` → session insert already happens in `spawn_coworker()`
- `get_busy_coworker_names()` → query sessions
- `active_names()` → query sessions (already in snapshot as `active_session_names`)
- `is_alive()` → `sessions.get(id).map(|s| s.is_running)`

- [ ] **Step 1: Add missing capabilities to DaemonPersistentState/DaemonState**

- [ ] **Step 2: Migrate callers from CoworkerManager to sessions**

- [ ] **Step 3: Remove CoworkerManager**

- [ ] **Step 4: `cargo test`, `cargo clippy`, `cargo fmt`**

- [ ] **Step 5: Commit**
```
refactor: Remove CoworkerManager, use SessionRecord as sole session state
```

---

## Task 5: Rename coworker_state.rs

`WorkflowPhase` is still actively used (90 references) and isn't going away. But `coworker_state.rs` is a misleading name now. Rename to `workflow.rs` or `workflow_phase.rs` as part of the `coworker` → `agent_session` terminology cleanup.

- [ ] **Step 1: Rename file and update mod declaration**

- [ ] **Step 2: `cargo test`, `cargo clippy`, `cargo fmt`**

- [ ] **Step 3: Commit**
```
refactor: Rename coworker_state.rs to workflow_phase.rs
```

---

## Task Dependencies

```
Task 1 (TaskSessionSpan) ─┐
                           ├─→ Task 2 (tasks.rs) ─→ Task 3+4 (SessionKey + CoworkerManager) ─→ Task 5 (rename)
                           │
                           └─→ can start immediately
```

Task 1 is independent and should be done first (biggest win, no dependencies).
Task 2 can be done in parallel with Task 1.
Tasks 3 and 4 should be done together (SessionKey is mostly used inside CoworkerManager).
Task 5 is a trivial rename, do last.
