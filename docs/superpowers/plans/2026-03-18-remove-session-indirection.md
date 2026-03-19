# Remove Session Indirection — Eliminate WorldSnapshot

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `WorldSnapshot` (70+ field god struct) with direct access to `DaemonPersistentState` + `&[Task]`. Remove `NamePool`, reverse maps, task metadata HashMaps, `SnapshotReviewerState`, and `TaskSessionSpan` infrastructure — all redundant now that agent names are unique per task and `SessionRecord` is the single source of truth.

**Architecture:** Decision functions change from `fn foo(snap: &WorldSnapshot)` to `fn foo(ps: &DaemonPersistentState, tasks: &[Task])`. Ephemeral per-tick data (process health, cooldowns, PR cache) moves onto `DaemonPersistentState` with `#[serde(skip)]`. A small `prepare_tick()` function populates these fields once per tick, replacing the 900-line `collect_world_snapshot()`. Task metadata lives on the `Task` struct (file per task), not in separate HashMaps.

**Tech Stack:** Rust, serde, chrono

**Spec:** `docs/superpowers/specs/2026-03-18-remove-session-indirection-design.md`

**Dependency:** TaskSessionSpan removal must land first (separate branch `unified-agent-sessions`).

---

## File Structure

### Files to delete
- `src/name_pool.rs` — `NamePool` struct
- `src/name_pool_tests.rs` — `NamePool` tests

### Files heavily modified
| File | Changes |
|------|---------|
| `src/daemon/snapshot.rs` | Delete `WorldSnapshot`, all 4 nested structs, `collect_world_snapshot()`. Keep `ProcessHealth`, `PrTaskIndex`, `CachedHealthSets`, helper fns. |
| `src/daemon/state.rs` | Add `#[serde(skip)]` ephemeral fields. Remove `TaskSessionSpan`, task metadata HashMaps, span helpers. Add `prepare_tick()` and lookup helpers. |
| `src/daemon/dispatch.rs` | Change ~15 functions from `&WorldSnapshot` to `(&DaemonPersistentState, &[Task])` |
| `src/daemon/pr.rs` | Change ~28 functions from `&WorldSnapshot` to `(&DaemonPersistentState, &[Task])` |
| `src/daemon/health.rs` | Change ~8 functions from `&WorldSnapshot` to `(&DaemonPersistentState, &[Task])` |
| `src/daemon/events.rs` | Change `evaluate_tick()` and ~7 dispatch calls |
| `src/daemon/effects.rs` | Remove `ReleaseName`, `CreateTaskSessionSpan`, `CloseTaskSessionSpan` effects |
| `src/daemon/mod.rs` | Remove `name_pool`, reverse maps. Update `run_tick()` to call `prepare_tick()`. |
| `src/task_store.rs` | Add `placeholder_comment_id`, `restart_count`, `execution_skill` to `Task`. Add `agent_type` to `TaskIndexEntry`. |
| `src/rules.rs` | Update 2 functions that access snap fields |
| `src/daemon/rpc_session.rs` | Replace ~15 reverse map reads with `SessionRecord` queries |
| `src/daemon/rpc_channel.rs` | Replace reverse map reads |
| `src/daemon/rpc_coworker.rs` | Replace reverse map reads, update snapshot call site |
| `src/daemon/rpc_prs.rs` | Update snapshot call site |
| `src/daemon/chat.rs` | Replace `name_to_session` with `SessionRecord` query |

---

## Chunk 1: Foundation — extend `Task` and `DaemonPersistentState`

Additive only. Nothing breaks.

### Task 1: Add missing fields to `Task`

**Files:** `src/task_store.rs`

- [ ] **Step 1: Add fields to `Task` struct (after `plan`)**

```rust
    /// GitHub comment ID for "Review in progress" placeholder (reviewer tasks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder_comment_id: Option<u64>,
    /// Number of times this task's session has been restarted.
    #[serde(default)]
    pub restart_count: u32,
    /// Execution skill for plan-driven execution (e.g., "subagent-driven-development").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_skill: Option<String>,
```

- [ ] **Step 2: Add `agent_type` to `TaskIndexEntry`**

```rust
pub struct TaskIndexEntry {
    pub status: TaskStatus,
    pub parent: Option<String>,
    pub agent_name: String,
    pub agent_type: String,  // NEW
}
```

Update `build_index()` and `update_task_index()` to populate it.

