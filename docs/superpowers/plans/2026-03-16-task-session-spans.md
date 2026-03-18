# Remove pr_reviewers — Temporal Session History Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `pr_reviewers` (GitHubState) and `task_reviewer_metadata` (DaemonPersistentState) with a unified `task_session_spans: Vec<TaskSessionSpan>` temporal session history table.

**Architecture:** A flat append-only log of `{task_id, agent_name, agent_type, session_id, pr_number, start_time, end_time}` spans on `DaemonPersistentState`. Open span (`end_time = None`) = active assignment. Helper methods on `DaemonPersistentState` provide all query patterns (by task, by PR, by name). Existing `task_placeholder_comment_id` and `task_restart_count` maps are retained — they are task-level metadata, not session-level. Migration uses `#[serde(default)]` (no explicit migration code).

**Tech Stack:** Rust, serde, chrono, tokio (async lock)

---

## File Structure

### New files
- `src/daemon/state.rs` — `TaskSessionSpan` struct + helpers added to existing `DaemonPersistentState` impl block
- `src/daemon/state_tests.rs` — Tests for all new helpers (follows existing test file placement)

### Modified files (by phase)
| File | Phase | Changes |
|------|-------|---------|
| `src/daemon/state.rs` | 1,5 | Add struct/field/helpers; remove `TaskReviewerMetadata`, `task_reviewer_metadata`, `task_reviewer_metadata_for_pr()`, `clear_reviewer_assignment()` |
| `src/daemon/state_tests.rs` | 1,5 | Add span tests; update tests that use `task_reviewer_metadata` |
| `src/daemon/effects.rs` | 2,5 | Dual-write spans in `AssignReviewer`; update `RemoveReviewerAssignment`, `lookup_existing_placeholder`, `post_pr_comment` |
| `src/daemon/snapshot.rs` | 3,5 | Migrate reviewer state collection; remove `build_reviewer_pr_assignments`, `compute_active_reviewers_with_health` |
| `src/daemon/snapshot_tests.rs` | 3,5 | Add span-based tests; update tests that use `pr_reviewers` directly |
| `src/daemon/pr.rs` | 4 | Migrate `is_assigned`, `get_reviewer`, `handle_pr_review_post`, remove backfill block |
| `src/daemon/chat.rs` | 4 | Migrate @mention routing |
| `src/daemon/mod.rs` | 4 | Migrate coworker type detection |
| `src/web.rs` | 4 | Migrate PR reviewer display |
| `src/daemon/rpc_prs.rs` | 4 | Migrate `active_assignments` for kanban, `handle_review_post_inner` |
| `src/daemon/rpc_coworker.rs` | 4 | Migrate `active_assignments` for coworker list |
| `src/daemon/rpc_status.rs` | 4 | Migrate reviewer map for status display |
| `src/github_state.rs` | 5 | Remove `PrReviewerAssignment`, `pr_reviewers`, all helper methods |
| `src/github_state_tests.rs` | 5 | Remove/rewrite tests that use `pr_reviewers` |
| `src/daemon/effects_tests.rs` | 5 | Update tests that check `ps.github.pr_reviewers` |
| `src/daemon/rpc_prs_tests.rs` | 5 | Update tests that construct `PrReviewerAssignment` |
| `src/daemon/dispatch_tests.rs` | 5 | Update if references `pr_reviewers` |

---

## Chunk 1: TaskSessionSpan struct, helpers, and tests

### Task 1: Define TaskSessionSpan struct

**Files:**
- Modify: `src/daemon/state.rs:57-67` (after existing `TaskReviewerMetadata`)

- [ ] **Step 1: Add `TaskSessionSpan` struct after `TaskReviewerMetadata` (line 67)**

Add this after the closing brace of `TaskReviewerMetadata`:

```rust
/// A temporal record of a session working on a specific task.
///
/// Spans are append-only: each task dispatch (initial + each restart) opens
/// a new span. An open span (`end_time = None`) means the session is currently
/// active for this task. Restart count for a task lives in `task_restart_count`.
///
/// Lives in `DaemonPersistentState::task_session_spans` as a `Vec<TaskSessionSpan>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSessionSpan {
    /// The task ID (e.g., "2301").
    pub task_id: String,
    /// Coworker name assigned to this task.
    pub agent_name: String,
    /// Agent type (e.g., "midtown-code-reviewer", "dev").
    /// Enables filtering spans by role without joining to `task_agent_type`.
    #[serde(default)]
    pub agent_type: String,
    /// Claude Code session ID for this span. `None` until spawn callback fires
    /// (optimistic assignment window).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The PR being reviewed (0 for non-reviewer tasks).
    #[serde(default)]
    pub pr_number: u64,
    /// When this span started (session dispatched).
    pub start_time: DateTime<Utc>,
    /// When this span ended (None = still active).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<DateTime<Utc>>,
}
```

- [ ] **Step 2: Add `task_session_spans` field to `DaemonPersistentState` (after line 274, the `task_reviewer_metadata` field)**

```rust
    /// Temporal session history for tasks.
    ///
    /// Append-only log of session spans. Open span (end_time=None) means the
    /// session is currently active. Used for reviewer assignment tracking,
    /// @mention routing, and handoff history.
    #[serde(default)]
    pub task_session_spans: Vec<TaskSessionSpan>,
```

- [ ] **Step 3: Add `task_session_spans: Vec::new()` to `migrate_from_legacy` struct literal (after line 528)**

In `state.rs:513-536`, add after `task_reviewer_metadata: HashMap::new(),`:

```rust
            task_session_spans: Vec::new(),
```

- [ ] **Step 4: Add span GC to `apply_gc` (after line 468)**

In the orphaned task pruning loop, add after `self.task_reviewer_metadata.remove(task_id);`:

```rust
            self.task_session_spans.retain(|s| s.task_id != *task_id);
```

- [ ] **Step 5: Run `cargo test` to verify no existing tests break**

Run: `cargo test`
Expected: All tests pass (new field has `#[serde(default)]`)

- [ ] **Step 6: Commit**

```bash
git add src/daemon/state.rs
git commit -m "feat: add TaskSessionSpan struct and field to DaemonPersistentState

Part of !2320: temporal session history to replace pr_reviewers."
```

---

### Task 2: Add query helper methods

**Files:**
- Modify: `src/daemon/state.rs` (add methods to `impl DaemonPersistentState` block)

- [ ] **Step 1: Add `active_span_for_task` helper**

Add to the `impl DaemonPersistentState` block (after `clear_reviewer_assignment`, around line 603):

