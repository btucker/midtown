# Remove Session Indirection — Design Spec

## Problem

With unique agent names per task and `SessionRecord` as the primary session model, the codebase has accumulated redundant data structures:

1. **`TaskSessionSpan`** — temporal session tracking, being removed in a separate branch
2. **`NamePool`** — LRU name allocation for recycling names across sessions
3. **Reverse maps** (`name_to_session`, `session_to_name`, `task_to_session`) — in-memory caches of data already in `SessionRecord`
4. **Task metadata maps** on `DaemonPersistentState` — `task_pr_number`, `task_placeholder_comment_id`, `task_restart_count`, `task_agent_type`, `task_channel`, `task_model`, `task_plan`, `task_execution_skill`, `task_thread_id`, `task_message_id`, `task_parent` — all duplicating fields on `Task` in `TaskStore`
5. **`WorldSnapshot`** — a 70+ field god struct that copies data from `DaemonPersistentState`, `Task`, and ephemeral `DaemonState` caches into a flat bag for "pure" decision functions. The purity is already violated (many functions also take `&DaemonState`). Originally created for test fixtures, it grew into a production data pipeline.
6. **`SnapshotReviewerState`** — reviewer-specific grouping that implies reviewers are a special subsystem, when they're just sessions with `agent_type == "midtown-code-reviewer"`

## Design

### Single source of truth

Two authoritative records:

- **`SessionRecord`** (in `DaemonPersistentState.sessions`) — session identity, lifecycle, task binding
- **`Task`** (in `TaskStore`, file per task) — task metadata, agent assignment, PR, channel, model, plan

Decision functions take these directly instead of going through `WorldSnapshot`.

### Moving ephemeral state onto `DaemonPersistentState`

Data that decision functions need but doesn't live on `SessionRecord` or `Task`:

| Data | Current location | Destination | Serde |
|------|-----------------|-------------|-------|
| Process health | `DaemonState.headless_health` | `DaemonPersistentState.process_health` | `#[serde(skip)]` |
| Cooldown pre-evaluations | Computed in `collect_world_snapshot` | `DaemonPersistentState.tick_cooldowns` | `#[serde(skip)]` |
| Open PR data | `DaemonState.pr_poll_data` | `DaemonPersistentState.open_prs_cache` | `#[serde(skip)]` |
| PR task index | Computed in `collect_world_snapshot` | `DaemonPersistentState.pr_task_index` | `#[serde(skip)]` |
| Config constants | `DaemonState` fields | `DaemonPersistentState.daemon_config` | `#[serde(skip)]` |
| Coworker start/stop times | `DaemonState` caches | `DaemonPersistentState.coworker_times` | `#[serde(skip)]` |
| Attached coworkers | `DaemonState.attached_coworkers` | `DaemonPersistentState.attached_coworkers` | `#[serde(skip)]` |

Using `#[serde(skip)]` means these fields are not persisted to `daemon-state.json` but are available to any code holding a `&DaemonPersistentState` reference.

### Decision function signatures

Before:
```rust
fn check_and_restart_dead_reviewers(snap: &WorldSnapshot) -> Vec<Effect>
```

After:
```rust
fn check_and_restart_dead_reviewers(ps: &DaemonPersistentState, tasks: &[Task]) -> Vec<Effect>
```

The `tasks` parameter is loaded once per tick from `TaskStore` (already happens today in `collect_world_snapshot`).

### What replaces `collect_world_snapshot`

A much smaller `prepare_tick` function that:
1. Loads `tasks` from `TaskStore` (one disk read, already happening)
2. Populates `#[serde(skip)]` fields on `DaemonPersistentState`:
   - Copies process health, cooldown state, PR cache, attached coworkers from `DaemonState`
   - Pre-computes derived sets (busy coworkers, PR-protected tasks, active session names)
   - Builds the `PrTaskIndex`
3. Returns `Vec<Task>` (the only thing not on `DaemonPersistentState`)

### What gets deleted

**Structs:**
- `WorldSnapshot`
- `SnapshotCoworkerState`
- `SnapshotPrState`
- `SnapshotReviewerState`
- `SnapshotHealthState`
- `TaskSessionSpan` (separate branch)
- `NamePool`

**Files:**
- `src/name_pool.rs` + `src/name_pool_tests.rs`
- `src/daemon/snapshot.rs` is gutted (keep `ProcessHealth`, `PrTaskIndex`, helper functions; remove the 900-line `collect_world_snapshot` and all nested structs)

**Fields on `DaemonPersistentState`:**
- All `task_*` HashMaps (channel, model, plan, execution_skill, thread_id, message_id, parent, agent_type, pr_number, placeholder_comment_id, restart_count)

**Fields on `DaemonState`:**
- `name_pool`, `name_to_session`, `session_to_name`, `task_to_session`

**Effects:**
- `ReleaseName`
- `CreateTaskSessionSpan`, `CloseTaskSessionSpan` (separate branch)

### Task metadata HashMap removal

The 11 `task_*` HashMaps on `DaemonPersistentState` duplicate fields already on `Task`:

| HashMap | Task field |
|---------|-----------|
| `task_channel` | `Task.channel` |
| `task_model` | `Task.model` |
| `task_plan` | `Task.plan` |
| `task_execution_skill` | no direct field yet — add `Task.execution_skill` |
| `task_thread_id` | `Task.thread_id` |
| `task_message_id` | `Task.message_id` |
| `task_parent` | `Task.parent` |
| `task_agent_type` | `Task.agent_type` |
| `task_pr_number` | `Task.pr` |
| `task_placeholder_comment_id` | add `Task.placeholder_comment_id` |
| `task_restart_count` | add `Task.restart_count` |

Decision functions that need task metadata look it up from `&[Task]` directly:
```rust
fn task_by_id<'a>(tasks: &'a [Task], id: &str) -> Option<&'a Task> {
    tasks.iter().find(|t| t.id == id)
}
```

### Serialization for debugging

The user wants to be able to serialize daemon state for debugging. This is served by serializing `DaemonPersistentState` directly (it's already serialized to `daemon-state.json`). The `#[serde(skip)]` ephemeral fields won't appear in the JSON, but they're also not useful for post-hoc debugging — they're tick-scoped caches.

For debug snapshots that include ephemeral state, a lightweight `DaemonDebugDump` struct can be constructed on demand (e.g., for the `snapshot` RPC command used by headless sessions), rather than being the primary data path for every tick.

### Execution order

This is a large refactor. Ordering matters for incremental compilation:

1. **Add fields to `Task`** (`placeholder_comment_id`, `restart_count`, `execution_skill`)
2. **Add `#[serde(skip)]` fields to `DaemonPersistentState`** for ephemeral data
3. **Add `prepare_tick` function** that populates skip fields + returns `Vec<Task>`
4. **Migrate decision functions file by file** — change signatures from `&WorldSnapshot` to `(&DaemonPersistentState, &[Task])`, updating field access. Each file can be migrated independently.
5. **Remove `WorldSnapshot` and nested structs** once all consumers are migrated
6. **Remove task metadata HashMaps** from `DaemonPersistentState`
7. **Remove `NamePool`, reverse maps, `ReleaseName`**
8. **Delete dead code and update docs**

Steps 4-5 are the bulk of the work — ~87 function signatures across `dispatch.rs`, `pr.rs`, `health.rs`, `events.rs`, `rules.rs`.