- [ ] **Step 3: Run `cargo test`, commit**

### Task 2: Add `#[serde(skip)]` ephemeral fields to `DaemonPersistentState`

**Files:** `src/daemon/state.rs`

These fields hold per-tick data that decision functions need. Populated by `prepare_tick()`, not persisted.

- [ ] **Step 1: Add ephemeral fields**

```rust
    // ── Per-tick ephemeral state (populated by prepare_tick, not persisted) ──

    /// Process health for headless coworkers.
    #[serde(skip)]
    pub process_health: HashMap<String, crate::daemon::snapshot::ProcessHealth>,

    /// Cached open PR data from last GitHub poll.
    #[serde(skip)]
    pub open_prs_cache: Vec<serde_json::Value>,

    /// Number of PRs needing review.
    #[serde(skip)]
    pub prs_needing_review: usize,

    /// Merged PR numbers from last poll.
    #[serde(skip)]
    pub merged_pr_numbers: HashSet<u64>,

    /// GitHub API rate limit state.
    #[serde(skip)]
    pub github_rate_limit: crate::github_rate_limit::GitHubRateLimit,

    /// Freshly fetched rate limit (only during RateLimitCheckTick).
    #[serde(skip)]
    pub freshly_fetched_rate_limit: Option<crate::github_rate_limit::GitHubRateLimit>,

    /// PR↔task index built from sessions + GitHub PR titles.
    #[serde(skip)]
    pub pr_task_index: crate::daemon::snapshot::PrTaskIndex,

    /// Pre-evaluated cooldown states.
    #[serde(skip)]
    pub orphan_spawn_cooldown_active: bool,
    #[serde(skip)]
    pub session_dispatch_cooldown_active: bool,
    #[serde(skip)]
    pub spawn_failure_cooldown_names: HashSet<String>,
    #[serde(skip)]
    pub note_staleness_cooldown_channels: HashSet<String>,
    #[serde(skip)]
    pub merge_rebase_nudge_cooldown_names: HashSet<String>,
    #[serde(skip)]
    pub rebase_nudge_processed_prs: HashSet<u64>,
    #[serde(skip)]
    pub rebase_regression_cooldown_names: HashSet<String>,
    #[serde(skip)]
    pub lead_worktree_freshness_cooldown_channels: HashSet<String>,
    #[serde(skip)]
    pub task_nudge_cooldown_ids: HashSet<String>,
    #[serde(skip)]
    pub recently_recovered_session_ids: HashSet<String>,
    #[serde(skip)]
    pub in_flight_task_spawns: HashSet<String>,

    /// Coworker start/stop times.
    #[serde(skip)]
    pub coworker_start_times: HashMap<String, DateTime<Utc>>,
    #[serde(skip)]
    pub coworker_stop_times: HashMap<String, DateTime<Utc>>,

    /// Attached coworkers with attach timestamp.
    #[serde(skip)]
    pub attached_coworkers: HashMap<String, DateTime<Utc>>,

    /// Config constants available to decision functions.
    #[serde(skip)]
    pub dir_key: String,
    #[serde(skip)]
    pub project_name: String,
    #[serde(skip)]
    pub default_channel: String,
    #[serde(skip)]
    pub default_branch: String,
    #[serde(skip)]
    pub repo_owner: Option<String>,
    #[serde(skip)]
    pub max_in_progress_tasks: usize,
    #[serde(skip)]
    pub lead_session_refresh_interval_secs: u64,
    #[serde(skip)]
    pub now_utc: DateTime<Utc>,

    /// Stale channel lead worktrees (behind origin/main).
    #[serde(skip)]
    pub stale_channel_lead_worktrees: HashSet<String>,

    /// Topic/fork sessions: thread_parent_id → session_id.
    #[serde(skip)]
    pub topic_sessions_cache: HashMap<String, String>,

    /// Session profile mapping: coworker name → auth profile email.
    #[serde(skip)]
    pub session_profile_map: HashMap<String, String>,

    /// Pool profiles currently at usage limit.
    #[serde(skip)]
    pub limited_pool_profiles: HashSet<String>,

    /// Channel messages for debugging context.
    #[serde(skip)]
    pub channel_messages: Vec<crate::message::Message>,

    /// Daemon log tail for debugging context.
    #[serde(skip)]
    pub daemon_logs: Vec<String>,

    /// Reviewer escalations already posted.
    #[serde(skip)]
    pub reviewer_escalations_posted: HashSet<u64>,

    /// Orphaned PR lead nudges already sent.
    #[serde(skip)]
    pub orphaned_pr_lead_nudges_sent: HashSet<u64>,

    /// Archived channels.
    #[serde(skip)]
    pub archived_channels: HashSet<String>,

    /// Stale channel notes.
    #[serde(skip)]
    pub stale_channel_notes: HashMap<String, Vec<String>>,
```

