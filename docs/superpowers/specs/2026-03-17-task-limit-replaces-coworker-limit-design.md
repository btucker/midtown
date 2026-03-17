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

### DaemonState changes

- `max_coworkers: usize` → `max_in_progress_tasks: usize`.
- Remove `is_at_coworker_limit()`, `is_at_dev_limit()`, `has_available_coworker_slot()`.
- Add single `is_at_task_limit(&self) -> bool` that counts in-progress tasks against `max_in_progress_tasks`.

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

Key properties:
- Pure function, no I/O — fits the `rules.rs` convention.
- Testable in isolation.
- Called once per tick; dispatch loop iterates the result.

## Dispatch Loop Changes

### `src/daemon/dispatch.rs`

- Replace input list with `dispatch_priority::prioritize_pending_tasks(...)` applied to `snap.pending_tasks_without_owners`.
- Replace dev cap check with task limit check: in-progress task count + spawns queued this tick >= `max_in_progress_tasks`.
- Remove all `is_at_dev_limit` / `is_at_coworker_limit` / `REVIEW_HEADROOM` references.
- `spawns_queued_this_tick` counter stays (prevents overshooting within a single tick).

### `src/daemon/pr.rs`

- All ~8 call sites checking `is_at_dev_limit` switch to `is_at_task_limit`.
- No reviewer headroom — review tasks compete for slots like any other task.

### Orphan recovery in `dispatch.rs`

- `is_at_dev_limit` guard → `is_at_task_limit`.
- `OrphanRecoveryContext.at_dev_limit` → `at_task_limit`.

### `src/daemon/rpc_coworker.rs`

- Dev limit error message references `max_in_progress_tasks`.

## UI & Status Changes

### CLI status (`response.rs`)

- `max_coworkers: Option<usize>` → `max_in_progress_tasks: Option<usize>`.
- Display: "Tasks (3/8 in progress)" style.

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

- `dispatch_dev_limit_tests.rs` — rewritten for task-based limits.
- All snapshot fixtures: `is_at_coworker_limit` / `is_at_dev_limit` → `is_at_task_limit`.
- `mod_tests.rs` — remove dev cap math tests, add task limit tests.
- `config_tests.rs` / CLI config tests — rename key references.
- New: `dispatch_priority_tests.rs` — focused tests for the three priority tiers.

### Docs

- `docs/architecture.md` — update dispatch and limit documentation.
- `docs/configuration.md` — update config key reference.
- `README.md` — update if `max_coworkers` is mentioned.
