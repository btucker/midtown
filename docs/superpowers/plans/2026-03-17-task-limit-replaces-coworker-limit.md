# Replace Max Coworker Limit with Max In-Progress Task Limit

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the process-based `max_coworkers` limit with a task-based `max_in_progress_tasks` limit, removing all reviewer headroom special-casing and adding dispatch priority ordering.

**Architecture:** One config value (`max_in_progress_tasks`) gates all task dispatch. A new pure function `prioritize_pending_tasks()` orders pending tasks by: children of in-progress parents > blockers > FIFO. All `REVIEW_HEADROOM`, `is_at_dev_limit`, `is_at_coworker_limit` infrastructure is removed.

**Tech Stack:** Rust, TOML config, Svelte/TypeScript web app

**Important:** Tasks 1–6 form an atomic rename across the codebase. They must all be completed before committing — intermediate states won't compile and pre-commit hooks (`cargo clippy`) will reject partial commits.

---

## Chunk 1: Atomic Rename — Constants, Config, State & All Consumers

### Task 1: Replace constants, config, state, and all consumers in one atomic commit

This task combines all the mechanical renaming into a single compilable commit. Every reference to `max_coworkers`, `REVIEW_HEADROOM`, `is_at_dev_limit`, `is_at_coworker_limit`, `has_available_coworker_slot`, and `DEFAULT_MAX_COWORKERS` must be updated before committing.

**Files (constants):**
- Modify: `src/daemon/constants.rs:10` (DEFAULT_MAX_COWORKERS → DEFAULT_MAX_IN_PROGRESS_TASKS)
- Modify: `src/daemon/constants.rs:161` (delete REVIEW_HEADROOM)

**Files (config):**
- Modify: `src/config.rs:338-394` (ProjectConfig struct, merge, accessor)
- Modify: `src/config.rs:935-944` (template)
- Modify: `src/config.rs:1027-1043` (load_project_config detection)

**Files (DaemonState):**
- Modify: `src/daemon/mod.rs:43` (pub use re-export)
- Modify: `src/daemon/mod.rs:206` (DaemonConfig.max_coworkers field)
- Modify: `src/daemon/mod.rs:288-299` (DaemonConfig::default() env var)
- Modify: `src/daemon/mod.rs:320` (field assignment)
- Modify: `src/daemon/mod.rs:482` (DaemonState.max_coworkers field)
- Modify: `src/daemon/mod.rs:1138-1196` (delete is_at_coworker_limit, is_at_dev_limit, has_available_coworker_slot; add is_at_task_limit)
- Modify: `src/daemon/mod.rs:1308,1424` (DaemonState::new)
- Modify: `src/daemon/mod.rs:3109-3164` (daemon startup)

**Files (WorldSnapshot):**
- Modify: `src/daemon/snapshot.rs:436-438` (fields)
- Modify: `src/daemon/snapshot.rs:~837` (add blocks_map computation)
- Modify: `src/daemon/snapshot.rs:1190-1191` (limit computation)
- Modify: `src/daemon/snapshot.rs:1506-1507` (assignment)
- Modify: `src/daemon/snapshot.rs:1585-1586` (test helper)

**Files (consumers):**
- Modify: `src/webhook.rs:299,344`
- Modify: `src/web.rs:184,1404`
- Modify: `src/daemon/rpc_status.rs:200-201`
- Modify: `src/daemon/rpc_coworker.rs:42-54,1105`
- Modify: `src/daemon/chat.rs:260`
- Modify: `src/daemon/pr.rs` (~8 call sites)
- Modify: `src/rules.rs:746,933,1027`
- Modify: `src/daemon/dispatch.rs:483,966,991,1520,1742-1756`
- Modify: `src/bin/midtown/cli/config.rs:42,276,335,398-399,493-494`
- Modify: `src/bin/midtown/cli/response.rs:31`
- Modify: `src/bin/midtown/cli/chat/app.rs:94,345,705,923,1094,3673-3674,3787,4145`
- Modify: `src/bin/midtown/cli/chat/ui/board.rs:556,1201,1344,1399,1449`

- [ ] **Step 1: Update constants.rs**

In `src/daemon/constants.rs`, replace:

```rust
/// Default maximum number of concurrent coworkers.
pub const DEFAULT_MAX_COWORKERS: usize = 8;
```

with:

```rust
/// Default maximum number of concurrent in-progress tasks.
pub const DEFAULT_MAX_IN_PROGRESS_TASKS: usize = 8;
```

Delete the `REVIEW_HEADROOM` constant and its doc comment (lines 160–161).

- [ ] **Step 2: Update the pub use re-export in mod.rs**