```rust
    /// Returns the open span for a given task_id, or None.
    pub fn active_span_for_task(&self, task_id: &str) -> Option<&TaskSessionSpan> {
        self.task_session_spans
            .iter()
            .filter(|s| s.task_id == task_id && s.end_time.is_none())
            .max_by_key(|s| s.start_time)
    }

    /// Returns the open span for a given PR number, or None.
    pub fn active_span_for_pr(&self, pr_number: u64) -> Option<&TaskSessionSpan> {
        self.task_session_spans
            .iter()
            .filter(|s| s.pr_number == pr_number && s.end_time.is_none())
            .max_by_key(|s| s.start_time)
    }

    /// Returns the most recent span (open or closed) for a PR number.
    /// Used when the active span may have already been closed (e.g., review-post race).
    pub fn most_recent_span_for_pr(&self, pr_number: u64) -> Option<&TaskSessionSpan> {
        self.task_session_spans
            .iter()
            .filter(|s| s.pr_number == pr_number)
            .max_by_key(|s| s.start_time)
    }

    /// Returns the open span for a coworker name (case-insensitive).
    /// Uses `max_by_key` for consistency with other `active_span_for_*` helpers.
    pub fn active_span_for_name(&self, agent_name: &str) -> Option<&TaskSessionSpan> {
        self.task_session_spans
            .iter()
            .filter(|s| s.agent_name.eq_ignore_ascii_case(agent_name) && s.end_time.is_none())
            .max_by_key(|s| s.start_time)
    }

    /// Returns reviewer name for a PR from the open span, or None.
    /// Replaces `GitHubState::get_reviewer(pr_number)`.
    pub fn reviewer_name_for_pr(&self, pr_number: u64) -> Option<&str> {
        self.active_span_for_pr(pr_number)
            .map(|s| s.agent_name.as_str())
    }

    /// Returns whether a PR has an active (open-span) reviewer assignment.
    /// Replaces `GitHubState::is_assigned(pr_number)` — open span = assigned.
    pub fn pr_is_assigned(&self, pr_number: u64) -> bool {
        self.active_span_for_pr(pr_number).is_some()
    }

    /// Returns reviewer_name → pr_number map for all open spans.
    /// Intentionally includes dead reviewers (no timeout filter) so
    /// `decide_dead_reviewer_respawns` can detect and respawn them.
    /// When a reviewer has multiple open spans, keeps the most recently started.
    pub fn all_reviewer_pr_assignments(&self) -> HashMap<String, u64> {
        let mut result: HashMap<String, (u64, DateTime<Utc>)> = HashMap::new();
        for span in &self.task_session_spans {
            if span.end_time.is_none() && span.pr_number > 0 {
                let dominated = result
                    .get(&span.agent_name)
                    .is_some_and(|(_, existing_at)| span.start_time <= *existing_at);
                if !dominated {
                    result.insert(
                        span.agent_name.clone(),
                        (span.pr_number, span.start_time),
                    );
                }
            }
        }
        result
            .into_iter()
            .map(|(name, (pr, _))| (name, pr))
            .collect()
    }

    /// Returns the set of reviewer names with open spans (active reviewers).
    /// Replaces `GitHubState::active_reviewers()`.
    pub fn active_reviewer_names(&self) -> HashSet<String> {
        self.task_session_spans
            .iter()
            .filter(|s| s.end_time.is_none() && s.pr_number > 0)
            .map(|s| s.agent_name.clone())
            .collect()
    }

    /// Returns active assignments as pr_number → (reviewer_name, start_time).
    /// Replaces `GitHubState::active_assignments()`.
    pub fn active_reviewer_assignments(&self) -> HashMap<u64, (String, DateTime<Utc>)> {
        let mut result = HashMap::new();
        for span in &self.task_session_spans {
            if span.end_time.is_none() && span.pr_number > 0 {
                let dominated = result
                    .get(&span.pr_number)
                    .is_some_and(|(_, existing_at): &(String, DateTime<Utc>)| {
                        span.start_time <= *existing_at
                    });
                if !dominated {
                    result.insert(
                        span.pr_number,
                        (span.agent_name.clone(), span.start_time),
                    );
                }
            }
        }
        result
    }
```

- [ ] **Step 2: Run `cargo check` to verify compilation**

Run: `cargo check`
Expected: Compiles cleanly

- [ ] **Step 3: Commit**

```bash
git add src/daemon/state.rs
git commit -m "feat: add query helpers for task_session_spans

active_span_for_task/pr/name, reviewer_name_for_pr, pr_is_assigned,
all_reviewer_pr_assignments, active_reviewer_names, active_reviewer_assignments."
```

---

### Task 3: Add mutation helper methods

**Files:**
- Modify: `src/daemon/state.rs` (continue adding methods)

- [ ] **Step 1: Add mutation helpers after the query helpers**

```rust
    /// Opens a new span for a task session.
    ///
    /// Closes any existing open span for the same task_id first (defensive
    /// against crash recovery or restart path leaving an orphaned open span).
    pub fn open_session_span(
        &mut self,
        task_id: String,
        agent_name: String,
        agent_type: String,
        session_id: Option<String>,
        pr_number: u64,
    ) {
        let now = Utc::now();
        // Close any existing open span for this task (restart/respawn path).
        for span in self.task_session_spans.iter_mut() {
            if span.task_id == task_id && span.end_time.is_none() {
                span.end_time = Some(now);
            }
        }
        self.task_session_spans.push(TaskSessionSpan {
            task_id,
            agent_name,
            agent_type,
            session_id,
            pr_number,
            start_time: now,
            end_time: None,
        });
    }

    /// Closes the open span for a task (review complete, task cancelled, etc.).
    /// Returns true if a span was closed.
    pub fn close_session_span_for_task(&mut self, task_id: &str) -> bool {
        let now = Utc::now();
        let mut closed = false;
        for span in self.task_session_spans.iter_mut() {
            if span.task_id == task_id && span.end_time.is_none() {
                span.end_time = Some(now);
                closed = true;
            }
        }
        closed
    }

    /// Closes the open span for a PR (by pr_number).
    /// Replaces `GitHubState::remove_assignment(pr_number)`.
    pub fn close_session_span_for_pr(&mut self, pr_number: u64) -> bool {
        let now = Utc::now();
        let mut closed = false;
        for span in self.task_session_spans.iter_mut() {
            if span.pr_number == pr_number && span.end_time.is_none() {
                span.end_time = Some(now);
                closed = true;
            }
        }
        closed
    }

    /// Closes the open span for a reviewer by name.
    /// Returns the PR number if a span was closed.
    /// Replaces `remove_assignment_by_reviewer` + `clear_reviewer_assignment`.
    pub fn close_session_span_by_name(&mut self, agent_name: &str) -> Option<u64> {
        let now = Utc::now();
        let mut pr_number = None;
        for span in self.task_session_spans.iter_mut() {
            if span.agent_name.eq_ignore_ascii_case(agent_name) && span.end_time.is_none() {
                span.end_time = Some(now);
                pr_number = Some(span.pr_number);
            }
        }
        pr_number
    }

    /// Backfill session_id on open spans matching the given agent_name.
    /// Called from the session ID backfill path in pr.rs.
    pub fn backfill_span_session_id(&mut self, agent_name: &str, session_id: &str) {
        for span in self.task_session_spans.iter_mut() {
            if span.agent_name.eq_ignore_ascii_case(agent_name)
                && span.end_time.is_none()
                && span.session_id.is_none()
            {
                span.session_id = Some(session_id.to_string());
            }
        }
    }

    /// Whether a named reviewer has an open span that started within the
    /// given timeout window. Used for the alive-grace extension in
    /// `compute_active_reviewers_with_health`.
    pub fn reviewer_has_recent_span(&self, agent_name: &str, timeout_secs: u64) -> bool {
        let limit = chrono::Duration::seconds(timeout_secs as i64);
        self.task_session_spans.iter().any(|s| {
            s.agent_name.eq_ignore_ascii_case(agent_name)
                && s.end_time.is_none()
                && Utc::now().signed_duration_since(s.start_time) < limit
        })
    }
```

