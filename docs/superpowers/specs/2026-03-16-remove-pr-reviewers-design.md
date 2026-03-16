# Remove pr_reviewers — Migrate to Task-Based Session History

**Task:** !2320
**Date:** 2026-03-16
**Status:** Design

## Problem

Reviewer state lives in two redundant structures:

1. **`pr_reviewers`** (`GitHubState`) — keyed by PR number, owns `PrReviewerAssignment` with reviewer name, session ID, assignment time, restart count, placeholder comment ID
2. **`task_reviewer_metadata`** (`DaemonPersistentState`) — keyed by task ID, mirrors PR number, session ID, restart count, placeholder comment ID

The `AssignReviewer` effect dual-writes to both. This creates consistency risks, makes the source of truth unclear, and couples reviewer identity to PR numbers rather than tasks.

Additionally, `is_assigned()` uses a 10-minute timeout heuristic that doesn't reflect actual session lifecycle — a reviewer that's still running after 10 minutes appears unassigned.

## Design

### New Structure: `TaskSessionSpan`

Replace both `pr_reviewers` and `task_reviewer_metadata` with a temporal session history:

```rust
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

Stored as `task_session_spans: Vec<TaskSessionSpan>` on `DaemonPersistentState`.

### Reviewer-Specific Metadata

Fields that are reviewer-specific but not temporal (placeholder comment ID, restart count) stay in existing per-task maps that already exist:

- `task_placeholder_comment_id: HashMap<String, u64>` (already exists, line 262)
- `task_restart_count: HashMap<String, u32>` (already exists, line 269)

These maps are already present in `DaemonPersistentState`. Today they're populated alongside `task_reviewer_metadata` but underused. After this change, they become the canonical store for these fields.

### What Gets Removed

1. **`pr_reviewers: HashMap<u64, PrReviewerAssignment>`** from `GitHubState`
2. **`PrReviewerAssignment`** struct
3. **`task_reviewer_metadata: HashMap<String, TaskReviewerMetadata>`** from `DaemonPersistentState`
4. **`TaskReviewerMetadata`** struct
5. **`AssignmentSource`** enum (assignment source is not needed in the new model — spans track what happened, not why)
6. **`PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS`** and **`OPTIMISTIC_ASSIGNMENT_GRACE_SECS`** constants
7. All `GitHubState` methods that operate on `pr_reviewers`: `assign_reviewer*`, `get_reviewer`, `is_assigned`, `remove_assignment*`, `assigned_reviewers`, `pr_for_reviewer`, `reviewer_has_recent_assignment`, `active_reviewers`, `cleanup_expired_preserving`, `backfill_reviewer_session_ids`

### Query Helpers on `DaemonPersistentState`

```rust
impl DaemonPersistentState {
    /// Find the currently active span for a task (end_time is None).
    pub fn active_span_for_task(&self, task_id: &str) -> Option<&TaskSessionSpan>;

    /// Find all spans for a task, ordered by start_time.
    pub fn spans_for_task(&self, task_id: &str) -> Vec<&TaskSessionSpan>;

    /// Find the active reviewer span for a PR number.
    /// Joins task_id from the span → SessionRecord.pr_number to find the PR.
    /// Also checks task_to_pr_map_from_sessions() as fallback.
    pub fn active_reviewer_for_pr(&self, pr_number: u64) -> Option<&TaskSessionSpan>;

    /// Check if a PR has an active reviewer (replaces is_assigned).
    /// Uses session running state instead of timeout heuristic.
    pub fn pr_has_active_reviewer(&self, pr_number: u64) -> bool;

    /// Get all currently active reviewer spans.
    pub fn active_reviewers(&self) -> Vec<&TaskSessionSpan>;

    /// Close a span (set end_time) when a session stops working on a task.
    pub fn close_span(&mut self, session_id: &str, task_id: &str);

    /// Close all open spans for a session (used on session shutdown).
    pub fn close_spans_for_session(&mut self, session_id: &str);