At `src/daemon/mod.rs:43`, update the `pub use constants::` block: replace `DEFAULT_MAX_COWORKERS` with `DEFAULT_MAX_IN_PROGRESS_TASKS`. Remove any re-exports of `REVIEW_HEADROOM` if present (it was `pub(super)` so it shouldn't be re-exported, but verify).

- [ ] **Step 3: Update config.rs**

In `src/config.rs`:
- Rename the `ProjectConfig` field: `max_coworkers: Option<usize>` → `max_in_progress_tasks: Option<usize>` (line 340). Update doc comment.
- Update `merge()`: `max_coworkers` → `max_in_progress_tasks` (line 365)
- Rename accessor: `max_coworkers()` → `max_in_progress_tasks()` (lines 391–394)
- Update template: `max_coworkers = 8` → `max_in_progress_tasks = 8` (lines 943–944)
- Update `load_project_config()` detection: `full.default.max_coworkers.is_some()` → `full.default.max_in_progress_tasks.is_some()` (line 1043)

- [ ] **Step 4: Update DaemonConfig and DaemonState in mod.rs**

In `src/daemon/mod.rs`:
- Rename `DaemonConfig.max_coworkers` field (line 206) to `max_in_progress_tasks`. Update doc comment.
- Update env var resolution (lines 288–299): `MIDTOWN_MAX_COWORKERS` → `MIDTOWN_MAX_IN_PROGRESS_TASKS`, `.max_coworkers()` → `.max_in_progress_tasks()`, `DEFAULT_MAX_COWORKERS` → `DEFAULT_MAX_IN_PROGRESS_TASKS`.
- Update field assignment at line 320.
- Rename `DaemonState.max_coworkers` field (line 482) to `max_in_progress_tasks`. Update doc comment.
- Rename `new()` parameter (line 1308) and field assignment (line 1424).

- [ ] **Step 5: Replace limit methods on DaemonState**

Delete `is_at_coworker_limit()` (lines 1138–1156), `is_at_dev_limit()` (lines 1158–1176), and `has_available_coworker_slot()` (lines 1178–1196).

Add a single new method:

```rust
    /// Check if the daemon is at the in-progress task limit.
    ///
    /// Reads task status from disk. Used by RPC handlers (`rpc_coworker.rs`,
    /// `chat.rs`) that operate outside the snapshot pipeline and don't have
    /// access to a pre-computed snapshot. The snapshot pipeline uses
    /// `snap.is_at_task_limit` (pre-computed from `in_progress_tasks.len()`)
    /// for pure decision functions.
    ///
    /// Orphaned tasks (in-progress but no running coworker) DO count toward
    /// the limit — orphan recovery will reassign or clear them.
    fn is_at_task_limit(&self) -> bool {
        let tasks = crate::tasks::read_tasks_for_repo(Some(self.paths.dir_key()));
        let in_progress_count = tasks
            .iter()
            .filter(|t| t.status == crate::tasks::TaskStatus::InProgress)
            .count();
        in_progress_count >= self.max_in_progress_tasks
    }
```

Note: The spec says `is_at_task_limit()` should read from the snapshot. However, two call sites (`rpc_coworker.rs:42` and `chat.rs:260`) operate on `DaemonState` directly without a snapshot. Rather than threading snapshot data to these call sites (significant refactor), this method reads from disk. The snapshot version (`snap.is_at_task_limit`) is pre-computed for pure decision functions. Update the spec to reflect this.

- [ ] **Step 6: Update daemon startup in mod.rs**

At lines 3109–3164:
1. `start_webhook_server` call: `config.max_coworkers` → `config.max_in_progress_tasks`
2. `DaemonState::new` call: `config.max_coworkers` → `config.max_in_progress_tasks`
3. Replace the startup log message:

```rust
    info!(
        "Max in-progress tasks limit: {}",
        config.max_in_progress_tasks,
    );
```

- [ ] **Step 7: Update WorldSnapshot**

In `src/daemon/snapshot.rs`:

Replace fields (lines 434–438):
```rust
    /// Whether the daemon is at the in-progress task limit.
    pub is_at_task_limit: bool,
    /// Maximum in-progress tasks (from config). Available to pure decision functions
    /// for per-spawn limit checks in the dispatch loop.
    pub max_in_progress_tasks: usize,
```

Add `blocks_map` field after `task_parent_map` (~line 361):
```rust
    /// Inverted blocking graph: task_id → list of task_ids it unblocks.
    /// Built from `Task.blocked_by` during snapshot collection.
    /// Used by dispatch priority to identify tasks that unblock other work.
    #[serde(default)]
    pub blocks_map: HashMap<String, Vec<String>>,
```

Add blocks_map computation after line 837 (near `pending_tasks_without_owners`):
```rust
    // Build inverted blocking graph: for each task X referenced in some task Y's
    // blocked_by, map X → [Y, ...]. Only include non-completed tasks.
    let mut blocks_map: HashMap<String, Vec<String>> = HashMap::new();
    for task in all_tasks.iter().filter(|t| t.status != crate::tasks::TaskStatus::Completed) {
        for blocker_id in &task.blocked_by {
            blocks_map
                .entry(blocker_id.clone())
                .or_default()
                .push(task.id.clone());
        }
    }
```

Update computation (lines 1190–1191):
```rust
    let is_at_task_limit = in_progress_tasks.len() >= state.max_in_progress_tasks;
```

Update assignment (lines 1506–1507):
```rust
        is_at_task_limit,
        max_in_progress_tasks: state.max_in_progress_tasks,
        blocks_map,
```

Update test helper (lines 1585–1586):
```rust
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
```

- [ ] **Step 8: Update webhook.rs**

- `start_webhook_server` parameter: `max_coworkers: usize` → `max_in_progress_tasks: usize` (line 299)
- `WebState` field: `max_coworkers` → `max_in_progress_tasks` (line 344)
- Field assignment in function body

- [ ] **Step 9: Update web.rs**

- `WebState.max_coworkers` → `max_in_progress_tasks` (line 184)
- Dashboard JSON key: `"max_coworkers"` → `"max_in_progress_tasks"` (line 1404)

- [ ] **Step 10: Update rpc_status.rs**

Replace (lines 200–201):
```rust
            "max_coworkers": state.max_coworkers,
            "max_dev_coworkers": state.max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1),
```

with:
```rust
            "max_in_progress_tasks": state.max_in_progress_tasks,
            "in_progress_task_count": {
                let tasks = crate::tasks::read_tasks_for_repo(Some(&state.paths.dir_key()));
                tasks.iter().filter(|t| t.status == crate::tasks::TaskStatus::InProgress).count()
            },
```

Remove the `REVIEW_HEADROOM` import if present.

- [ ] **Step 11: Update rpc_coworker.rs**

At line 42, replace `state.is_at_dev_limit(&channel_lead_names)` with `state.is_at_task_limit()`. Remove the `channel_lead_names` variable if it's only used for the limit check (check other usages first).

Update the error message (lines 47–50):
```rust
                format!(
                    "In-progress task limit ({}) reached. Adjust with MIDTOWN_MAX_IN_PROGRESS_TASKS or max_in_progress_tasks in config.toml",
                    state.max_in_progress_tasks,
                ),
```

At line 1105, replace `"max_coworkers": state.max_coworkers` with `"max_in_progress_tasks": state.max_in_progress_tasks`.

- [ ] **Step 12: Update chat.rs**

At line 260, replace `state.is_at_dev_limit(&channel_lead_names)` with `state.is_at_task_limit()`.

- [ ] **Step 13: Update pr.rs**

Replace all `state.is_at_dev_limit(&channel_lead_names)` calls with `state.is_at_task_limit()`. Call sites at lines: 720, 2384, 2592, 2672, 2693, 2705, 2795, 3584, 3725, 3823. Rename local variables `at_dev_limit` / `is_at_dev_limit` to `at_task_limit` / `is_at_task_limit`.

- [ ] **Step 14: Update rules.rs**

- `decide_pr_action()` (line 746): rename param `at_dev_limit: bool` → `at_task_limit: bool`. Update all internal references.
- `decide_mention_action()` (line 1027): rename param `at_dev_limit: bool` → `at_task_limit: bool`. Update internal references.
- `OrphanRecoveryContext` (line 933): rename field `at_dev_limit` → `at_task_limit`. Update all usages.

- [ ] **Step 15: Update dispatch.rs**

At line 483: `at_dev_limit: snap.is_at_dev_limit` → `at_task_limit: snap.is_at_task_limit`

At line 966: `if snap.is_at_dev_limit` → `if snap.is_at_task_limit`

At line 991: `at_dev_limit: snap.is_at_dev_limit` → `at_task_limit: snap.is_at_task_limit`

At line 1520: update similarly.

Replace the local limit re-derivation block (lines 1742–1756):

```rust
    // Dev cap = max_coworkers (REVIEW_HEADROOM does NOT reduce dev slots).
    let dev_cap = state.max_coworkers;
    // Use running coworkers from snapshot (excludes lead and channel leads).
    let channel_lead_names = snap.channel_lead_names();
    let current_coworker_count = snap
        .coworkers
        .running_coworkers
        .iter()
        .filter(|cw| is_non_lead_coworker(&cw.name, &snap.project_name, &channel_lead_names))
        .count();
```

with:

```rust
    // Task limit: count current in-progress tasks from the snapshot.
    let in_progress_count = snap.in_progress_tasks.len();
    let task_cap = snap.max_in_progress_tasks;
```

And in the loop body (lines 1754–1761), replace:

```rust
        let effective_count = current_coworker_count + spawns_queued_this_tick;
        if effective_count >= dev_cap {
            debug!(
                "Dev coworkers limit reached ({}+{} >= {}), deferring unowned task !{}",
                current_coworker_count, spawns_queued_this_tick, dev_cap, task.id
            );
            break;
        }
```

with:

```rust
        let effective_count = in_progress_count + spawns_queued_this_tick;
        if effective_count >= task_cap {
            debug!(
                "In-progress task limit reached ({}+{} >= {}), deferring unowned task !{}",
                in_progress_count, spawns_queued_this_tick, task_cap, task.id
            );
            break;
        }
```

- [ ] **Step 16: Update CLI config.rs**

In `src/bin/midtown/cli/config.rs`, replace all `"default.max_coworkers"` with `"default.max_in_progress_tasks"`. Update field access: `config.default.max_coworkers` → `config.default.max_in_progress_tasks`.

- [ ] **Step 17: Update CLI response.rs**

At line 31: `pub max_coworkers: Option<usize>` → `pub max_in_progress_tasks: Option<usize>`. Update all display formatting that references the field (check lines 231 and 330).

- [ ] **Step 18: Update chat app.rs**

Replace all `max_coworkers` references with `max_in_progress_tasks`:
- `CoworkerStatusData.max_coworkers` (line 94)
- `App.max_coworkers` (line 345)
- Default values (lines 705, 1094, 4145)
- JSON parsing (lines 3673–3674, 3787)
- Assignment from data (line 923)

- [ ] **Step 19: Update board.rs (production AND test code)**

At line 556, replace:
```rust
    let header = format!("Coworkers ({}/{})", active_count, app.max_coworkers);
```
with:
```rust
    let header = format!("Coworkers ({}/{})", active_count, app.max_in_progress_tasks);
```

Also update test code within `board.rs` at lines 1201, 1344, 1399, 1449:
`app.max_coworkers = 4` → `app.max_in_progress_tasks = 4`

- [ ] **Step 20: Verify compilation**

```bash
cargo check 2>&1 | tail -20
```

Expected: compiles with no errors. Clean up any unused import warnings (e.g., `REVIEW_HEADROOM`, `is_non_lead_coworker` if no longer used by limit checks).

- [ ] **Step 21: Commit**

```bash
git add -A
git commit -m "refactor: replace max_coworkers with max_in_progress_tasks, remove REVIEW_HEADROOM

Remove process-based coworker limits (max_coworkers, REVIEW_HEADROOM,
is_at_dev_limit, is_at_coworker_limit) and replace with a single
task-based limit (max_in_progress_tasks, is_at_task_limit).

All task types (dev, review) now share the same limit. Coworker
processes are no longer directly capped — the task limit is the
only governor."
```

### Task 2: Add deprecation warning for old max_coworkers config key

**Files:**
- Modify: `src/config.rs` (ProjectConfig, load_project_config)
- Modify: `src/daemon/mod.rs` (DaemonConfig::default startup)

- [ ] **Step 1: Add deprecated field to ProjectConfig**

In `src/config.rs`, add a hidden serde field to `ProjectConfig` that catches the old key:

```rust
    /// Deprecated: use max_in_progress_tasks instead.
    /// Retained only for deprecation warning detection.
    #[serde(default, alias = "max_coworkers")]
    #[doc(hidden)]
    deprecated_max_coworkers: Option<usize>,
```

Note: `alias` doesn't work here since `max_coworkers` is removed as a field — use `serde(rename)` on a separate field or detect it in the load path. The simpler approach: in `load_project_config()`, after parsing, check if the raw TOML string contains `max_coworkers` (not `max_in_progress_tasks`):

```rust
    // Deprecation warning for old config key
    if contents.contains("max_coworkers") && !contents.contains("max_in_progress_tasks") {
        tracing::warn!(
            "Config file {} uses deprecated 'max_coworkers' key. \
             Please rename to 'max_in_progress_tasks'. \
             Note: the new setting limits concurrent tasks, not processes. \
             If you previously relied on REVIEW_HEADROOM overflow, \
             set max_in_progress_tasks to your old effective ceiling.",
        );
    }
```

Add similar detection in `GlobalConfig::load()` for the global config.

- [ ] **Step 2: Add env var deprecation check at daemon startup**

In `DaemonConfig::default()` (mod.rs), after the `max_in_progress_tasks` resolution, add:

```rust
        if std::env::var("MIDTOWN_MAX_COWORKERS").is_ok() {
            tracing::warn!(
                "MIDTOWN_MAX_COWORKERS is deprecated. Use MIDTOWN_MAX_IN_PROGRESS_TASKS instead."
            );
        }
```

- [ ] **Step 3: Run tests**

```bash
cargo test --lib -- config 2>&1 | tail -20
```

- [ ] **Step 4: Commit**

```bash
git add src/config.rs src/daemon/mod.rs
git commit -m "feat: add deprecation warnings for old max_coworkers config key and env var"
```

### Task 3: Update config tests

**Files:**
- Modify: `src/config.rs` (test functions referencing max_coworkers)
- Modify: `src/bin/midtown/cli/config_tests.rs`

- [ ] **Step 1: Update config.rs tests**

In `src/config.rs`, find all test functions referencing `max_coworkers` and update:
- Field names: `max_coworkers` → `max_in_progress_tasks`
- Method calls: `.max_coworkers()` → `.max_in_progress_tasks()`
- TOML strings: `max_coworkers = 8` → `max_in_progress_tasks = 8`
- Comments and assertions
- The test at line 266 with `max_coworkers: Some(8)` — update to `max_in_progress_tasks: Some(8)`

- [ ] **Step 2: Update CLI config tests**

In `src/bin/midtown/cli/config_tests.rs`, update all references from `max_coworkers` to `max_in_progress_tasks` in key strings, assertions, and test names.

- [ ] **Step 3: Run tests**

```bash
cargo test --lib -- config 2>&1 | tail -20
```

Expected: all config tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/config.rs src/bin/midtown/cli/config_tests.rs
git commit -m "test: update config tests for max_in_progress_tasks rename"
```

### Task 4: Update remaining test files

**Files:**
- Modify: `src/daemon/health_tests.rs` (5 occurrences)
- Modify: `src/daemon/dispatch_tests.rs` (~14 occurrences)
- Modify: `src/daemon/dispatch_dev_limit_tests.rs` → rename to `dispatch_task_limit_tests.rs`
- Modify: `src/daemon/mod_tests.rs`
- Modify: `tests/dispatch_e2e.rs`
- Modify: `tests/multi_tick_harness.rs`
- Modify: `src/web_tests.rs`
- Modify: `src/bin/midtown/cli/response_tests.rs`
- Modify: `src/bin/midtown/cli/chat/ui/board_tests.rs`
- Modify: All snapshot fixture JSON files in `tests/fixtures/snapshot/`

- [ ] **Step 1: Update health_tests.rs**

Replace all `is_at_coworker_limit: false` and `is_at_dev_limit: false` with `is_at_task_limit: false, max_in_progress_tasks: 8`. Add `blocks_map: HashMap::new()` to each snapshot struct literal.

- [ ] **Step 2: Update dispatch_tests.rs**

Same pattern. For the test at line 4099 that sets `snap.is_at_dev_limit = true`, change to `snap.is_at_task_limit = true`.

- [ ] **Step 3: Rename and rewrite dispatch_dev_limit_tests.rs**

Rename `src/daemon/dispatch_dev_limit_tests.rs` to `src/daemon/dispatch_task_limit_tests.rs`. Update the module declaration in the source file that includes it (check whether it's in `dispatch.rs` or `mod.rs`). Rewrite tests for task-based limits:

- When `in_progress_tasks.len()` < `max_in_progress_tasks`, dispatch proceeds
- When `in_progress_tasks.len()` >= `max_in_progress_tasks`, dispatch defers
- `spawns_queued_this_tick` prevents overshooting within a tick

- [ ] **Step 4: Update mod_tests.rs**

Remove dev cap math tests (lines ~1072–1094 testing REVIEW_HEADROOM subtraction). Remove `is_at_dev_limit`/`is_at_coworker_limit` regression tests (lines ~1448–1510).

- [ ] **Step 5: Update E2E test files**

In `tests/dispatch_e2e.rs`:
- `is_at_coworker_limit: bool` → `is_at_task_limit: bool` in `DispatchSnapshot` struct (line 70)
- Update parsing (line 210) and assertions (lines 1059, 1308)

In `tests/multi_tick_harness.rs`:
- `"is_at_coworker_limit": false` → `"is_at_task_limit": false` (line 149)
- Add `"max_in_progress_tasks": 8` and `"blocks_map": {}` to the snapshot JSON template

- [ ] **Step 6: Update CLI and UI test files**

In `src/web_tests.rs`: replace all `max_coworkers: 8` with `max_in_progress_tasks: 8`.

In `src/bin/midtown/cli/response_tests.rs`: replace `max_coworkers: Some(N)` / `None` with `max_in_progress_tasks: Some(N)` / `None`.

In `src/bin/midtown/cli/chat/ui/board_tests.rs`: replace `app.max_coworkers = 4` with `app.max_in_progress_tasks = 4`.

- [ ] **Step 7: Update snapshot fixture JSON files**

All JSON files in `tests/fixtures/snapshot/` need updating. The `#[serde(default)]` annotation on `blocks_map` and `max_in_progress_tasks` means missing keys deserialize to defaults, but `is_at_coworker_limit` / `is_at_dev_limit` keys must be renamed since those fields no longer exist. Use a script:

```bash
cd tests/fixtures/snapshot
for f in *.json; do
  python3 -c "
import json, sys
with open('$f') as fh:
    data = json.load(fh)
# Rename fields
if 'is_at_coworker_limit' in data:
    data['is_at_task_limit'] = data.pop('is_at_coworker_limit')
data.pop('is_at_dev_limit', None)
data.setdefault('max_in_progress_tasks', 8)
data.setdefault('blocks_map', {})
with open('$f', 'w') as fh:
    json.dump(data, fh, indent=2)
    fh.write('\n')
"
done
```

Manually verify a few files to confirm correctness.

- [ ] **Step 8: Run full test suite**

```bash
cargo test 2>&1 | tail -30
```

Expected: all tests pass.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "test: update all tests and fixtures for max_in_progress_tasks migration"
```

## Chunk 2: Dispatch Priority Module

### Task 5: Write dispatch_priority tests

**Files:**
- Create: `src/daemon/dispatch_priority.rs`
- Create: `src/daemon/dispatch_priority_tests.rs`

- [ ] **Step 1: Create module stub with test module declaration**

Create `src/daemon/dispatch_priority.rs`:

```rust
//! Dispatch priority ordering for pending tasks.

use crate::tasks::Task;
use std::collections::{HashMap, HashSet};

pub(crate) fn prioritize_pending_tasks(
    _pending_tasks: &[Task],
    _in_progress_task_ids: &HashSet<String>,
    _task_parent_map: &HashMap<String, String>,
    _blocks_map: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    todo!()
}

#[path = "dispatch_priority_tests.rs"]
#[cfg(test)]
mod tests;
```

In `src/daemon/mod.rs`, add after existing module declarations:

```rust
pub(crate) mod dispatch_priority;
```

Note: the test module is declared inside `dispatch_priority.rs` (not `mod.rs`) per project conventions. The test file uses `use super::prioritize_pending_tasks;` (not `use super::dispatch_priority::...`).

- [ ] **Step 2: Write failing tests**

Create `src/daemon/dispatch_priority_tests.rs`:

```rust
use super::prioritize_pending_tasks;
use crate::tasks::{Task, TaskStatus};
use std::collections::{HashMap, HashSet};

fn make_task(id: &str, created_secs_ago: u64) -> Task {
    Task {
        id: id.to_string(),
        subject: format!("Task {}", id),
        status: TaskStatus::Pending,
        owner: None,
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: Some(
            std::time::SystemTime::now()
                - std::time::Duration::from_secs(created_secs_ago),
        ),
    }
}

#[test]
fn fifo_ordering_when_no_parents_or_blockers() {
    let tasks = vec![make_task("3", 10), make_task("1", 30), make_task("2", 20)];
    let in_progress: HashSet<String> = HashSet::new();
    let parent_map: HashMap<String, String> = HashMap::new();
    let blocks_map: HashMap<String, Vec<String>> = HashMap::new();

    let result = prioritize_pending_tasks(&tasks, &in_progress, &parent_map, &blocks_map);

    // FIFO = oldest first: 1 (30s ago), 2 (20s ago), 3 (10s ago)
    assert_eq!(result, vec!["1", "2", "3"]);
}

#[test]
fn children_of_in_progress_parents_come_first() {
    let tasks = vec![make_task("A", 30), make_task("B", 20), make_task("C", 10)];
    let in_progress: HashSet<String> = ["parent-1".to_string()].into();
    let parent_map: HashMap<String, String> =
        [("C".to_string(), "parent-1".to_string())].into();
    let blocks_map: HashMap<String, Vec<String>> = HashMap::new();

    let result = prioritize_pending_tasks(&tasks, &in_progress, &parent_map, &blocks_map);

    // C is child of in-progress parent → tier 1. A, B → tier 3 FIFO.
    assert_eq!(result, vec!["C", "A", "B"]);
}

#[test]
fn child_of_non_in_progress_parent_is_fifo() {
    let tasks = vec![make_task("A", 30), make_task("B", 20)];
    let in_progress: HashSet<String> = HashSet::new();
    let parent_map: HashMap<String, String> =
        [("B".to_string(), "parent-1".to_string())].into();
    let blocks_map: HashMap<String, Vec<String>> = HashMap::new();

    let result = prioritize_pending_tasks(&tasks, &in_progress, &parent_map, &blocks_map);

    // parent-1 is NOT in progress → B falls to tier 3. FIFO: A, B.
    assert_eq!(result, vec!["A", "B"]);
}

#[test]
fn blockers_come_before_fifo() {
    let tasks = vec![make_task("A", 30), make_task("B", 20), make_task("C", 10)];
    let in_progress: HashSet<String> = HashSet::new();
    let parent_map: HashMap<String, String> = HashMap::new();
    let blocks_map: HashMap<String, Vec<String>> =
        [("B".to_string(), vec!["X".to_string()])].into();

    let result = prioritize_pending_tasks(&tasks, &in_progress, &parent_map, &blocks_map);

    // B is a blocker → tier 2. A, C → tier 3 FIFO.
    assert_eq!(result, vec!["B", "A", "C"]);
}

#[test]
fn children_of_in_progress_beat_blockers() {
    let tasks = vec![make_task("A", 30), make_task("B", 20), make_task("C", 10)];
    let in_progress: HashSet<String> = ["parent-1".to_string()].into();
    let parent_map: HashMap<String, String> =
        [("C".to_string(), "parent-1".to_string())].into();
    let blocks_map: HashMap<String, Vec<String>> =
        [("A".to_string(), vec!["X".to_string()])].into();

    let result = prioritize_pending_tasks(&tasks, &in_progress, &parent_map, &blocks_map);

    // C → tier 1 (child of in-progress). A → tier 2 (blocker). B → tier 3.
    assert_eq!(result, vec!["C", "A", "B"]);
}

#[test]
fn fifo_within_same_tier() {
    let tasks = vec![make_task("C1", 20), make_task("C2", 10)];
    let in_progress: HashSet<String> = ["p".to_string()].into();
    let parent_map: HashMap<String, String> = [
        ("C1".to_string(), "p".to_string()),
        ("C2".to_string(), "p".to_string()),
    ]
    .into();
    let blocks_map: HashMap<String, Vec<String>> = HashMap::new();

    let result = prioritize_pending_tasks(&tasks, &in_progress, &parent_map, &blocks_map);

    // Both tier 1. FIFO within: C1 (older) before C2.
    assert_eq!(result, vec!["C1", "C2"]);
}

#[test]
fn empty_input_returns_empty() {
    let result = prioritize_pending_tasks(
        &[],
        &HashSet::new(),
        &HashMap::new(),
        &HashMap::new(),
    );
    assert!(result.is_empty());
}
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
cargo test --lib -- dispatch_priority 2>&1 | tail -20
```

Expected: all 7 tests FAIL (not implemented panic).

- [ ] **Step 4: Commit**

```bash
git add src/daemon/dispatch_priority.rs src/daemon/dispatch_priority_tests.rs src/daemon/mod.rs
git commit -m "test: add dispatch priority tests (failing — implementation next)"
```

### Task 6: Implement dispatch_priority

**Files:**
- Modify: `src/daemon/dispatch_priority.rs`

- [ ] **Step 1: Implement the function**

Replace the contents of `src/daemon/dispatch_priority.rs`:

```rust
//! Dispatch priority ordering for pending tasks.
//!
//! Pure function — no I/O. Called once per tick to order pending tasks
//! before the dispatch loop iterates them.
//!
//! Priority tiers (stable sort — FIFO within each tier):
//! 1. Children of in-progress parents
//! 2. Tasks that block other tasks
//! 3. Everything else (FIFO by creation time)

use crate::tasks::Task;
use std::collections::{HashMap, HashSet};

/// Assign a priority tier to a pending task.
/// Lower number = higher priority.
fn tier(
    task: &Task,
    in_progress_task_ids: &HashSet<String>,
    task_parent_map: &HashMap<String, String>,
    blocks_map: &HashMap<String, Vec<String>>,
) -> u8 {
    // Tier 1: child of an in-progress parent
    if let Some(parent_id) = task_parent_map.get(&task.id) {
        if in_progress_task_ids.contains(parent_id) {
            return 1;
        }
    }
    // Tier 2: blocks at least one other task
    if blocks_map.contains_key(&task.id) {
        return 2;
    }
    // Tier 3: everything else
    3
}

/// Order pending tasks by dispatch priority.
///
/// Input tasks are assumed to already be in FIFO order (oldest first).
/// The output preserves FIFO ordering within each tier.
pub(crate) fn prioritize_pending_tasks(
    pending_tasks: &[Task],
    in_progress_task_ids: &HashSet<String>,
    task_parent_map: &HashMap<String, String>,
    blocks_map: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    // Sort by (created_at ASC) first to establish FIFO baseline,
    // then stable-sort by tier to preserve FIFO within each tier.
    let mut tasks: Vec<&Task> = pending_tasks.iter().collect();
    tasks.sort_by_key(|t| t.created_at);
    tasks.sort_by_key(|t| tier(t, in_progress_task_ids, task_parent_map, blocks_map));
    tasks.into_iter().map(|t| t.id.clone()).collect()
}

#[path = "dispatch_priority_tests.rs"]
#[cfg(test)]
mod tests;
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib -- dispatch_priority 2>&1 | tail -20
```

Expected: all 7 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add src/daemon/dispatch_priority.rs
git commit -m "feat: implement dispatch priority ordering (children > blockers > FIFO)"
```

### Task 7: Wire dispatch priority into the dispatch loop

**Files:**
- Modify: `src/daemon/dispatch.rs` (around line 1753, the pending task iteration)

- [ ] **Step 1: Add priority ordering call**

In the `evaluate_pending_task_dispatch` function, just before the `for task in snap.pending_tasks_without_owners.iter()` loop (line 1753), add:

```rust
    // Order pending tasks by dispatch priority before iterating.
    let in_progress_ids: std::collections::HashSet<String> = snap
        .in_progress_tasks
        .iter()
        .map(|(id, _, _)| id.clone())
        .collect();
    let prioritized_ids = crate::daemon::dispatch_priority::prioritize_pending_tasks(
        &snap.pending_tasks_without_owners,
        &in_progress_ids,
        &snap.task_parent_map,
        &snap.blocks_map,
    );
```

Then change the loop to iterate over the prioritized IDs, looking up each task:

```rust
    for task_id in prioritized_ids.iter() {
        let Some(task) = snap.pending_tasks_without_owners.iter().find(|t| &t.id == task_id) else {
            continue;
        };
```

Make sure the rest of the loop body uses `task` as before (it should — the type is the same `&Task`).

Important: The `in_progress_ids` must be bound to a `let` variable — you cannot pass `&snap.in_progress_tasks.iter().map(...).collect()` directly because the temporary `HashSet` would be dropped before the function uses it.

- [ ] **Step 2: Verify compilation and tests**

```bash
cargo check 2>&1 | tail -10
cargo test --lib 2>&1 | tail -20
```

Expected: compiles and existing tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/daemon/dispatch.rs
git commit -m "feat: wire dispatch priority into pending task dispatch loop"
```

## Chunk 3: Web App, Docs & Final Cleanup

### Task 8: Update web app

**Files:**
- Modify: `web-app/src/lib/types.ts:211`
- Modify: `web-app/src/lib/store.ts:116`
- Modify: `web-app/src/lib/api.ts:15,476-477`
- Modify: `web-app/src/lib/CoworkerStatus.svelte:8,110`
- Modify: `web-app/e2e/helpers.js` (if it references max_coworkers)

- [ ] **Step 1: Update TypeScript type**

In `web-app/src/lib/types.ts:211`:
```typescript
	max_in_progress_tasks?: number;
```

- [ ] **Step 2: Update store**

In `web-app/src/lib/store.ts:116`:
```typescript
export const maxInProgressTasks = writable<number>(8);
```

- [ ] **Step 3: Update API**

In `web-app/src/lib/api.ts`, update import (line 15) and setter (lines 476–477):
```typescript
if (data.max_in_progress_tasks !== undefined) {
    maxInProgressTasks.set(data.max_in_progress_tasks);
}
```

- [ ] **Step 4: Update Svelte component**

In `web-app/src/lib/CoworkerStatus.svelte`:
- Update import: `maxCoworkers` → `maxInProgressTasks`
- Update display (line 110): `{$maxCoworkers}` → `{$maxInProgressTasks}`

- [ ] **Step 5: Update e2e helpers if needed**

Check `web-app/e2e/helpers.js` for `max_coworkers` references and update.

- [ ] **Step 6: Commit**

```bash
git add web-app/
git commit -m "feat: update web app for max_in_progress_tasks"
```

### Task 9: Update documentation

**Files:**
- Modify: `docs/architecture.md`
- Modify: `docs/configuration.md`
- Modify: `README.md` (if max_coworkers mentioned)

- [ ] **Step 1: Update architecture.md**

Search for `max_coworkers`, `REVIEW_HEADROOM`, `is_at_dev_limit`, `is_at_coworker_limit`. Replace with new terminology. Add or update the dispatch section to describe:
- The task limit (`max_in_progress_tasks`)
- Dispatch priority ordering (children of in-progress parents > blockers > FIFO)
- That `is_at_task_limit` on DaemonState reads from disk while `snap.is_at_task_limit` is pre-computed

- [ ] **Step 2: Update configuration.md**

Update config key: `max_coworkers` → `max_in_progress_tasks`. Update env var: `MIDTOWN_MAX_COWORKERS` → `MIDTOWN_MAX_IN_PROGRESS_TASKS`.

- [ ] **Step 3: Update README.md**

Search for `max_coworkers` and update if found.

- [ ] **Step 4: Commit**

```bash
git add docs/ README.md
git commit -m "docs: update for max_in_progress_tasks migration"
```

### Task 10: Final cleanup and verification

- [ ] **Step 1: Run clippy**

```bash
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -30
```

Fix any warnings about unused imports.

- [ ] **Step 2: Run full test suite**

```bash
cargo test 2>&1 | tail -30
```

Expected: all tests pass.

- [ ] **Step 3: Run fmt check**

```bash
cargo fmt --all -- --check
```

- [ ] **Step 4: Commit any cleanup**

```bash
git add -A
git commit -m "chore: clean up unused imports and lint warnings"
```

- [ ] **Step 5: Run coverage diff**

```bash
./scripts/coverage-diff.sh 2>&1 | tail -30
```

Review coverage for new code (`dispatch_priority.rs` should have good coverage from the tests).

- [ ] **Step 6: Update spec to reconcile is_at_task_limit disk read**

Update the spec at `docs/superpowers/specs/2026-03-17-task-limit-replaces-coworker-limit-design.md` to clarify that `DaemonState::is_at_task_limit()` reads from disk for RPC handlers outside the snapshot pipeline, while `snap.is_at_task_limit` is pre-computed for pure decision functions.