- [ ] **Step 2: Run `cargo check`**

Run: `cargo check`
Expected: Compiles cleanly

- [ ] **Step 3: Commit**

```bash
git add src/daemon/state.rs
git commit -m "feat: add mutation helpers for task_session_spans

open_session_span, close_session_span_for_task/pr/name,
backfill_span_session_id, reviewer_has_recent_span."
```

---

### Task 4: Write tests for all helpers

**Files:**
- Modify: `src/daemon/state_tests.rs`

- [ ] **Step 1: Add span helper tests**

Add at the end of `state_tests.rs`:

```rust
// ── TaskSessionSpan helpers ─────────────────────────────────────────────

#[test]
fn test_open_session_span_creates_span() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span(
        "100".to_string(),
        "lexington".to_string(),
        "midtown-code-reviewer".to_string(),
        Some("sess-1".to_string()),
        42,
    );
    assert_eq!(ps.task_session_spans.len(), 1);
    let span = &ps.task_session_spans[0];
    assert_eq!(span.task_id, "100");
    assert_eq!(span.agent_name, "lexington");
    assert_eq!(span.agent_type, "midtown-code-reviewer");
    assert_eq!(span.session_id, Some("sess-1".to_string()));
    assert_eq!(span.pr_number, 42);
    assert!(span.end_time.is_none());
}

#[test]
fn test_open_session_span_closes_existing_open_span() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    ps.open_session_span("100".into(), "park".into(), "reviewer".into(), None, 42);
    assert_eq!(ps.task_session_spans.len(), 2);
    // First span should be closed
    assert!(ps.task_session_spans[0].end_time.is_some());
    // Second span should be open
    assert!(ps.task_session_spans[1].end_time.is_none());
}

#[test]
fn test_active_span_for_task() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    assert!(ps.active_span_for_task("100").is_some());
    assert!(ps.active_span_for_task("999").is_none());
}

#[test]
fn test_active_span_for_pr() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    assert!(ps.active_span_for_pr(42).is_some());
    assert!(ps.active_span_for_pr(99).is_none());
}

#[test]
fn test_active_span_for_name_case_insensitive() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "Lexington".into(), "reviewer".into(), None, 42);
    assert!(ps.active_span_for_name("lexington").is_some());
    assert!(ps.active_span_for_name("LEXINGTON").is_some());
    assert!(ps.active_span_for_name("park").is_none());
}

#[test]
fn test_reviewer_name_for_pr() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    assert_eq!(ps.reviewer_name_for_pr(42), Some("lex"));
    assert_eq!(ps.reviewer_name_for_pr(99), None);
}

#[test]
fn test_pr_is_assigned() {
    let mut ps = DaemonPersistentState::default();
    assert!(!ps.pr_is_assigned(42));
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    assert!(ps.pr_is_assigned(42));
}

#[test]
fn test_close_session_span_for_task() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    assert!(ps.pr_is_assigned(42));
    assert!(ps.close_session_span_for_task("100"));
    assert!(!ps.pr_is_assigned(42));
    // Closing again returns false
    assert!(!ps.close_session_span_for_task("100"));
}

#[test]
fn test_close_session_span_for_pr() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    assert!(ps.close_session_span_for_pr(42));
    assert!(!ps.pr_is_assigned(42));
}

#[test]
fn test_close_session_span_by_name() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    assert_eq!(ps.close_session_span_by_name("lex"), Some(42));
    assert!(!ps.pr_is_assigned(42));
    assert_eq!(ps.close_session_span_by_name("lex"), None);
}

#[test]
fn test_all_reviewer_pr_assignments_includes_dead_reviewers() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    ps.open_session_span("101".into(), "park".into(), "reviewer".into(), None, 43);
    let assignments = ps.all_reviewer_pr_assignments();
    assert_eq!(assignments.len(), 2);
    assert_eq!(assignments["lex"], 42);
    assert_eq!(assignments["park"], 43);
}

#[test]
fn test_all_reviewer_pr_assignments_keeps_most_recent() {
    let mut ps = DaemonPersistentState::default();
    // Manually push two open spans for same reviewer (edge case)
    ps.task_session_spans.push(TaskSessionSpan {
        task_id: "100".into(),
        agent_name: "lex".into(),
        agent_type: "reviewer".into(),
        session_id: None,
        pr_number: 42,
        start_time: Utc::now() - chrono::Duration::seconds(100),
        end_time: None,
    });
    ps.task_session_spans.push(TaskSessionSpan {
        task_id: "101".into(),
        agent_name: "lex".into(),
        agent_type: "reviewer".into(),
        session_id: None,
        pr_number: 43,
        start_time: Utc::now(),
        end_time: None,
    });
    let assignments = ps.all_reviewer_pr_assignments();
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments["lex"], 43); // most recent
}

#[test]
fn test_all_reviewer_pr_assignments_excludes_closed_spans() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    ps.close_session_span_for_task("100");
    let assignments = ps.all_reviewer_pr_assignments();
    assert!(assignments.is_empty());
}

#[test]
fn test_active_reviewer_names() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    ps.open_session_span("101".into(), "park".into(), "reviewer".into(), None, 43);
    let names = ps.active_reviewer_names();
    assert!(names.contains("lex"));
    assert!(names.contains("park"));
    assert_eq!(names.len(), 2);
}

#[test]
fn test_active_reviewer_assignments() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    let assignments = ps.active_reviewer_assignments();
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[&42].0, "lex");
}

#[test]
fn test_backfill_span_session_id() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    assert!(ps.task_session_spans[0].session_id.is_none());
    ps.backfill_span_session_id("lex", "sess-abc");
    assert_eq!(
        ps.task_session_spans[0].session_id,
        Some("sess-abc".to_string())
    );
}

#[test]
fn test_backfill_span_session_id_skips_already_set() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span(
        "100".into(),
        "lex".into(),
        "reviewer".into(),
        Some("sess-original".into()),
        42,
    );
    ps.backfill_span_session_id("lex", "sess-new");
    // Should not overwrite
    assert_eq!(
        ps.task_session_spans[0].session_id,
        Some("sess-original".to_string())
    );
}

#[test]
fn test_most_recent_span_for_pr_includes_closed() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    ps.close_session_span_for_pr(42);
    // active_span_for_pr returns None (closed)
    assert!(ps.active_span_for_pr(42).is_none());
    // most_recent_span_for_pr still returns the closed span
    let span = ps.most_recent_span_for_pr(42).unwrap();
    assert_eq!(span.agent_name, "lex");
    assert!(span.end_time.is_some());
}

#[test]
fn test_apply_gc_prunes_orphaned_task_spans() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    ps.open_session_span("200".into(), "park".into(), "reviewer".into(), None, 43);
    let result = ps.apply_gc(&[], &["100".to_string()]);
    assert_eq!(result.orphaned_tasks_pruned, 1);
    assert_eq!(ps.task_session_spans.len(), 1);
    assert_eq!(ps.task_session_spans[0].task_id, "200");
}

#[test]
fn test_reviewer_has_recent_span() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    // Fresh span — should be within any reasonable timeout
    assert!(ps.reviewer_has_recent_span("lex", 600));
    assert!(!ps.reviewer_has_recent_span("park", 600));
}

#[test]
fn test_span_serialization_roundtrip() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span(
        "100".into(),
        "lex".into(),
        "midtown-code-reviewer".into(),
        Some("sess-1".into()),
        42,
    );
    let json = serde_json::to_string(&ps).unwrap();
    let deserialized: DaemonPersistentState = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.task_session_spans.len(), 1);
    assert_eq!(deserialized.task_session_spans[0].agent_type, "midtown-code-reviewer");
}

#[test]
fn test_span_deserialization_with_missing_field() {
    // Simulate old daemon-state.json without task_session_spans
    let json = r#"{"sessions": {}}"#;
    let ps: DaemonPersistentState = serde_json::from_str(json).unwrap();
    assert!(ps.task_session_spans.is_empty());
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test state_tests`
Expected: All new and existing tests pass

