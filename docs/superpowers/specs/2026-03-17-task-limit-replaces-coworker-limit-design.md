# Replace Max Coworker Limit with Max In-Progress Task Limit

## Motivation

The current `max_coworkers` infrastructure limits concurrent Claude Code processes. This has grown complex with `REVIEW_HEADROOM`, separate `is_at_dev_limit()` / `is_at_coworker_limit()` checks, and special-casing for reviewers vs. dev coworkers. This change:

1. **Cost control** — caps concurrent work (tasks), not concurrent processes.
2. **Simplification** — one limit, one concept. Review tasks are just tasks. No headroom, no dev vs. reviewer distinction.
3. **Elastic coworkers** — processes spin up/down freely. The task limit is the only governor. Process count is implicitly bounded (each in-progress task needs at most one coworker).

## Config & State

### Config changes

- Remove `max_coworkers` from `ProjectConfig` and `DefaultConfig` in `config.rs`.
- Add `max_in_progress_tasks: Option<usize>` in its place.
- Remove `DEFAULT_MAX_COWORKERS` from `constants.rs`, add `DEFAULT_MAX_IN_PROGRESS_TASKS` (default: 8).
- Remove `REVIEW_HEADROOM` constant entirely.
- Env var: `MIDTOWN_MAX_IN_PROGRESS_TASKS` replaces `MIDTOWN_MAX_COWORKERS`.
- CLI config key: `default.max_in_progress_tasks` replaces `default.max_coworkers`.
- If old `max_coworkers` key is present in TOML, emit a deprecation warning at daemon startup. Do not silently map old to new (different semantics).
- Note for users who relied on `REVIEW_HEADROOM`: previously `max_coworkers = 6` allowed up to 8 total processes (6 dev + 2 reviewer overflow). With `max_in_progress_tasks = 6`, all task types share the same 6 slots. Users who want equivalent throughput should set `max_in_progress_tasks` to their old effective ceiling (e.g., 8).

### DaemonState changes

- `max_coworkers: usize` → `max_in_progress_tasks: usize`.
- Remove `is_at_coworker_limit()`, `is_at_dev_limit()`, `has_available_coworker_slot()`.
- Add single `is_at_task_limit(&self) -> bool` that counts in-progress tasks against `max_in_progress_tasks`.
- `DaemonState::is_at_task_limit()` reads from disk (the shared task storage already loaded in memory) for RPC handlers that operate outside the snapshot pipeline (e.g., `rpc_coworker.rs`). It is **not** called from pure decision functions — those use `snap.is_at_task_limit` instead, which is pre-computed during `collect_world_snapshot()` to preserve the no-I/O-in-decision-functions convention.
- Orphaned tasks (in-progress but no running coworker) DO count toward the limit. This is intentional: orphan recovery will either reassign or clear them, and counting them prevents overcommitting while recovery is in progress.

### WorldSnapshot changes

- Remove `is_at_coworker_limit: bool` and `is_at_dev_limit: bool`.
- Add `is_at_task_limit: bool`.

## Dispatch Priority Module

New file: `src/daemon/dispatch_priority.rs`

Pure function:

```rust
pub fn prioritize_pending_tasks(
    pending_tasks: &[TaskSnapshot],
    in_progress_task_ids: &HashSet<String>,
    task_parent_map: &HashMap<String, String>,      // child_id -> parent_id (existing)
    blocked_by_map: &HashMap<String, Vec<String>>,   // task_id -> task_ids it blocks (inverted)
) -> Vec<String>  // task IDs in priority order
```

### Priority tiers (stable sort — FIFO within each tier)

1. **Children of in-progress parents** — `task_parent_map[task_id]` exists AND parent is in `in_progress_task_ids`. A child whose parent is not in-progress falls to tier 3.
2. **Blockers** — task blocks at least one other task (appears as key in `blocked_by_map`).
3. **FIFO** — everything else, ordered by creation time.

### `blocked_by_map` construction

The `Task` struct stores `blocked_by: Vec<String>` (prerequisites). The `blocked_by_map` inverts this: for each task X that appears in some task Y's `blocked_by`, map X → [Y, ...]. This inversion is computed during snapshot collection and added to `WorldSnapshot` as `blocks_map: HashMap<String, Vec<String>>`. Only pending/in-progress tasks participate — completed tasks are excluded.

