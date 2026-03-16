# Remove pr_reviewers — Migrate to TaskSessionSpan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace dual `pr_reviewers`/`task_reviewer_metadata` structures with a unified `TaskSessionSpan` model as the single source of truth for reviewer session tracking.

**Architecture:** Add `TaskSessionSpan` struct and `task_session_spans: Vec<TaskSessionSpan>` to `DaemonPersistentState`. Build query helpers that replace all `GitHubState` reviewer methods. Migrate consumer sites file-by-file, keeping the `SnapshotReviewerState` shape mostly unchanged so `rules.rs` decision functions need minimal changes. Finally delete old structures and tests.

**Tech Stack:** Rust, serde, chrono, tokio

**Spec:** `docs/superpowers/specs/2026-03-16-remove-pr-reviewers-design.md`

---

## Chunk 1: Foundation — TaskSessionSpan struct and query helpers

### Task 1: Add TaskSessionSpan struct and storage

**Files:**
- Modify: `src/daemon/state.rs` (add struct, add field to DaemonPersistentState)
- Create: `src/daemon/span_tests.rs` (unit tests)

- [ ] **Step 1: Write failing tests for TaskSessionSpan query helpers**

Create `src/daemon/span_tests.rs` with tests for the core query API:

```rust
// Tests to write:
// - test_active_span_for_task: create span with end_time=None, verify found
// - test_active_span_for_task_closed: create span with end_time=Some, verify not found
// - test_spans_for_task_ordered: create multiple spans, verify ordered by start_time
// - test_active_reviewer_for_pr: create reviewer span, link SessionRecord with pr_number, verify found
// - test_pr_has_active_reviewer_running: span + running session → true
// - test_pr_has_active_reviewer_stopped: span + stopped session → false
// - test_active_reviewers: multiple spans, only reviewer type returned
// - test_close_span: close specific span by session_id + task_id
// - test_close_spans_for_session: close all spans for a session
// - test_close_spans_for_task: close all spans for a task
```

Wire up in `state.rs` with `#[path = "span_tests.rs"] #[cfg(test)] mod span_tests;`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test span_tests -v`
Expected: FAIL — struct and methods don't exist yet

- [ ] **Step 3: Add TaskSessionSpan struct to state.rs**

Add to `src/daemon/state.rs` after the `TaskReviewerMetadata` struct:

```rust
/// A temporal record of a session working on a task.
///
/// Tracks the time span during which a specific session was assigned to a task.
/// Replaces the dual pr_reviewers/task_reviewer_metadata model with a single
/// source of truth. Open spans (end_time = None) represent active assignments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSessionSpan {
    /// Task ID this span belongs to.
    pub task_id: String,
    /// Coworker name at the time of this span.
    pub agent_name: String,
    /// Role: "dev", "reviewer", or "channel-lead".
    pub agent_type: String,
    /// Claude Code session ID.
    pub session_id: String,
    /// When the session started working on this task.
    pub start_time: DateTime<Utc>,
    /// When the session stopped (None = still active).
    pub end_time: Option<DateTime<Utc>>,
}
```

Add field to `DaemonPersistentState`:

```rust
/// Temporal session history for tasks.
/// Tracks which sessions worked on which tasks and when.
#[serde(default)]
pub task_session_spans: Vec<TaskSessionSpan>,
```

Add `task_pr_number` map (for PR number resolution before SessionRecord is populated):

```rust
/// Task-to-PR-number mapping for reviewer tasks.
/// Set at review task creation so PR lookups work before the reviewer session
/// populates SessionRecord.pr_number.
#[serde(default)]
pub task_pr_number: HashMap<String, u64>,
```

- [ ] **Step 4: Add query helper methods**

Add to `impl DaemonPersistentState` in `state.rs`:

```rust
/// Find the currently active span for a task (end_time is None).
pub fn active_span_for_task(&self, task_id: &str) -> Option<&TaskSessionSpan> {
    self.task_session_spans
        .iter()
        .rfind(|s| s.task_id == task_id && s.end_time.is_none())
}

/// Find all spans for a task, ordered by start_time.
pub fn spans_for_task(&self, task_id: &str) -> Vec<&TaskSessionSpan> {
    let mut spans: Vec<_> = self.task_session_spans
        .iter()
        .filter(|s| s.task_id == task_id)
        .collect();
    spans.sort_by_key(|s| s.start_time);
    spans
}