- [ ] **Step 3: Commit**

```bash
git add src/daemon/state_tests.rs
git commit -m "test: add comprehensive tests for TaskSessionSpan helpers"
```

---

## Chunk 2: Dual-write and span lifecycle

### Task 5: Dual-write spans in AssignReviewer effect handler

**Files:**
- Modify: `src/daemon/effects.rs:1869-1913`

- [ ] **Step 1: Add span write to `AssignReviewer` handler**

After the existing `task_reviewer_metadata` upsert block (line 1910), before the `save_for_repo` call (line 1911), add:

```rust
                // Write to task_session_spans (temporal session history).
                if let Some(ref tid) = task_id {
                    let agent_type = ps
                        .task_agent_type
                        .get(tid)
                        .cloned()
                        .unwrap_or_else(|| "dev".to_string());
                    ps.open_session_span(
                        tid.clone(),
                        reviewer_name.clone(),
                        agent_type,
                        reviewer_session_id.clone(),
                        pr_number,
                    );
                }
```

- [ ] **Step 2: Run `cargo test`**

Run: `cargo test`
Expected: All tests pass

- [ ] **Step 3: Commit**

```bash
git add src/daemon/effects.rs
git commit -m "feat: dual-write task_session_spans in AssignReviewer handler"
```

---

### Task 6: Close spans on reviewer removal

**Files:**
- Modify: `src/daemon/effects.rs:1915-1929` (`RemoveReviewerAssignment` handler)
- Modify: `src/daemon/state.rs:586-602` (`clear_reviewer_assignment`)
- Modify: `src/daemon/pr.rs:2590` (review completion path)

- [ ] **Step 1: Add span close to `RemoveReviewerAssignment` handler**