    /// Close all open spans for a task (used on task completion/cancellation).
    pub fn close_spans_for_task(&mut self, task_id: &str);
}
```

### PR Number Resolution

`active_reviewer_for_pr(pr_number)` needs to map PR → task to find the relevant span. The join path:

1. Filter `task_session_spans` for active reviewer spans (`end_time = None`, `agent_type = "reviewer"`)
2. For each span, look up `SessionRecord` by `span.session_id`
3. Check `SessionRecord.pr_number == Some(pr_number)`

This works because `SessionRecord.pr_number` is already set when a coworker opens a PR and persists across restarts. The existing `pr_to_task_map_from_sessions()` helper uses this same join.

For the assignment window (span created but PR not yet opened by the reviewer), the task's PR number can also be resolved through the task itself — the review task is created with the PR number context. We store this in `task_pr_number: HashMap<String, u64>` (a new per-task map following the existing `task_*` pattern) to ensure the join works before the reviewer session populates `SessionRecord.pr_number`.

### Lifecycle: When Spans Are Created and Closed

| Event | Action |
|---|---|
| Coworker spawned for task | Create span with `start_time = now`, `end_time = None` |
| Reviewer spawned for PR review task | Create span with `agent_type = "reviewer"`, populate `task_pr_number` |
| Session completes/exits | Close span (`end_time = now`) |
| Session is killed/restarted | Close old span, create new span on restart (new session ID) |
| Task handoff | Close old session's span, new session creates its own |
| Daemon restart | Persisted spans survive restart. Open spans for sessions marked `is_running = false` are force-closed during startup reconciliation. Sessions that resume get new spans (they get new session IDs from Claude Code). |

### Migration of Consumer Sites

Key consumer mappings (non-exhaustive, covers the critical paths):

| Old Pattern | New Pattern |
|---|---|
| `github.is_assigned(pr)` | `ps.pr_has_active_reviewer(pr)` |
| `github.get_reviewer(pr)` | `ps.active_reviewer_for_pr(pr).map(\|s\| &s.agent_name)` |
| `github.assign_reviewer(pr, name, source)` | Create `TaskSessionSpan` + use `task_placeholder_comment_id`/`task_restart_count` |
| `github.remove_assignment(pr)` | `ps.close_span(session_id, task_id)` |
| `github.pr_for_reviewer(name)` | Scan active spans for agent_name match |
| `github.assigned_reviewers()` | `ps.active_reviewers().map(\|s\| &s.agent_name)` |
| `github.active_reviewers()` | `ps.active_reviewers()` filtered by running sessions |
| `task_reviewer_metadata.get(task_id)` | `ps.active_span_for_task(task_id)` + `task_placeholder_comment_id`/`task_restart_count` |

### Snapshot Changes

`SnapshotReviewerState` keeps its full shape — only the data source changes (spans replace `pr_reviewers`). This minimizes changes in `rules.rs` decision functions.

```rust
pub struct SnapshotReviewerState {
    /// Active reviewer names (derived from active spans with running sessions).
    pub active_reviewers: HashSet<String>,
    /// Coworkers in reviewing workflow phase — REMOVED.
    /// No longer needed: the 10-minute timeout that this field guarded against
    /// is replaced by is_running checks on active spans. A running reviewer's
    /// span stays open; a dead reviewer's span is closed by session shutdown.
    // pub reviewing_phase_coworkers: HashSet<String>,  // REMOVED
    /// Reviewer name → PR number (from active reviewer spans).
    pub reviewer_pr_assignments: HashMap<String, u64>,
    /// Placeholder comment IDs — data source changes from pr_reviewers to
    /// task_placeholder_comment_id map. Shape unchanged.
    pub reviewer_in_progress_comment_ids: HashMap<u64, u64>,
    /// PRs verified as reviewed — unchanged, sourced from reviewed_prs cache.
    pub reviewed_prs: HashSet<u64>,
    /// Count of open PRs needing review — unchanged, computed from GitHub API.
    pub prs_needing_review: usize,
    /// PR number → restart count — data source changes from pr_reviewers to
    /// task_restart_count map. Shape unchanged.
    pub reviewer_restart_counts: HashMap<u64, u32>,
    /// Escalation tracking — unchanged, sourced from persistent state.
    pub reviewer_escalations_posted: HashSet<u64>,
}
```

**`reviewing_phase_coworkers` removal rationale:** This field was a defense-in-depth guard against the 10-minute `is_assigned()` timeout — it protected reviewers whose assignment had "expired" but whose session was still running. With the new model, `pr_has_active_reviewer()` checks `SessionRecord.is_running` directly, so the timeout-based race condition no longer exists. The guard is unnecessary.

### `is_assigned` Replacement

The 10-minute timeout in `is_assigned()` is replaced by checking whether the session in the active span is actually running:

```rust
pub fn pr_has_active_reviewer(&self, pr_number: u64) -> bool {
    self.active_reviewer_for_pr(pr_number)
        .map(|span| {
            self.sessions.get(&span.session_id)
                .map(|s| s.is_running)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}
```

This is more accurate — a reviewer running for 30 minutes is still "assigned", and a reviewer that exited after 2 minutes is correctly "unassigned".

### GC Strategy

Closed spans (with `end_time`) are retained for historical queries but cleaned up by `apply_gc()`:
- Spans older than 48 hours with `end_time` set are removed (sessions are minutes-to-hours, so 48h provides ample history without unbounded growth)
- Spans for tasks that no longer exist are removed
- Open spans for sessions that no longer exist are force-closed and then subject to normal GC
- Cap: if total spans exceed 500, oldest closed spans are pruned first

### Effect Changes

| Old Effect | New Effect |
|---|---|
| `AssignReviewer { pr_number, reviewer, source, ... }` | `CreateTaskSessionSpan { task_id, agent_name, agent_type, session_id }` |
| `RemoveReviewerAssignment { pr_number }` | `CloseTaskSessionSpan { session_id, task_id }` |
| `SetReviewerSessionId { pr_number, session_id }` | Not needed — session_id is set at span creation |

### Backward Compatibility

Deserialization: `DaemonPersistentState` does NOT use `#[serde(deny_unknown_fields)]`, so removing struct fields is safe — serde ignores unknown keys by default. Both `pr_reviewers` (in `GitHubState`) and `task_reviewer_metadata` already have `#[serde(default)]`. After the structs are removed, old JSON with these fields will deserialize cleanly — unknown fields are silently dropped. No explicit migration is needed since active reviewer assignments are transient (10-minute timeout means they expire naturally before the next daemon restart).

## Scope

- ~26 source files affected
- ~592 occurrences to migrate (roughly half in test files)
- Core changes in: `github_state.rs`, `state.rs`, `effects.rs`, `snapshot.rs`, `pr.rs`
- Test changes in: `github_state_tests.rs`, `github_state_reviewer.rs`, `effects_tests.rs`, `snapshot_tests.rs`, `state_tests.rs`, `pr_tests.rs`, `health_tests.rs`

## Non-Goals

- Changing how dev (non-reviewer) sessions are tracked — they continue using `SessionRecord.task_id`
- Modifying the web UI — it consumes snapshot data that keeps the same shape
- Changing task dispatch logic — dispatch creates spans instead of assignments, but the decision logic in `rules.rs` stays the same