Note: Some of these field names will conflict with existing non-skip fields (e.g., `project_name` is not currently on `DaemonPersistentState`). Use distinct names if needed (e.g., `tick_project_name`) or rename at the time of implementation.

- [ ] **Step 2: Run `cargo test`, commit**

### Task 3: Add lookup helpers to `DaemonPersistentState`

**Files:** `src/daemon/state.rs`, `src/daemon/state_tests.rs`

- [ ] **Step 1: Write failing tests for helpers**

- [ ] **Step 2: Implement helpers**

```rust
/// Find a session record by coworker name (exact match).
pub fn session_by_name(&self, name: &str) -> Option<&SessionRecord> {
    self.sessions.values().find(|s| s.name == name)
}

/// Find a mutable session record by coworker name.
pub fn session_by_name_mut(&mut self, name: &str) -> Option<&mut SessionRecord> {
    self.sessions.values_mut().find(|s| s.name == name)
}

/// Find a session record by task ID.
pub fn session_by_task(&self, task_id: &str) -> Option<&SessionRecord> {
    self.sessions.values().find(|s| s.task_id.as_deref() == Some(task_id))
}

/// Find an active reviewer session for a PR.
pub fn active_reviewer_for_pr(&self, pr_number: u64) -> Option<&SessionRecord> {
    self.sessions.values().find(|s|
        s.agent_type == "midtown-code-reviewer"
        && s.is_running
        && s.pr_number == Some(pr_number)
    )
}

/// All running reviewer sessions.
pub fn running_reviewer_sessions(&self) -> Vec<&SessionRecord> {
    self.sessions.values()
        .filter(|s| s.agent_type == "midtown-code-reviewer" && s.is_running)
        .collect()
}

/// Name → task assignments derived from sessions.
pub fn name_task_assignments(&self) -> HashMap<String, String> {
    self.sessions.values()
        .filter(|s| !s.name.is_empty())
        .filter_map(|s| s.task_id.as_ref().map(|tid| (s.name.to_lowercase(), tid.clone())))
        .collect()
}

/// Busy coworker names (have an in-progress task).
pub fn busy_coworker_names(&self, in_progress_task_ids: &HashSet<&str>) -> HashSet<String> {
    self.sessions.values()
        .filter(|s| s.task_id.as_deref().is_some_and(|tid| in_progress_task_ids.contains(tid)))
        .filter(|s| !s.name.is_empty())
        .map(|s| s.name.to_lowercase())
        .collect()
}
```

- [ ] **Step 3: Run tests, commit**

### Task 4: Write `prepare_tick()` function

**Files:** `src/daemon/state.rs` (or new `src/daemon/tick.rs`)

This function populates the `#[serde(skip)]` fields on `DaemonPersistentState` from `DaemonState`'s caches. It replaces `collect_world_snapshot()`.

- [ ] **Step 1: Implement `prepare_tick`**

The function takes `&DaemonState`, locks `persistent_state`, and copies ephemeral data from the various `DaemonState` Mutex/RwLock fields into the `#[serde(skip)]` fields. Also loads `Vec<Task>` from TaskStore.

Signature:
```rust
pub(crate) async fn prepare_tick(state: &DaemonState) -> Vec<crate::task_store::Task> {
    // 1. Load tasks from TaskStore
    let tasks = state.task_store.load_all();

    // 2. Lock persistent_state and populate skip fields
    let mut ps = state.persistent_state.lock().await;

    // Copy process health
    ps.process_health = state.headless_health.read().unwrap().clone();

    // Copy PR poll data
    { let cache = state.pr_poll_data.read().unwrap();
      ps.prs_needing_review = cache.prs_needing_review;
      ps.open_prs_cache = cache.open_prs_data.clone(); }

    // Copy cooldowns
    ps.orphan_spawn_cooldown_active = state.cooldowns.lock().unwrap()
        .is_active("orphan_spawn_global");
    // ... (same pattern for all cooldown fields)

    // Copy config
    ps.dir_key = state.paths.dir_key().to_string();
    ps.project_name = state.project_name.clone();
    // ... etc

    // Build PR task index
    let session_task_to_pr = task_to_pr_map_from_sessions(&ps.sessions);
    let pr_to_task = pr_to_task_map_from_sessions(&ps.sessions);
    // ... build github_open_pr_task_ids from open_prs_cache
    ps.pr_task_index = PrTaskIndex::new(session_task_to_pr, github_open_pr_task_ids, pr_to_task);

    ps.now_utc = Utc::now();

    tasks
}
```