In `effects.rs`, inside the `RemoveReviewerAssignment` handler (after line 1917's `remove_assignment` call), add:

```rust
                ps.close_session_span_for_pr(pr_number);
```

- [ ] **Step 2: Add span close to `clear_reviewer_assignment`**

In `state.rs:586`, add span close before the `save_for_repo` call:

```rust
    pub fn clear_reviewer_assignment(&mut self, reviewer_name: &str, repo: &str) -> bool {
        if let Some(assignment) = self.github.remove_assignment_by_reviewer(reviewer_name) {
            self.close_session_span_by_name(reviewer_name);
            tracing::info!(
```

- [ ] **Step 3: Add span close in review completion path**

In `pr.rs`, find the block around line 2590 where `ps.github.remove_assignment(pr_number)` is called. Add after it:

```rust
                ps.close_session_span_for_pr(pr_number);
```

Note: Read `pr.rs:2585-2600` first to find the exact location.

- [ ] **Step 4: Run `cargo test`**

Run: `cargo test`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add src/daemon/effects.rs src/daemon/state.rs src/daemon/pr.rs
git commit -m "feat: close task_session_spans on reviewer removal/completion"
```

---

### Task 6b: Close spans when PRs are cleaned up

**Files:**
- Modify: `src/daemon/pr.rs:715-724`

The existing `cleanup_closed_prs` (line 718) and `cleanup_expired_preserving` (line 720) remove entries from `pr_reviewers` but don't close spans. Without this, removed `pr_reviewers` entries would leave orphaned open spans that make `pr_is_assigned` return stale `true`.

- [ ] **Step 1: Add `close_spans_for_closed_prs` helper to `DaemonPersistentState`**

Add to the mutation helpers in `state.rs`:

```rust
    /// Close open spans for PRs that are no longer open.
    /// Counterpart to `GitHubState::cleanup_closed_prs`.
    pub fn close_spans_for_closed_prs(&mut self, open_pr_numbers: &[u64]) {
        let open_set: HashSet<u64> = open_pr_numbers.iter().copied().collect();
        let now = Utc::now();
        for span in self.task_session_spans.iter_mut() {
            if span.pr_number > 0
                && span.end_time.is_none()
                && !open_set.contains(&span.pr_number)
            {
                span.end_time = Some(now);
            }
        }
    }
```

- [ ] **Step 2: Call the helper in `pr.rs:718` cleanup block**

In `pr.rs`, after `ps.github.cleanup_closed_prs(&open_pr_numbers)` (line 718), add:

```rust
        ps.close_spans_for_closed_prs(&open_pr_numbers);
```

- [ ] **Step 3: Add test for `close_spans_for_closed_prs`**

In `state_tests.rs`:

```rust
#[test]
fn test_close_spans_for_closed_prs() {
    let mut ps = DaemonPersistentState::default();
    ps.open_session_span("100".into(), "lex".into(), "reviewer".into(), None, 42);
    ps.open_session_span("101".into(), "park".into(), "reviewer".into(), None, 43);
    // PR 42 is still open, PR 43 is closed
    ps.close_spans_for_closed_prs(&[42]);
    assert!(ps.pr_is_assigned(42)); // still open
    assert!(!ps.pr_is_assigned(43)); // closed
}
```

- [ ] **Step 4: Run `cargo test`**

- [ ] **Step 5: Commit**

```bash
git add src/daemon/state.rs src/daemon/state_tests.rs src/daemon/pr.rs
git commit -m "feat: close task_session_spans when PRs are cleaned up"
```

---

### Task 7: Backfill span session IDs from pr.rs

**Files:**
- Modify: `src/daemon/pr.rs:590-614`

- [ ] **Step 1: Add span backfill alongside existing backfill**

In `pr.rs`, after the existing `task_reviewer_metadata` backfill loop (around line 608-613), add:

```rust
        // Also backfill task_session_spans session_id from running coworker sessions.
        for (name, sid) in &reviewer_session_map {
            ps.backfill_span_session_id(name, sid);
        }
```

- [ ] **Step 2: Run `cargo test`**

Run: `cargo test`
Expected: All tests pass

- [ ] **Step 3: Commit**

```bash
git add src/daemon/pr.rs
git commit -m "feat: backfill task_session_spans session_id from running coworkers"
```

---

## Chunk 3: Migrate snapshot.rs reads

### Task 8: Migrate reviewer state collection in snapshot.rs

**Files:**
- Modify: `src/daemon/snapshot.rs:934-1032`

- [ ] **Step 1: Read the current reviewer state block**

Read `src/daemon/snapshot.rs:934-1070` to understand the full block.

- [ ] **Step 2: Replace `build_reviewer_pr_assignments` call with span-based lookup**

Replace lines 942 with:

```rust
        // Build reviewer → PR assignments: prefer task_session_spans (open spans),
        // fall back to legacy pr_reviewers during transition.
        let span_assignments = ps.all_reviewer_pr_assignments();
        let assignments = if !span_assignments.is_empty() {
            span_assignments
        } else {
            build_reviewer_pr_assignments(&ps.github)
        };
```

- [ ] **Step 3: Replace `compute_active_reviewers_with_health` usage**

Replace the call at line 937 with:

```rust
        // Active reviewers: prefer spans, augment with alive-process grace.
        let span_reviewer_names = ps.active_reviewer_names();
        let reviewers = if !span_reviewer_names.is_empty() {
            let mut set = span_reviewer_names;
            // Grace extension: include alive reviewers with recent spans
            for (name, health) in &headless_process_health {
                if health.is_alive
                    && ps.reviewer_has_recent_span(
                        name,
                        crate::github_state::PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS
                            + REVIEWER_ALIVE_GRACE_SECS,
                    )
                {
                    set.insert(name.clone());
                }
            }
            set
        } else {
            compute_active_reviewers_with_health(&ps.github, &headless_process_health)
        };
```

- [ ] **Step 4: Replace restart_counts collection to use spans + task_restart_count**

Replace the entire `restart_counts` block (lines 944-960) with:

```rust
        // Collect PR → restart_count for stuck reviewer backoff.
        // Use task_restart_count via span's task_id; fall back to legacy sources.
        let restart_counts: HashMap<u64, u32> = {
            let mut counts: HashMap<u64, u32> = HashMap::new();
            // Primary: derive from task_restart_count via active spans
            for span in &ps.task_session_spans {
                if span.pr_number > 0 {
                    if let Some(&count) = ps.task_restart_count.get(&span.task_id) {
                        if count > 0 {
                            counts.insert(span.pr_number, count);
                        }
                    }
                }
            }
            // Legacy fallback: pr_reviewers (removed in Phase 5)
            if counts.is_empty() {
                for (pr, a) in &ps.github.pr_reviewers {
                    if a.restart_count > 0 {
                        counts.insert(*pr, a.restart_count);
                    }
                }
                for meta in ps.task_reviewer_metadata.values() {
                    if meta.restart_count > 0 {
                        counts.insert(meta.pr_number, meta.restart_count);
                    }
                }
            }
            counts
        };
```

- [ ] **Step 5: Update stored_placeholder_ids to prefer spans for task_id lookup**

In the `stored_placeholder_ids` block (lines 1016-1032), update the lookup to also try finding the task_id via spans:

```rust
            assigned_unreviewed_prs
                .iter()
                .map(|&pr| {
                    // Prefer task_placeholder_comment_id via span's task_id
                    let span_placeholder = ps
                        .active_span_for_pr(pr)
                        .or_else(|| ps.most_recent_span_for_pr(pr))
                        .and_then(|s| ps.task_placeholder_comment_id.get(&s.task_id).copied());

                    let id = span_placeholder
                        .or_else(|| {
                            super::state::task_reviewer_metadata_for_pr(&ps, pr)
                                .and_then(|m| m.placeholder_comment_id)
                        })
                        .or_else(|| {
                            ps.github
                                .pr_reviewers
                                .get(&pr)
                                .and_then(|a| a.placeholder_comment_id)
                        });
                    (pr, id)
                })
                .collect()
```

- [ ] **Step 6: Run `cargo test`**

Run: `cargo test`
Expected: All tests pass

- [ ] **Step 7: Commit**

```bash
git add src/daemon/snapshot.rs
git commit -m "feat: migrate snapshot.rs reviewer state to prefer task_session_spans

Falls back to legacy pr_reviewers during transition period."
```

---

### Task 9: Add snapshot tests for span-based lookup

**Files:**
- Modify: `src/daemon/snapshot_tests.rs`

- [ ] **Step 1: Read existing snapshot tests for reviewer state**

Read `src/daemon/snapshot_tests.rs` to find tests that exercise `build_reviewer_pr_assignments` and `compute_active_reviewers_with_health`.

- [ ] **Step 2: Add parallel span-based tests**

Add tests that exercise the new span-based paths. These should verify that when spans are populated, they take precedence over `pr_reviewers`. Tests should:
- Create spans via `ps.open_session_span()`
- Verify `all_reviewer_pr_assignments()` returns correct data
- Verify `active_reviewer_names()` returns correct data
- Verify closed spans are excluded from active sets

- [ ] **Step 3: Run `cargo test`**

Run: `cargo test snapshot_tests`
Expected: All new and existing tests pass

- [ ] **Step 4: Commit**

```bash
git add src/daemon/snapshot_tests.rs
git commit -m "test: add snapshot tests for span-based reviewer state collection"
```

---

## Chunk 4: Migrate remaining consumer sites

### Task 10: Migrate pr.rs consumers

**Files:**
- Modify: `src/daemon/pr.rs`

- [ ] **Step 1: Read all `pr_reviewers` usage in pr.rs**

Run: `grep -n 'pr_reviewers\|is_assigned\|get_reviewer' src/daemon/pr.rs`
Read the surrounding context for each match.

- [ ] **Step 2: Migrate `is_assigned` and `get_reviewer` checks**

At line ~1708, replace:
```rust
.filter(|&n| ps.github.is_assigned(n))
```
With:
```rust
.filter(|&n| ps.github.is_assigned(n) || ps.pr_is_assigned(n))
```

At line ~1714, replace:
```rust
if ps.github.get_reviewer(n).is_some() && !ps.github.has_cached_review(n) {
```
With:
```rust
if (ps.reviewer_name_for_pr(n).is_some() || ps.github.get_reviewer(n).is_some())
    && !ps.github.has_cached_review(n)
{
```

At line ~2590, replace:
```rust
if ps.github.is_assigned(pr_number) {
```
With:
```rust
if ps.github.is_assigned(pr_number) || ps.pr_is_assigned(pr_number) {
```

- [ ] **Step 3: Migrate `handle_pr_review_post` reviewer lookup at line ~3567**

Read the exact context first. Replace the `pr_reviewers.get` lookup with a span-based lookup that falls back to legacy:

```rust
let reviewer_name = ps
    .reviewer_name_for_pr(pr_number)
    .map(|s| s.to_string())
    .or_else(|| ps.github.pr_reviewers.get(&pr_number).map(|a| a.reviewer.clone()));
```

- [ ] **Step 4: Run `cargo test`**

Run: `cargo test`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add src/daemon/pr.rs
git commit -m "feat: migrate pr.rs consumers to prefer task_session_spans"
```

---

### Task 11: Migrate chat.rs @mention routing

**Files:**
- Modify: `src/daemon/chat.rs:238-254`

- [ ] **Step 1: Read the @mention routing block**

Read `src/daemon/chat.rs:230-260`.

- [ ] **Step 2: Add span-based lookup before legacy fallback**

Replace the reviewer_session lookup block with:

```rust
        let reviewer_session = {
            let ps = state.persistent_state.lock().await;
            // Prefer task_session_spans for direct session_id lookup.
            ps.active_span_for_name(&target_name)
                .and_then(|span| {
                    span.session_id.as_ref().map(|sid| ReviewerSessionInfo {
                        session_id: sid.clone(),
                        pr_number: span.pr_number,
                    })
                })
                .or_else(|| {
                    // Legacy fallback: pr_reviewers → task_reviewer_metadata
                    ps.github
                        .pr_reviewers
                        .values()
                        .find(|a| a.reviewer.eq_ignore_ascii_case(&target_name))
                        .and_then(|a| {
                            let session_id =
                                super::state::task_reviewer_metadata_for_pr(&ps, a.pr_number)
                                    .and_then(|m| m.reviewer_session_id.clone())
                                    .or_else(|| a.reviewer_session_id.clone());
                            session_id.map(|sid| ReviewerSessionInfo {
                                session_id: sid,
                                pr_number: a.pr_number,
                            })
                        })
                })
        };
```

Note: Check `ReviewerSessionInfo` struct definition — it may have a `task_id` field instead of `pr_number`. Adjust accordingly based on what you find when reading the code.

- [ ] **Step 3: Run `cargo test`**

Run: `cargo test`
Expected: All tests pass

- [ ] **Step 4: Commit**

```bash
git add src/daemon/chat.rs
git commit -m "feat: migrate chat.rs @mention routing to prefer task_session_spans"
```

---

### Task 12: Migrate mod.rs coworker type detection

**Files:**
- Modify: `src/daemon/mod.rs:2732-2757`

- [ ] **Step 1: Read the coworker type detection block**

Read `src/daemon/mod.rs:2725-2760`.

- [ ] **Step 2: Replace pr_reviewers scan with span lookup**

Replace:
```rust
            let is_reviewer = persistent
                .github
                .pr_reviewers
                .values()
                .any(|assignment| assignment.reviewer == coworker.name);
```
With:
```rust
            let is_reviewer = persistent
                .active_span_for_name(&coworker.name)
                .is_some()
                || persistent
                    .github
                    .pr_reviewers
                    .values()
                    .any(|assignment| assignment.reviewer == coworker.name);
```

And replace the inner `pr_reviewers.values().find(...)` block with:
```rust
                if let Some(span) = persistent.active_span_for_name(&coworker.name) {
                    info.pr_number = Some(span.pr_number);
                    info.purpose = format!("reviewer for PR #{}", span.pr_number);
                } else if let Some(assignment) = persistent
                    .github
                    .pr_reviewers
                    .values()
                    .find(|a| a.reviewer == coworker.name)
                {
                    info.pr_number = Some(assignment.pr_number);
                    info.purpose = format!("reviewer for PR #{}", assignment.pr_number);
                } else {
                    info.purpose = "reviewer (unassigned)".to_string();
                }
```

- [ ] **Step 3: Run `cargo test`**

- [ ] **Step 4: Commit**

```bash
git add src/daemon/mod.rs
git commit -m "feat: migrate mod.rs coworker type detection to prefer task_session_spans"
```

---

### Task 13: Migrate web.rs PR reviewer display

**Files:**
- Modify: `src/web.rs:1170`

- [ ] **Step 1: Read the PR reviewer display block**

Read `src/web.rs:1160-1180`.

- [ ] **Step 2: Add span-based lookup with legacy fallback**

Replace:
```rust
            let assignment = persistent_state.github.pr_reviewers.get(&pr_number);
            let reviewer = pr
                .get("reviewer")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| assignment.map(|a| a.reviewer.clone()));
            let reviewer_assigned_at = assignment.map(|a| a.assigned_at.to_rfc3339());
```
With:
```rust
            let span = persistent_state.active_span_for_pr(pr_number);
            let assignment = persistent_state.github.pr_reviewers.get(&pr_number);
            let reviewer = pr
                .get("reviewer")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| span.map(|s| s.agent_name.clone()))
                .or_else(|| assignment.map(|a| a.reviewer.clone()));
            let reviewer_assigned_at = span
                .map(|s| s.start_time.to_rfc3339())
                .or_else(|| assignment.map(|a| a.assigned_at.to_rfc3339()));
```

- [ ] **Step 3: Run `cargo test`**

- [ ] **Step 4: Commit**

```bash
git add src/web.rs
git commit -m "feat: migrate web.rs PR reviewer display to prefer task_session_spans"
```

---

### Task 14: Migrate RPC handlers (rpc_prs.rs, rpc_coworker.rs, rpc_status.rs)

**Files:**
- Modify: `src/daemon/rpc_prs.rs:58-62`
- Modify: `src/daemon/rpc_coworker.rs:1133`
- Modify: `src/daemon/rpc_status.rs:57-63`

- [ ] **Step 1: Read all three RPC handler usages**

Read `rpc_prs.rs:55-65`, `rpc_coworker.rs:1128-1140`, `rpc_status.rs:55-65`.

- [ ] **Step 2: Migrate `rpc_prs.rs:58` — kanban active assignments**

For `rpc_prs.rs` — `reviewer_assignments` is `HashMap<u64, PrReviewerAssignment>`. During transition, construct synthetic `PrReviewerAssignment` from span data or keep legacy path. After Phase 5, change `fetch_prs_all`'s signature to not require `PrReviewerAssignment`.

- [ ] **Step 3: Migrate `rpc_prs.rs:1026` — `handle_review_post_inner` reviewer lookup**

Replace the `ps.github.pr_reviewers.get(&pr_number)` block at line 1026-1041 with:

```rust
        let reviewer_name_val = ps
            .reviewer_name_for_pr(pr_number)
            .map(|s| s.to_string())
            .or_else(|| ps.github.pr_reviewers.get(&pr_number).map(|a| a.reviewer.clone()));
        let placeholder = ps
            .active_span_for_pr(pr_number)
            .or_else(|| ps.most_recent_span_for_pr(pr_number))
            .and_then(|s| ps.task_placeholder_comment_id.get(&s.task_id).copied())
            .or_else(|| {
                ps.github
                    .pr_reviewers
                    .get(&pr_number)
                    .and_then(|a| a.placeholder_comment_id)
            });
        match reviewer_name_val {
            Some(name) => (name, placeholder),
            None => {
                return Response::error(
                    id,
                    RpcError::new(
                        -32603,
                        format!("No reviewer assignment found for PR #{}", pr_number),
                    ),
                );
            }
        }
```

- [ ] **Step 4: Migrate `rpc_coworker.rs:1133` — active assignments for coworker list**

Replace `ps.github.active_assignments()` with span-based lookup:

```rust
            let span_assignments = ps.active_reviewer_assignments();
            let assignments: HashMap<u64, (String, DateTime<Utc>)> = if !span_assignments.is_empty()
            {
                span_assignments
            } else {
                ps.github
                    .active_assignments()
                    .into_iter()
                    .map(|(pr, a)| (pr, (a.reviewer.clone(), a.assigned_at)))
                    .collect()
            };
```

Then adjust the downstream code that accesses assignment fields to use the tuple `(reviewer_name, assigned_at)` instead of `PrReviewerAssignment`.

- [ ] **Step 5: Migrate `rpc_status.rs:57-63` — reviewer map for status display**

Replace with:
```rust
        let rev_map: std::collections::HashMap<String, u64> = {
            let span_map = ps.all_reviewer_pr_assignments();
            if !span_map.is_empty() {
                span_map
            } else {
                ps.github
                    .active_assignments()
                    .iter()
                    .map(|(pr_number, assignment)| (assignment.reviewer.clone(), *pr_number))
                    .collect()
            }
        };
```

- [ ] **Step 3: Run `cargo test`**

- [ ] **Step 4: Commit**

```bash
git add src/daemon/rpc_prs.rs src/daemon/rpc_coworker.rs src/daemon/rpc_status.rs
git commit -m "feat: migrate RPC handlers to prefer task_session_spans"
```

---

### Task 15: Migrate effects.rs lookup_existing_placeholder and post_pr_comment

**Files:**
- Modify: `src/daemon/effects.rs:3903-3919` (`lookup_existing_placeholder`)
- Modify: `src/daemon/effects.rs:4084-4094` (`post_pr_comment` placeholder write)

- [ ] **Step 1: Update `lookup_existing_placeholder` to prefer spans**

In the Tier 1 lookup block, add span-based lookup before the existing `task_reviewer_metadata` lookup:

```rust
        let id = {
            // Prefer task_placeholder_comment_id via span's task_id
            let span_placeholder = ps
                .active_span_for_pr(pr_number)
                .or_else(|| ps.most_recent_span_for_pr(pr_number))
                .and_then(|s| ps.task_placeholder_comment_id.get(&s.task_id).copied());

            span_placeholder
                .or_else(|| {
                    super::state::task_reviewer_metadata_for_pr(&ps, pr_number)
                        .and_then(|m| m.placeholder_comment_id)
                })
                .or_else(|| {
                    ps.github
                        .pr_reviewers
                        .get(&pr_number)
                        .and_then(|a| a.placeholder_comment_id)
                })
        };
```

- [ ] **Step 2: Update `post_pr_comment` to write placeholder via span's task_id**

After the existing `task_reviewer_metadata` write (line 4090-4093), add:

```rust
            // Also write to task_placeholder_comment_id via span's task_id.
            if let Some(span) = ps.active_span_for_pr(pr_number)
                .or_else(|| ps.most_recent_span_for_pr(pr_number))
            {
                ps.task_placeholder_comment_id
                    .insert(span.task_id.clone(), comment_id);
            }
```

- [ ] **Step 3: Run `cargo test`**

- [ ] **Step 4: Commit**

```bash
git add src/daemon/effects.rs
git commit -m "feat: migrate placeholder comment lookup/write to prefer task_session_spans"
```

---

## Chunk 5: Remove legacy structures

### Task 16: Remove pr_reviewers writes from effects.rs

**Files:**
- Modify: `src/daemon/effects.rs:1877-1913`

- [ ] **Step 1: Remove `assign_reviewer` calls and `pr_reviewers` writes from `AssignReviewer` handler**

Replace the entire `AssignReviewer` handler body with:

```rust
            Effect::AssignReviewer {
                pr_number,
                reviewer_name,
                source: _,
                restart_count,
                reviewer_session_id,
                task_id,
            } => {
                let mut ps = state.persistent_state.lock().await;
                // Write to task_session_spans (temporal session history).
                if let Some(ref tid) = task_id {
                    let agent_type = ps
                        .task_agent_type
                        .get(tid)
                        .cloned()
                        .unwrap_or_else(|| "dev".to_string());
                    ps.open_session_span(
                        tid.clone(),
                        reviewer_name.clone(),
                        agent_type,
                        reviewer_session_id.clone(),
                        pr_number,
                    );
                    // Update task_restart_count
                    if restart_count > 0 {
                        ps.task_restart_count.insert(tid.clone(), restart_count);
                    }
                }
                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                    warn!("Failed to save daemon-state.json: {}", e);
                }
            }
```

- [ ] **Step 2: Update `RemoveReviewerAssignment` handler**

Replace:
```rust
            Effect::RemoveReviewerAssignment { pr_number } => {
                let mut ps = state.persistent_state.lock().await;
                if let Some(assignment) = ps.github.remove_assignment(pr_number) {
                    ...
                }
            }
```
With:
```rust
            Effect::RemoveReviewerAssignment { pr_number } => {
                let mut ps = state.persistent_state.lock().await;
                if ps.close_session_span_for_pr(pr_number) {
                    debug!(
                        "Closed session span for PR #{}",
                        pr_number,
                    );
                } else {
                    debug!("No open session span to close for PR #{}", pr_number);
                }
                // Also remove from legacy pr_reviewers during transition
                ps.github.remove_assignment(pr_number);
                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                    warn!(
                        "Failed to save daemon-state.json after removing assignment: {}",
                        e
                    );
                }
            }
```

- [ ] **Step 3: Remove `task_reviewer_metadata` writes from `post_pr_comment`**

Remove lines 4090-4093 (the `task_reviewer_metadata.values_mut()` loop) and line 4086-4088 (the `pr_reviewers.get_mut` write).

- [ ] **Step 4: Remove `task_reviewer_metadata` writes from `AssignReviewer`**

Already done in Step 1.

- [ ] **Step 5: Run `cargo test`**

Run: `cargo test`
Expected: Some tests may fail — proceed to fix in Task 17.

- [ ] **Step 6: Commit (if tests pass)**

```bash
git add src/daemon/effects.rs
git commit -m "refactor: remove legacy pr_reviewers/task_reviewer_metadata writes from effects.rs"
```

---

### Task 17: Remove legacy fallbacks from all consumer sites

**Files:**
- All files modified in Tasks 8-15 — remove the legacy fallback paths

- [ ] **Step 1: Remove `pr_reviewers` fallbacks from snapshot.rs**

Replace the "if spans empty, fall back to legacy" pattern with spans-only.

- [ ] **Step 2: Remove `pr_reviewers` fallbacks from pr.rs, chat.rs, mod.rs, web.rs, rpc_*.rs, effects.rs**

For each file, remove the `|| ps.github.pr_reviewers...` and `|| ps.github.is_assigned(...)` fallback clauses.

- [ ] **Step 3: Remove `pr.rs` backfill block (lines 598-614)**

The entire `task_reviewer_metadata` backfill from `pr_reviewers` is no longer needed. Keep only the span backfill added in Task 7.

- [ ] **Step 4: Remove `clear_reviewer_assignment` legacy code**

In `state.rs:586`, replace the method to only use spans:

```rust
    pub fn clear_reviewer_assignment(&mut self, reviewer_name: &str, repo: &str) -> bool {
        if let Some(pr_number) = self.close_session_span_by_name(reviewer_name) {
            tracing::info!(
                "Cleared reviewer span for {} (was reviewing PR #{})",
                reviewer_name,
                pr_number,
            );
            if let Err(e) = self.save_for_repo(repo) {
                tracing::warn!(
                    "Failed to save persistent state after clearing reviewer span: {}",
                    e
                );
            }
            true
        } else {
            false
        }
    }
```

- [ ] **Step 5: Run `cargo test`**

Expect failures in test files that construct `PrReviewerAssignment` directly. Fix those in Task 18.

- [ ] **Step 6: Commit (if tests pass)**

```bash
git add -A
git commit -m "refactor: remove all legacy pr_reviewers fallbacks from consumer sites"
```

---

### Task 18: Remove `PrReviewerAssignment`, `pr_reviewers`, and `TaskReviewerMetadata`

**Files:**
- Modify: `src/github_state.rs` — remove `PrReviewerAssignment`, `pr_reviewers`, all related methods
- Modify: `src/daemon/state.rs` — remove `TaskReviewerMetadata`, `task_reviewer_metadata`, `task_reviewer_metadata_for_pr()`
- Modify: `src/github_state_tests.rs` — remove/rewrite tests
- Modify: `src/daemon/state_tests.rs` — remove/rewrite tests
- Modify: `src/daemon/effects_tests.rs` — update tests
- Modify: `src/daemon/snapshot_tests.rs` — update tests
- Modify: `src/daemon/rpc_prs_tests.rs` — update tests
- Modify: `src/daemon/dispatch_tests.rs` — update if needed

- [ ] **Step 1: Remove `PrReviewerAssignment` struct and `AssignmentSource` enum from `github_state.rs`**

Remove the struct definition (lines 102-134), the `AssignmentSource` enum, the `default_assignment_source` function, and all methods on `GitHubState` that access `pr_reviewers`:
- `assign_reviewer`, `assign_reviewer_with_event_id`, `assign_reviewer_with_restart_count`
- `get_reviewer`, `is_assigned`
- `remove_assignment`, `remove_assignment_by_reviewer`
- `assigned_reviewers`, `pr_for_reviewer`, `reviewer_has_recent_assignment`
- `cleanup_expired_assignments`, `cleanup_expired_preserving`
- `backfill_reviewer_session_ids`
- `active_count`, `active_reviewers`, `active_assignments`
- `cleanup_closed_prs` (the pr_reviewers portion)

Remove the `pr_reviewers` field from the `GitHubState` struct.

Keep `AssignmentSource` if it's used elsewhere (check with grep). If only used by `pr_reviewers`, remove it.

- [ ] **Step 2: Remove `TaskReviewerMetadata` struct and related fields from `state.rs`**

Remove:
- `TaskReviewerMetadata` struct (lines 52-67)
- `task_reviewer_metadata` field from `DaemonPersistentState`
- `task_reviewer_metadata_for_pr()` free function (lines 697-713)
- References in `apply_gc` (line 468)
- References in `migrate_from_legacy` (line 528)

- [ ] **Step 3: Remove `AssignReviewer` effect's `source` field and `AssignmentSource` enum**

In `effects.rs:381-395`, remove the `source` field from the `AssignReviewer` variant. Update all call sites that construct `AssignReviewer` effects:
- `src/daemon/dispatch.rs:2195` — remove `source: AssignmentSource::PollingFallback`
- `src/daemon/health.rs:1165` — remove `source: AssignmentSource::Manual`

Then remove `AssignmentSource` enum and `default_assignment_source()` from `github_state.rs` (they become dead code after `PrReviewerAssignment` is removed). Clippy `-D warnings` will catch this.

- [ ] **Step 3b: Update stale comments**

- `dispatch_tests.rs:5797-5799` — Update comment "pr_reviewers entry" → "task_session_spans"
- `health.rs:993` — Update comment "uses `PrReviewerAssignment`" → "uses `task_session_spans`"

- [ ] **Step 4: Update all test files**

For each test file with `pr_reviewers` or `PrReviewerAssignment` references:
- `github_state_tests.rs` (51 refs) — Remove tests that only test `pr_reviewers` methods. Rewrite tests that test general reviewer behavior to use `task_session_spans`.
- `state_tests.rs` (62 refs) — Remove `TaskReviewerMetadata` tests, update any that reference `pr_reviewers`.
- `effects_tests.rs` (18 refs) — Update assertions from `ps.github.pr_reviewers.get(...)` to `ps.active_span_for_pr(...)` or `ps.pr_is_assigned(...)`.
- `snapshot_tests.rs` (48 refs) — Update tests that populate `github.pr_reviewers` to use `ps.open_session_span(...)` instead.
- `rpc_prs_tests.rs` (2 refs) — Update reviewer construction.
- `dispatch_tests.rs` (2 refs) — Update if needed.

- [ ] **Step 5: Run full test suite and clippy**

Run: `cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: All pass

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: remove PrReviewerAssignment, pr_reviewers, and TaskReviewerMetadata

Completes the migration to task_session_spans as the single source of
truth for reviewer assignment tracking. Part of !2320."
```

---

## Chunk 6: Cleanup and finalization

### Task 19: Remove unused functions and clean up

**Files:**
- Modify: `src/daemon/snapshot.rs` — remove `build_reviewer_pr_assignments`, `compute_active_reviewers_with_health`
- Modify: `src/daemon/effects.rs` — clean up `lookup_existing_placeholder` to only use spans + `task_placeholder_comment_id`
- Modify: `src/daemon/pr.rs` — remove the now-unnecessary `backfill_reviewer_session_ids` call to `GitHubState`

- [ ] **Step 1: Remove `build_reviewer_pr_assignments` function from snapshot.rs**

Remove the function definition (lines 1678-1697) and its doc comment. Replace all calls with `ps.all_reviewer_pr_assignments()`.

- [ ] **Step 2: Remove `compute_active_reviewers_with_health` function from snapshot.rs**

Remove the function definition (lines 1646-1657). The inline span-based logic from Task 8 replaces it.

- [ ] **Step 3: Simplify `lookup_existing_placeholder` in effects.rs**

Remove legacy `task_reviewer_metadata` and `pr_reviewers` fallbacks. Only use:
```rust
    let ps = state.persistent_state.lock().await;
    let id = ps
        .active_span_for_pr(pr_number)
        .or_else(|| ps.most_recent_span_for_pr(pr_number))
        .and_then(|s| ps.task_placeholder_comment_id.get(&s.task_id).copied());
    if let Some(id) = id {
        return Some(id);
    }
```

- [ ] **Step 4: Run full test suite + clippy**

Run: `cargo test && cargo clippy --all-targets --all-features -- -D warnings`

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: remove unused legacy reviewer functions and simplify lookups"
```

---

### Task 20: Run coverage and final checks

**Files:** None (verification only)

- [ ] **Step 1: Run `cargo fmt`**

Run: `cargo fmt --all`

- [ ] **Step 2: Run full test suite**

Run: `cargo test`

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`

- [ ] **Step 4: Run coverage diff**

Run: `./scripts/coverage-diff.sh`
Review the summary for uncovered lines in changed files.

- [ ] **Step 5: Final commit if any formatting changes**

```bash
git add -A
git commit -m "style: format code"
```