Key properties:
- Pure function, no I/O — fits the `rules.rs` convention.
- Testable in isolation.
- Called once per tick; dispatch loop iterates the result.

## Dispatch Loop Changes

### `src/daemon/dispatch.rs`

- Replace input list with `dispatch_priority::prioritize_pending_tasks(...)` applied to `snap.pending_tasks_without_owners`.
- Replace dev cap check with task limit check: in-progress task count + spawns queued this tick >= `max_in_progress_tasks`.
- The local limit re-derivation block (lines ~1742–1756: `dev_cap`, `current_coworker_count`, `effective_count >= dev_cap`) must be replaced — it does not use `snap.is_at_dev_limit` and will be missed by a simple grep. Replace with: `snap.in_progress_tasks.len() + spawns_queued_this_tick >= state.max_in_progress_tasks`.
- Remove all `is_at_dev_limit` / `is_at_coworker_limit` / `REVIEW_HEADROOM` references.
- `spawns_queued_this_tick` counter stays (prevents overshooting within a single tick).

### `src/daemon/pr.rs`

- All ~8 call sites checking `is_at_dev_limit` switch to `is_at_task_limit`.
- No reviewer headroom — review tasks compete for slots like any other task.

### Orphan recovery in `dispatch.rs`

- `is_at_dev_limit` guard → `is_at_task_limit`.
- `OrphanRecoveryContext.at_dev_limit` → `at_task_limit`.

### `src/daemon/chat.rs`

- `chat.rs:260` calls `state.is_at_dev_limit()` to gate mention handling — switch to `is_at_task_limit`.

### `src/daemon/rpc_coworker.rs`

- `rpc_coworker.rs:42` calls `state.is_at_dev_limit()` directly on `DaemonState` (not snapshot). `is_at_task_limit()` remains a `DaemonState` method that reads from the shared task storage (already loaded in memory via the snapshot pipeline), so this call site works without changes beyond the rename.
- Dev limit error message references `max_in_progress_tasks`.

### `src/webhook.rs`

- `start_webhook_server` takes `max_coworkers: usize` parameter and stores it in `WebhookState`. Update to `max_in_progress_tasks`.
- Call site in `daemon/mod.rs` startup passes the new config field.

## UI & Status Changes

### CLI status (`response.rs`)

- `max_coworkers: Option<usize>` → `max_in_progress_tasks: Option<usize>`.
- Display numerator changes from active coworker count to in-progress task count. These can differ (idle coworkers, orphaned tasks). Display: "Tasks (3/8 in progress)" style.

### Chat TUI (`chat/ui/board.rs`, `chat/app.rs`)

- `app.max_coworkers` → `app.max_in_progress_tasks`.
- Board header updated accordingly.

### RPC status (`rpc_status.rs`, `rpc_coworker.rs`)

- JSON keys: `max_coworkers` → `max_in_progress_tasks`, drop `max_dev_coworkers`.
- Add `in_progress_task_count` for visibility.

### Web app

- `web-app/src/lib/types.ts`, `api.ts` — update types.
- `web.rs` — `DashboardState.max_coworkers` → `max_in_progress_tasks`.

## Cleanup & Migration

### Removed

- `DEFAULT_MAX_COWORKERS` constant
- `REVIEW_HEADROOM` constant
- `DaemonState::is_at_coworker_limit()`
- `DaemonState::is_at_dev_limit()`
- `DaemonState::has_available_coworker_slot()`

### Test updates

- `dispatch_dev_limit_tests.rs` → renamed to `dispatch_task_limit_tests.rs`, rewritten for task-based limits.
- All snapshot fixtures: `is_at_coworker_limit` / `is_at_dev_limit` → `is_at_task_limit`.
- `mod_tests.rs` — remove dev cap math tests, add task limit tests.
- `config_tests.rs` / CLI config tests — rename key references.
- New: `dispatch_priority_tests.rs` — focused tests for the three priority tiers.

### Docs

- `docs/architecture.md` — update dispatch and limit documentation.
- `docs/configuration.md` — update config key reference.
- `README.md` — update if `max_coworkers` is mentioned.