- [ ] **Step 2: Run `cargo test`, commit**

---

## Chunk 2: Migrate decision functions (file by file)

This is the bulk of the work. Each task migrates one file's functions from `&WorldSnapshot` to `(&DaemonPersistentState, &[Task])`. During migration, both `WorldSnapshot` and the new signatures coexist — unmigrated callers still use `collect_world_snapshot()`.

**Migration pattern for each function:**
1. Change signature: `snap: &WorldSnapshot` → `ps: &DaemonPersistentState, tasks: &[Task]`
2. Replace field accesses:
   - `snap.sessions` → `ps.sessions`
   - `snap.task_channel.get(id)` → `tasks.iter().find(|t| t.id == id).and_then(|t| t.channel.as_deref())`
   - `snap.coworkers.active_names` → derive from `ps.sessions` or pass as param
   - `snap.reviewer.*` → use `ps.running_reviewer_sessions()` etc.
   - `snap.health.*` → `ps.process_health` etc.
   - `snap.find_session_for_task(id)` → `ps.session_by_task(id)`
   - Cooldown booleans → `ps.orphan_spawn_cooldown_active` etc.
3. Update tests to construct `DaemonPersistentState` + `Vec<Task>` instead of `WorldSnapshot`

### Task 5: Migrate `health.rs`

**Files:** `src/daemon/health.rs`, `src/daemon/health_tests.rs`

Functions to migrate (~8):
- `check_and_restart_dead_reviewers`
- `check_for_usage_limits`
- `maybe_nudge_usage_limit_expiry`
- `check_and_nudge_api_errors`
- `check_and_handle_auth_errors`
- `check_and_restart_tool_name_conflicts`
- `maybe_refresh_lead_session`
- `check_channel_lead_worktree_freshness`
- `check_and_respawn_dead_processes`
- `build_reviewer_respawn_effects`

These are the simplest — they mostly read health state and reviewer assignments.

- [ ] **Step 1: Migrate function signatures**
- [ ] **Step 2: Replace snap field accesses with ps/tasks lookups**
- [ ] **Step 3: Update health_tests.rs** — replace `WorldSnapshot` construction with `DaemonPersistentState` + `Vec<Task>`
- [ ] **Step 4: Update callers in `events.rs`** — `evaluate_tick` still uses `WorldSnapshot` for other files, but passes `(&ps, &tasks)` to health functions
- [ ] **Step 5: Run `cargo test`, commit**

### Task 6: Migrate `rules.rs`

**Files:** `src/rules.rs`

Small — only 2 functions access snap fields.

- [ ] **Step 1: Migrate function signatures**
- [ ] **Step 2: Run `cargo test`, commit**

### Task 7: Migrate `dispatch.rs`

**Files:** `src/daemon/dispatch.rs`, `src/daemon/dispatch_tests.rs`, `src/daemon/dispatch_session_tests.rs`, `src/daemon/dispatch_dev_limit_tests.rs`

Functions to migrate (~15):
- `check_and_recover_orphans` / `check_and_recover_orphans_impl`
- `dispatch_via_sessions` / `dispatch_via_sessions_inner`
- `check_for_duplicate_task_workers`
- `spawn_for_pending_tasks_excluding`
- `dispatch_owned_pending_tasks`
- `dispatch_unowned_pending_tasks`
- `build_spawn_effects`
- `prepare_task_worktree`
- `build_plan_prompt_section`
- `compute_recently_stopped`
- `find_idle_coworker`
- `resolve_grouped_name`
- `build_subject_based_completion_effects`
- `reset_orphaned_tasks`

Heavy consumer of task maps (`task_channel`, `task_model_map`, `task_plan_map`, `task_agent_type_map`, `task_agent_name_map`, `task_worktree_map`, etc.). All replaced by `Task` field lookups.