/// Find the active reviewer span for a PR number.
///
/// Resolution path:
/// 1. Filter active reviewer spans (end_time = None, agent_type = "reviewer")
/// 2. Check task_pr_number map for the span's task
/// 3. Fallback: check SessionRecord.pr_number
pub fn active_reviewer_for_pr(&self, pr_number: u64) -> Option<&TaskSessionSpan> {
    self.task_session_spans
        .iter()
        .filter(|s| s.end_time.is_none() && s.agent_type == "reviewer")
        .find(|s| {
            // Primary: task_pr_number map
            self.task_pr_number.get(&s.task_id) == Some(&pr_number)
                // Fallback: SessionRecord.pr_number
                || self.sessions.get(&s.session_id)
                    .and_then(|r| r.pr_number)
                    == Some(pr_number)
        })
}

/// Check if a PR has an active reviewer with a running session.
pub fn pr_has_active_reviewer(&self, pr_number: u64) -> bool {
    self.active_reviewer_for_pr(pr_number)
        .map(|span| {
            self.sessions.get(&span.session_id)
                .map(|s| s.is_running)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// Get all currently active reviewer spans.
pub fn active_reviewer_spans(&self) -> Vec<&TaskSessionSpan> {
    self.task_session_spans
        .iter()
        .filter(|s| s.end_time.is_none() && s.agent_type == "reviewer")
        .collect()
}

/// Close a span (set end_time) when a session stops working on a task.
pub fn close_span(&mut self, session_id: &str, task_id: &str) {
    for span in &mut self.task_session_spans {
        if span.session_id == session_id && span.task_id == task_id && span.end_time.is_none() {
            span.end_time = Some(Utc::now());
        }
    }
}

/// Close all open spans for a session (used on session shutdown).
pub fn close_spans_for_session(&mut self, session_id: &str) {
    for span in &mut self.task_session_spans {
        if span.session_id == session_id && span.end_time.is_none() {
            span.end_time = Some(Utc::now());
        }
    }
}

/// Close all open spans for a task (used on task completion/cancellation).
pub fn close_spans_for_task(&mut self, task_id: &str) {
    for span in &mut self.task_session_spans {
        if span.task_id == task_id && span.end_time.is_none() {
            span.end_time = Some(Utc::now());
        }
    }
}

/// Create a new task session span.
pub fn create_span(&mut self, task_id: &str, agent_name: &str, agent_type: &str, session_id: &str) {
    self.task_session_spans.push(TaskSessionSpan {
        task_id: task_id.to_string(),
        agent_name: agent_name.to_string(),
        agent_type: agent_type.to_string(),
        session_id: session_id.to_string(),
        start_time: Utc::now(),
        end_time: None,
    });
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test span_tests -v`
Expected: PASS

- [ ] **Step 6: Add GC for spans**

Add to `apply_gc()` in `state.rs`:

```rust
// Step 1: Force-close open spans for sessions that no longer exist
// (preserves historical record before GC pass removes old closed spans)
let now = Utc::now();
for span in &mut self.task_session_spans {
    if span.end_time.is_none() && !self.sessions.contains_key(&span.session_id) {
        span.end_time = Some(now);
    }
}

// Step 2: GC closed spans older than 48 hours
let gc_cutoff = now - chrono::Duration::hours(48);
self.task_session_spans.retain(|s| {
    match s.end_time {
        Some(end) => end > gc_cutoff,
        None => true, // Open spans for existing sessions are kept
    }
});

// Step 3: Cap at 500 spans (prune oldest closed spans first)
if self.task_session_spans.len() > 500 {
    self.task_session_spans.sort_by_key(|s| {
        (s.end_time.is_none(), s.start_time)
    });
    self.task_session_spans.truncate(500);
}
```

- [ ] **Step 7: Run full test suite**

Run: `cargo test`
Expected: PASS — new code is additive, nothing breaks

- [ ] **Step 8: Commit**

```bash
git add src/daemon/state.rs src/daemon/span_tests.rs
git commit -m "feat: Add TaskSessionSpan struct and query helpers (!2320)"
```

---

### Task 2: Add span-based Effect variants and handlers

**Files:**
- Modify: `src/daemon/effects.rs` (add new variants and handlers)
- Create: `src/daemon/span_effect_tests.rs` (tests for new handlers)

- [ ] **Step 1: Write failing tests for new effect handlers**

Tests to cover:
- `test_create_task_session_span_effect`: dispatching effect creates span
- `test_close_task_session_span_effect`: dispatching effect closes span
- `test_create_span_sets_task_pr_number`: reviewer span populates task_pr_number

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test span_effect_tests -v`
Expected: FAIL

- [ ] **Step 3: Add new Effect variants**

Add to the `Effect` enum in `effects.rs`:

```rust
/// Create a new task session span (reviewer or dev session starting work).
CreateTaskSessionSpan {
    task_id: String,
    agent_name: String,
    agent_type: String,
    session_id: String,
    /// For reviewer tasks: the PR number being reviewed.
    pr_number: Option<u64>,
},
/// Close a task session span (session stopping work on a task).
CloseTaskSessionSpan {
    session_id: String,
    task_id: String,
},
```

- [ ] **Step 4: Add effect handlers**

In the `execute_effect()` match block:

```rust
Effect::CreateTaskSessionSpan { task_id, agent_name, agent_type, session_id, pr_number } => {
    let mut ps = state.persistent_state.lock().await;
    ps.create_span(&task_id, &agent_name, &agent_type, &session_id);
    if let Some(pr) = pr_number {
        ps.task_pr_number.insert(task_id.clone(), pr);
    }
    ps.save_for_repo(&state.paths.dir_key())?;
}
Effect::CloseTaskSessionSpan { session_id, task_id } => {
    let mut ps = state.persistent_state.lock().await;
    ps.close_span(&session_id, &task_id);
    ps.save_for_repo(&state.paths.dir_key())?;
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test span_effect_tests -v`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/daemon/effects.rs src/daemon/span_effect_tests.rs
git commit -m "feat: Add CreateTaskSessionSpan/CloseTaskSessionSpan effects (!2320)"
```

---

## Chunk 2: Dual-write phase — emit spans alongside old structures

### Task 3: Emit CreateTaskSessionSpan from reviewer dispatch

**Files:**
- Modify: `src/daemon/effects.rs` (AssignReviewer handler also creates span)

The strategy: keep old `AssignReviewer` working but ALSO create a span in the effect handler. No changes needed in `pr.rs` or `health.rs` for this step — they emit the same `AssignReviewer` effect, and the handler does the dual-write.

- [ ] **Step 1: In AssignReviewer handler, also create a span**

In `effects.rs`, after the existing `AssignReviewer` handler code, add:

```rust
// Dual-write: also create a TaskSessionSpan
if let Some(ref tid) = task_id {
    ps.create_span(
        tid,
        &reviewer_name,
        "reviewer",
        reviewer_session_id.as_deref().unwrap_or(""),
    );
    ps.task_pr_number.insert(tid.clone(), pr_number);
}
```

- [ ] **Step 2: In RemoveReviewerAssignment handler, also close spans**

After existing removal code:

```rust
// Dual-write: close any open spans for this PR's reviewer task
let task_ids_to_close: Vec<String> = ps.task_session_spans
    .iter()
    .filter(|s| s.end_time.is_none() && s.agent_type == "reviewer")
    .filter(|s| ps.task_pr_number.get(&s.task_id) == Some(&pr_number))
    .map(|s| s.task_id.clone())
    .collect();
for task_id in task_ids_to_close {
    ps.close_spans_for_task(&task_id);
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: PASS — dual-write is additive

- [ ] **Step 4: Commit**

```bash
git add src/daemon/effects.rs
git commit -m "feat: Dual-write TaskSessionSpans alongside pr_reviewers (!2320)"
```

---

### Task 4: Wire span closure into session shutdown paths

**Files:**
- Modify: `src/daemon/sessions.rs` or `src/daemon/mod.rs` (wherever session shutdown is handled)

- [ ] **Step 1: Find session shutdown handler**

Search for where `is_running = false` is set on `SessionRecord` and add `close_spans_for_session()` calls alongside it.

Key locations:
- Session exit detection in the event loop
- `handle_coworker_exit()` or equivalent
- `ClearOrphanedReviewerAssignments` effect handler

- [ ] **Step 2: Add close_spans_for_session at session exit**

At each point where a session is marked as stopped:

```rust
ps.close_spans_for_session(&session_id);
```

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git commit -am "feat: Close TaskSessionSpans on session shutdown (!2320)"
```

---

## Chunk 3: Migrate snapshot collection to spans

### Task 5: Migrate snapshot.rs to read from spans (with fallback)

**Files:**
- Modify: `src/daemon/snapshot.rs`

This is the key switchover. The snapshot populates `SnapshotReviewerState` — once it reads from spans, all decision functions in `rules.rs` automatically use the new data.

- [ ] **Step 1: Replace compute_active_reviewers_with_health**

Replace the function body to read from `task_session_spans` instead of `github.active_reviewers()`:

```rust
pub(crate) fn compute_active_reviewers_from_spans(
    ps: &DaemonPersistentState,
    process_health: &HashMap<String, ProcessHealth>,
) -> HashSet<String> {
    let mut reviewers = HashSet::new();
    for span in ps.active_reviewer_spans() {
        // Check if session is running
        let is_running = ps.sessions.get(&span.session_id)
            .map(|s| s.is_running)
            .unwrap_or(false);
        // Also check process health
        let is_alive = process_health.get(&span.agent_name)
            .map(|h| h.is_alive)
            .unwrap_or(false);
        if is_running || is_alive {
            reviewers.insert(span.agent_name.clone());
        }
    }
    reviewers
}
```

- [ ] **Step 2: Replace build_reviewer_pr_assignments**

```rust
pub(crate) fn build_reviewer_pr_assignments_from_spans(
    ps: &DaemonPersistentState,
) -> HashMap<String, u64> {
    let mut assignments = HashMap::new();
    for span in ps.active_reviewer_spans() {
        if let Some(&pr) = ps.task_pr_number.get(&span.task_id) {
            assignments.insert(span.agent_name.clone(), pr);
        }
    }
    assignments
}
```

- [ ] **Step 3: Update collect_world_snapshot to use new functions**

Replace the reviewer state collection block (~lines 934-972) to call the new functions.

Also update `reviewer_restart_counts` to read from `task_restart_count` map:

```rust
let reviewer_restart_counts: HashMap<u64, u32> = ps.task_restart_count
    .iter()
    .filter_map(|(task_id, &count)| {
        ps.task_pr_number.get(task_id).map(|&pr| (pr, count))
    })
    .collect();
```

Update `reviewer_in_progress_comment_ids` to read from `task_placeholder_comment_id`:

```rust
let reviewer_in_progress_comment_ids: HashMap<u64, u64> = ps.task_placeholder_comment_id
    .iter()
    .filter_map(|(task_id, &comment_id)| {
        ps.task_pr_number.get(task_id).map(|&pr| (pr, comment_id))
    })
    .collect();
```

- [ ] **Step 4: Remove reviewing_phase_coworkers from SnapshotReviewerState**

Remove the field from `SnapshotReviewerState` AND all consumer-side reads across the codebase. Search for `reviewing_phase_coworkers` in all files — known consumers:
- `src/daemon/snapshot.rs` (field definition + population code)
- `src/daemon/pr.rs` (`augment_reviewer_from_snapshot()` reads it)
- `src/daemon/pr_tests.rs` (test setup)
- `src/daemon/health_tests.rs` (test setup)
- `tests/multi_tick_harness.rs` (snapshot builder)

Remove all reads/writes atomically. The `is_running` check on spans replaces this defense-in-depth guard.

- [ ] **Step 5: Run tests, fix snapshot_tests.rs**

Run: `cargo test`

Update `snapshot_tests.rs` to set up `task_session_spans` instead of `pr_reviewers` for reviewer-related tests. Delete tests for `compute_active_reviewers_with_health` and `build_reviewer_pr_assignments` (old signatures). Add tests for new functions.

- [ ] **Step 6: Commit**

```bash
git commit -am "refactor: Migrate snapshot collection to TaskSessionSpans (!2320)"
```

---

## Chunk 4: Migrate remaining consumer files

### Task 6: Migrate effects.rs — remove dual-write, use spans only

**Files:**
- Modify: `src/daemon/effects.rs`

- [ ] **Step 1: Replace AssignReviewer handler**

Replace the entire handler to only create a span (remove all `pr_reviewers` writes):

```rust
Effect::AssignReviewer { pr_number, reviewer_name, source: _, restart_count, reviewer_session_id, task_id } => {
    let mut ps = state.persistent_state.lock().await;
    if let Some(ref tid) = task_id {
        // Close any existing spans for this task
        ps.close_spans_for_task(tid);
        // Create new span
        ps.create_span(
            tid,
            &reviewer_name,
            "reviewer",
            reviewer_session_id.as_deref().unwrap_or(""),
        );
        ps.task_pr_number.insert(tid.clone(), pr_number);
        if restart_count > 0 {
            ps.task_restart_count.insert(tid.clone(), restart_count);
        }
    }
    ps.save_for_repo(&state.paths.dir_key())?;
}
```

- [ ] **Step 2: Replace RemoveReviewerAssignment handler**

```rust
Effect::RemoveReviewerAssignment { pr_number } => {
    let mut ps = state.persistent_state.lock().await;
    // Find and close reviewer spans for this PR
    let task_ids: Vec<String> = ps.active_reviewer_spans()
        .iter()
        .filter(|s| ps.task_pr_number.get(&s.task_id) == Some(&pr_number))
        .map(|s| s.task_id.clone())
        .collect();
    for tid in task_ids {
        ps.close_spans_for_task(&tid);
    }
    ps.save_for_repo(&state.paths.dir_key())?;
}
```

- [ ] **Step 3: Update lookup_existing_placeholder**

Change 3-tier lookup to use `task_placeholder_comment_id` only (remove `pr_reviewers` fallback).

- [ ] **Step 4: Update update_placeholder_on_pr_comment**

Write to `task_placeholder_comment_id` only (remove `pr_reviewers` dual-write).

- [ ] **Step 5: Run tests, fix effects_tests.rs**

Run: `cargo test`
Fix/rewrite affected tests.

- [ ] **Step 6: Commit**

```bash
git commit -am "refactor: Migrate effects.rs to spans-only (!2320)"
```

---

### Task 7: Migrate pr.rs

**Files:**
- Modify: `src/daemon/pr.rs`

- [ ] **Step 1: Replace is_assigned calls with pr_has_active_reviewer**

Line 1708: `ps.github.is_assigned(pr_number)` → `ps.pr_has_active_reviewer(pr_number)`
Line 2590: `ps.github.is_assigned(pr_number)` → `ps.pr_has_active_reviewer(pr_number)`

- [ ] **Step 2: Replace get_reviewer calls**

Use `ps.active_reviewer_for_pr(pr).map(|s| s.agent_name.as_str())` instead of `ps.github.get_reviewer(pr)`.

- [ ] **Step 3: Remove calls to cleanup_expired_preserving() and backfill_reviewer_session_ids()**

These are `GitHubState` methods called from `pr.rs` (around lines 584-612). Both are unnecessary with spans — session IDs are set at span creation, and running state replaces the timeout heuristic. Remove these call sites and the wrapper logic around them.

- [ ] **Step 4: Run tests, fix pr_tests.rs**

Run: `cargo test`
Update merge-blocking tests to use spans.

- [ ] **Step 5: Commit**

```bash
git commit -am "refactor: Migrate pr.rs to TaskSessionSpans (!2320)"
```

---

### Task 8: Migrate remaining daemon files

**Files:**
- Modify: `src/daemon/mod.rs`
- Modify: `src/daemon/chat.rs`
- Modify: `src/daemon/rpc_prs.rs`
- Modify: `src/daemon/rpc_coworker.rs`
- Modify: `src/daemon/rpc_auth.rs`
- Modify: `src/daemon/rpc_status.rs` (calls `active_assignments()`)
- Modify: `src/daemon/rpc_session.rs` (calls `get_reviewer()`)
- Modify: `src/daemon/health.rs`
- Modify: `src/daemon/dispatch.rs`
- Modify: `src/web.rs`

- [ ] **Step 1: Migrate mod.rs**

- `update_session_health()`: Replace `pr_reviewers.values()` scan with `active_reviewer_spans()` scan
- `schedule_event_channel_for_lead()`: Replace `get_reviewer()` with `active_reviewer_for_pr()`
- `handle_webhook_review_complete()`: Replace `get_reviewer()` with `active_reviewer_for_pr()`
- Remove `PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS` re-export

- [ ] **Step 2: Migrate chat.rs**

Replace 3-tier reviewer session lookup with span-based lookup:
```rust
let span = ps.active_reviewer_for_pr(pr_number);
let session_id = span.map(|s| s.session_id.clone());
```

- [ ] **Step 3: Migrate rpc_prs.rs**

- `handle_spawn_reviewer()`: Replace `get_reviewer()` gate with `pr_has_active_reviewer()`
- `handle_pr_merge()`: Replace `get_reviewer()` with `active_reviewer_for_pr()`
- `handle_pr_review_post()`: Replace `pr_reviewers.get()` with `task_placeholder_comment_id` lookup
- `fetch_prs_all()`: Replace `active_assignments()` with span query

- [ ] **Step 4: Migrate rpc_coworker.rs, rpc_auth.rs**

- `get_available_prs()`: Replace `PrReviewerAssignment` type with span query
- `handle_whoami()`: Replace `pr_for_reviewer()` with span scan

- [ ] **Step 5: Migrate rpc_status.rs, rpc_session.rs**

- `rpc_status.rs`: Replace `active_assignments()` with span-based query for reviewer→PR mapping
- `rpc_session.rs`: Replace `get_reviewer(pr_num)` with `active_reviewer_for_pr(pr_num)`

- [ ] **Step 6: Migrate health.rs**

- `check_and_restart_dead_reviewers()`: Use spans to find dead reviewers and their task IDs

- [ ] **Step 7: Migrate dispatch.rs**

- Replace `snap.reviewer.active_reviewers` reads (these stay the same — snapshot shape unchanged)
- Replace any direct `AssignmentSource` usage

- [ ] **Step 8: Run tests**

Run: `cargo test`
Expected: PASS (may need test fixes)

- [ ] **Step 9: Commit**

```bash
git commit -am "refactor: Migrate remaining daemon files to TaskSessionSpans (!2320)"
```

---

## Chunk 5: Remove old structures and clean up tests

### Task 9: Remove PrReviewerAssignment and pr_reviewers

**Files:**
- Modify: `src/github_state.rs` (remove struct, field, and all methods)
- Modify: `src/daemon/state.rs` (remove TaskReviewerMetadata, task_reviewer_metadata)

- [ ] **Step 1: Remove from github_state.rs**

Delete:
- `PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS` constant
- `OPTIMISTIC_ASSIGNMENT_GRACE_SECS` constant
- `PrReviewerAssignment` struct
- `AssignmentSource` enum and its Display impl
- `pr_reviewers` field from `GitHubState`
- All methods: `assign_reviewer*`, `get_reviewer`, `is_assigned`, `remove_assignment*`, `assigned_reviewers`, `pr_for_reviewer`, `reviewer_has_recent_assignment`, `active_reviewers`, `cleanup_expired_preserving`, `backfill_reviewer_session_ids`
- `default_assignment_source` function
- `active_assignments` method

Keep everything else in `GitHubState` (reviewed_prs, pr_last_webhook_event, rate_limit, pr_review_comment_ids, external_prs, etc.)

- [ ] **Step 2: Remove from state.rs**

Delete:
- `TaskReviewerMetadata` struct
- `task_reviewer_metadata` field from `DaemonPersistentState`
- `task_reviewer_metadata_for_pr()` function
- `clear_reviewer_assignment()` method (if it only touches pr_reviewers)
- Any remaining references in `apply_gc()`

- [ ] **Step 3: Fix all compilation errors**

Run: `cargo build 2>&1 | head -100`

Work through remaining references. At this point all consumers should already be migrated (Tasks 6-8), so errors should be minimal — mostly leftover imports or type references.

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: Compilation errors in test files — fix in next task.

- [ ] **Step 5: Commit**

```bash
git commit -am "refactor: Remove PrReviewerAssignment and TaskReviewerMetadata (!2320)"
```

---

### Task 10: Delete and rewrite test files

**Files:**
- Delete: `src/github_state_tests.rs` (51 tests — all test removed pr_reviewers API)
- Delete: `tests/github_state_reviewer.rs` (31 tests — all test removed lifecycle)
- Modify: `tests/reviewer_break_clears_assignment.rs` (rewrite for spans)
- Modify: `tests/daemon_restart_recovery_e2e.rs` (delete pr_reviewers test, keep session test)
- Modify: `src/daemon/effects_tests.rs` (rewrite AssignReviewer tests)
- Modify: `src/daemon/snapshot_tests.rs` (clean up any remaining failures)
- Modify: `src/daemon/state_tests.rs` (remove task_reviewer_metadata tests)
- Modify: `src/daemon/pr_tests.rs` (update merge blocking tests)
- Modify: `src/daemon/health_tests.rs` (update idle shutdown tests)
- Modify: `src/daemon/dispatch_tests.rs` (update nudge tests)
- Modify: `src/daemon/rpc_prs_tests.rs` (update as needed)
- Modify: `src/daemon/rpc_coworker_tests.rs` (update reviewer idle tests)
- Modify: `src/daemon/rpc_pr_review_tests.rs` (update assignment detection tests)
- Modify: `src/daemon/pr_name_collision_tests.rs` (references `AssignmentSource`)
- Modify: `tests/multi_tick_harness.rs` (matches on `Effect::AssignReviewer`/`RemoveReviewerAssignment`)

- [ ] **Step 1: Delete github_state_tests.rs test module**

Remove the `#[path = "github_state_tests.rs"] #[cfg(test)] mod tests;` line from `github_state.rs` and delete the file.

- [ ] **Step 2: Delete github_state_reviewer.rs**

Remove from `tests/` directory.

- [ ] **Step 3: Rewrite reviewer_break_clears_assignment.rs**

Rewrite tests to verify that breaking a coworker closes its TaskSessionSpan instead of removing from pr_reviewers.

- [ ] **Step 4: Update daemon_restart_recovery_e2e.rs**

Delete `test_reviewer_assignments_preserved_after_restart` test. Keep `test_sessions_preserved_after_restart`. Rewrite `test_persistent_state_prevents_duplicate_spawns` to use spans.

- [ ] **Step 5: Fix remaining test compilation errors**

Run: `cargo test 2>&1 | head -200`
Fix each file until compilation passes.

- [ ] **Step 6: Run full test suite**

Run: `cargo test`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git commit -am "test: Migrate tests to TaskSessionSpan model (!2320)"
```

---

### Task 11: Remove AssignReviewer effect variant

**Files:**
- Modify: `src/daemon/effects.rs` (replace old variants with new)

Once all callers emit `CreateTaskSessionSpan`/`CloseTaskSessionSpan`:

- [ ] **Step 1: Replace AssignReviewer with CreateTaskSessionSpan everywhere**

Search for all `Effect::AssignReviewer` constructions in rules/decision code and replace with `Effect::CreateTaskSessionSpan`.

- [ ] **Step 2: Replace RemoveReviewerAssignment with CloseTaskSessionSpan everywhere**

Search for all `Effect::RemoveReviewerAssignment` and replace.

- [ ] **Step 3: Delete old variants from Effect enum**

Remove `AssignReviewer`, `RemoveReviewerAssignment`, `ClearOrphanedReviewerAssignments` variants and their handlers.

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: PASS

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git commit -am "refactor: Replace reviewer effect variants with span-based effects (!2320)"
```

---

## Chunk 6: Final verification and cleanup

### Task 12: Verify no references remain and run coverage

- [ ] **Step 1: Search for any remaining references**

```bash
grep -r "pr_reviewers\|PrReviewerAssignment\|TaskReviewerMetadata\|task_reviewer_metadata\|AssignmentSource\|ASSIGNMENT_TIMEOUT\|OPTIMISTIC_ASSIGNMENT" src/ tests/ --include="*.rs" -l
```

Expected: No results

- [ ] **Step 2: Run full test suite**

Run: `cargo test`
Expected: PASS

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS

- [ ] **Step 4: Check code coverage**

Run: `./scripts/coverage-diff.sh`
Review coverage of new span code.

- [ ] **Step 5: Update docs/architecture.md**

Document the TaskSessionSpan model in the architecture reference:
- Add section on temporal session tracking
- Document the query helpers and lifecycle
- Remove any references to pr_reviewers

- [ ] **Step 6: Final commit**

```bash
git commit -am "docs: Update architecture for TaskSessionSpan model (!2320)"
```

- [ ] **Step 7: Clean up build artifacts**

```bash
cargo clean
```