- [ ] **Step 1: Add `task_by_id` helper** (or method on a `Tasks` newtype)

```rust
fn task_by_id<'a>(tasks: &'a [Task], id: &str) -> Option<&'a Task> {
    tasks.iter().find(|t| t.id == id)
}
```

- [ ] **Step 2: Migrate function signatures**
- [ ] **Step 3: Replace snap field accesses**

Key replacements:
- `snap.task_agent_type_map.get(id)` → `task_by_id(tasks, id).map(|t| t.agent_type.as_str())`
- `snap.task_channel.get(id)` → `task_by_id(tasks, id).and_then(|t| t.channel.as_deref())`
- `snap.task_model_map.get(id)` → `task_by_id(tasks, id).and_then(|t| t.model.as_deref())`
- `snap.task_plan_map.get(id)` → `task_by_id(tasks, id).and_then(|t| t.plan.as_deref())`
- `snap.task_agent_name_map.get(id)` → `task_by_id(tasks, id).map(|t| t.agent_name.as_str())`
- `snap.task_worktree_map.get(id)` → `ps.worktree_registry.get_by_task(id)`
- `snap.all_tasks` → `tasks`
- `snap.pending_tasks_without_owners` → filter `tasks` inline
- `snap.busy_coworkers` → `ps.busy_coworker_names(&in_progress_ids)`
- `snap.find_session_for_task(id)` → `ps.session_by_task(id)`

- [ ] **Step 4: Update dispatch test files** (3 files)
- [ ] **Step 5: Run `cargo test`, commit**

### Task 8: Migrate `pr.rs`

**Files:** `src/daemon/pr.rs`, `src/daemon/pr_tests.rs`

Functions to migrate (~28 — the largest file). Many are internal helpers called by `poll_prs_for_issues`.

- [ ] **Step 1: Migrate top-level entry points**
  - `poll_prs_for_issues`
  - `reconcile_orphaned_prs`
  - `collect_pr_task_link_effects`
  - `collect_merged_pr_cleanup_effects`
  - `collect_merge_rebase_nudge_effects`
  - `check_for_rebase_regressions`

- [ ] **Step 2: Migrate internal helpers** (signatures cascade from entry points)
  - `detect_abandoned_pr_tasks`
  - `update_pr_caches`
  - `resolve_pr_owner`
  - `detect_and_block_external_prs`
  - `process_pr_issue_nudges`
  - `maybe_decide_pr_issue_effects`
  - `decide_and_build_pr_issue_effects`
  - `collect_green_with_feedback_effects`
  - `collect_stuck_condition_effects`
  - `collect_comment_notification_effects`
  - `collect_reviewer_effects` / `collect_reviewer_effects_with_source`
  - `collect_review_complete_effects`
  - `collect_stale_check_effects` / `collect_stale_check_effects_with_time`
  - `handle_pr_comment_nudge`
  - `handle_webhook_review_state_change`
  - `handle_webhook_ci_failure`

- [ ] **Step 3: Replace snap field accesses** — similar pattern to dispatch.rs
- [ ] **Step 4: Update pr_tests.rs**
- [ ] **Step 5: Run `cargo test`, commit**

### Task 9: Migrate `events.rs` and remaining callers

**Files:** `src/daemon/events.rs`, `src/daemon/mod.rs`, `src/daemon/rpc.rs`, `src/daemon/rpc_prs.rs`, `src/daemon/rpc_coworker.rs`

- [ ] **Step 1: Update `evaluate_tick` signature**

```rust
pub async fn evaluate_tick(
    event: &DaemonEvent,
    ps: &DaemonPersistentState,
    tasks: &[Task],
    state: &DaemonState,
) -> Vec<Effect>
```

- [ ] **Step 2: Update `run_tick` in `mod.rs`**

Replace:
```rust
let snap = collect_world_snapshot(state).await;
let effects = evaluate_tick(&event, &snap, state).await;
```

With:
```rust
let tasks = prepare_tick(state).await;
let ps = state.persistent_state.lock().await;
let effects = evaluate_tick(&event, &ps, &tasks, state).await;
```

- [ ] **Step 3: Update RPC handlers that call `collect_world_snapshot`**

`handle_pr_review` in `rpc_prs.rs`, `handle_coworker_report_state` in `rpc_coworker.rs`, `dispatch_request` in `rpc.rs`, `handle_snapshot` in `rpc_headless.rs` — these call `collect_world_snapshot` for ad-hoc snapshot access. Replace with `prepare_tick` + direct `ps` access.

For `handle_snapshot` (headless debug endpoint): construct a lightweight debug dump struct instead.

- [ ] **Step 4: Run `cargo test`, commit**

---

## Chunk 3: Delete `WorldSnapshot` and `collect_world_snapshot`

All consumers now use `(&DaemonPersistentState, &[Task])`.

### Task 10: Remove WorldSnapshot infrastructure

**Files:** `src/daemon/snapshot.rs`

- [ ] **Step 1: Delete structs**

Remove:
- `WorldSnapshot`
- `SnapshotCoworkerState`
- `SnapshotPrState`
- `SnapshotReviewerState`
- `SnapshotHealthState`
- `collect_world_snapshot()`
- `compute_active_reviewers_from_spans()`
- `build_reviewer_pr_assignments_from_spans()`
- `WorldSnapshot::default()` (test helper)
- All `impl WorldSnapshot` methods

Keep:
- `ProcessHealth` + `Default` impl
- `PrTaskIndex` + methods
- `CachedHealthSets` + `compute_health_sets()`
- `read_daemon_log_tail()`
- `u64_key_map` module (if still needed)

- [ ] **Step 2: Delete test fixtures that construct WorldSnapshot**

`src/daemon/snapshot_tests.rs` will need heavy rewriting or deletion.

- [ ] **Step 3: Run `cargo test`, commit**

---

## Chunk 4: Remove task metadata HashMaps from `DaemonPersistentState`

Now that decision functions read from `Task` directly, the legacy HashMaps on persistent state are dead code.

### Task 11: Remove task metadata maps and their writers

**Files:** `src/daemon/state.rs`, `src/daemon/effects.rs`, `src/daemon/rpc_task.rs`, `src/daemon/migration.rs`

- [ ] **Step 1: Redirect all writers to use `TaskStore`**

Find and update all `.insert()` calls for:
- `task_channel`
- `task_model`
- `task_plan`
- `task_execution_skill`
- `task_thread_id`
- `task_message_id`
- `task_parent`
- `task_agent_type`
- `task_pr_number`
- `task_placeholder_comment_id`
- `task_restart_count`

Key write sites:
- `handle_task_create` in `rpc_task.rs`
- `handle_task_handoff` in `rpc_task.rs`
- `CreateTaskSessionSpan` handler in `effects.rs`
- `post_pr_comment` in `effects.rs`
- `spawn_coworker` in `mod.rs`

Each insert becomes a `TaskStore::load` → mutate field → `TaskStore::save`.

- [ ] **Step 2: Remove the 11 HashMap fields from `DaemonPersistentState`**

Also remove from `migrate_from_legacy()` and `apply_gc()`.

- [ ] **Step 3: Update `migration.rs`**

`migrate_tasks_if_needed` reads `task_agent_type` — remove or update.

- [ ] **Step 4: Run `cargo test`, fix compilation errors, commit**

---

## Chunk 5: Remove `NamePool` and reverse maps

### Task 12: Remove `NamePool` and `ReleaseName` effect

**Files:** `src/name_pool.rs`, `src/name_pool_tests.rs`, `src/lib.rs`, `src/daemon/mod.rs`, `src/daemon/effects.rs`, `src/daemon/pr.rs`, `tests/multi_tick_harness.rs`

- [ ] **Step 1: Remove `ReleaseName` variant from `Effect` enum and its handler**
- [ ] **Step 2: Remove all `name_pool.lock()` calls in effects.rs and mod.rs**
- [ ] **Step 3: Remove `name_pool` field from `DaemonState` and initialization**
- [ ] **Step 4: Remove `name_pool.release()` from `cleanup_coworker_state_internal`**
- [ ] **Step 5: Remove `ReleaseName` from `effect_variant_name` and `multi_tick_harness.rs`**
- [ ] **Step 6: Delete `src/name_pool.rs` and `src/name_pool_tests.rs`**
- [ ] **Step 7: Remove `pub mod name_pool` from `src/lib.rs`**
- [ ] **Step 8: Run `cargo test`, fix compilation (delete NamePool tests in effects_tests.rs, mod_tests.rs), commit**

### Task 13: Remove reverse maps

**Files:** `src/daemon/mod.rs`, `src/daemon/effects.rs`, `src/daemon/chat.rs`, `src/daemon/rpc_session.rs`, `src/daemon/rpc_channel.rs`, `src/daemon/rpc_coworker.rs`, `src/daemon/rpc_task.rs`, `src/daemon/rpc_auth.rs`

- [ ] **Step 1: Replace all `name_to_session.lock()` reads**

Replace with `ps.session_by_name(name)` (lock persistent_state).

Key files (from reviewer findings):
- `rpc_session.rs` — ~15 call sites (largest: `create_fork_session`, `handle_session_fork`, `handle_session_attach`, `handle_session_detach`, `handle_session_clear`, `handle_session_resolve`)
- `rpc_channel.rs` — `handle_channel_post`, `try_lazy_fork_respawn`
- `effects.rs` — `SpawnForTask`, `ShutdownSession` handlers
- `chat.rs` — `route_mentions`
- `rpc_coworker.rs` — `handle_coworker_report_state`

- [ ] **Step 2: Replace all `session_to_name.lock()` reads**

Replace with `ps.sessions.get(session_id).map(|s| &s.name)`.

Key files:
- `rpc_session.rs` — same functions as above
- `rpc_channel.rs` — `handle_channel_post`
- `effects.rs` — `send_session_nudge`, `ShutdownSession`

- [ ] **Step 3: Replace all `task_to_session.lock()` reads**

Replace with `ps.session_by_task(task_id)`.

- [ ] **Step 4: Remove all reverse map writes (inserts)**

- [ ] **Step 5: Remove the three fields from `DaemonState`**

```rust
// DELETE these:
pub(crate) name_to_session: std::sync::Mutex<HashMap<String, String>>,
pub(crate) session_to_name: std::sync::Mutex<HashMap<String, String>>,
pub(crate) task_to_session: std::sync::Mutex<HashMap<String, String>>,
```

- [ ] **Step 6: Simplify `cleanup_coworker_state_internal`**

Remove the NamePool release (already gone) and all reverse map cleanup blocks.

- [ ] **Step 7: Remove `restore_task_assignments_from_disk` if it only rebuilds reverse maps**

- [ ] **Step 8: Run `cargo test`, fix compilation, commit**

---

## Chunk 6: Remove `TaskSessionSpan` infrastructure

(If not already done by the dependency branch.)

### Task 14: Remove `TaskSessionSpan` and related effects

**Files:** `src/daemon/state.rs`, `src/daemon/effects.rs`, `src/daemon/pr.rs`, `src/daemon/span_tests.rs`, `tests/multi_tick_harness.rs`

- [ ] **Step 1: Remove `TaskSessionSpan` struct and `task_session_spans` field**
- [ ] **Step 2: Remove all span helper methods** (`active_span_for_task`, `spans_for_task`, `active_reviewer_for_pr`, `pr_has_active_reviewer`, `active_reviewer_spans`, `close_span`, `close_spans_for_session`, `close_spans_for_task`, `create_span`, `clear_reviewer_assignment`)
- [ ] **Step 3: Remove `CreateTaskSessionSpan` and `CloseTaskSessionSpan` effects**
- [ ] **Step 4: Delete `src/daemon/span_tests.rs`** (or the span section of state_tests.rs)
- [ ] **Step 5: Update `multi_tick_harness.rs`**
- [ ] **Step 6: Run `cargo test`, commit**

---

## Chunk 7: Final cleanup

### Task 15: Clean up documentation and stale comments

**Files:** `docs/architecture.md`, `src/daemon/mod.rs`, various

- [ ] **Step 1: Update `docs/architecture.md`**

Remove/rewrite:
- "TaskSessionSpan — Temporal Session Tracking" section
- Any references to `NamePool`, `WorldSnapshot`, reverse maps
- Add section on `SessionRecord` as single source of truth
- Add section on `prepare_tick()` replacing `collect_world_snapshot()`

- [ ] **Step 2: Clean up stale comments across codebase**

Search for: "name reuse", "NamePool", "name pool", "release name", "WorldSnapshot", "collect_world_snapshot", "task_pr_number", "task_agent_type", "task_reviewer_metadata", "task_session_spans", "snap.", "snapshot"

- [ ] **Step 3: Run full test suite**

Run: `cargo test`

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`

- [ ] **Step 5: Commit**

### Task 16: Coverage check

- [ ] **Step 1: Run `./scripts/coverage-diff.sh`**
- [ ] **Step 2: Review uncovered lines, add tests if needed**
- [ ] **Step 3: Commit**
