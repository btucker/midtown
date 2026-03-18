# Unified Agent Sessions Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unify the divergent session spawn paths, naming, and type tracking into a single model where leads are bound to channels, forks to threads, and workers to tasks.

**Architecture:** Task struct absorbs all scattered `DaemonPersistentState` HashMaps. `LaunchConfig` collapses to one constructor parameterized by agent type, name, working dir, initial prompt, and optional extra system prompt. Fork spawn path merges into the standard `spawn_coworker()` path. Avenue names are removed; names come from tasks (workers), `--name` flag (forks), or channel name (leads).

**Tech Stack:** Rust, serde JSON, chrono::DateTime

**Spec:** `docs/superpowers/specs/2026-03-18-unified-agent-sessions-design.md`

---

## Task 1: New Task Storage Layer

Create Midtown's own task storage at `~/.midtown/<project>/tasks/`. This is the foundation everything else builds on.

**Files:**
- Create: `src/task_store.rs` — new task persistence layer
- Create: `src/task_store_tests.rs` — tests
- Modify: `src/lib.rs` — add `mod task_store;`

- [ ] **Step 1: Write failing test for save_task / load_task round-trip**

```rust
// src/task_store_tests.rs
use super::*;
use tempfile::TempDir;

#[test]
fn test_save_and_load_task_round_trip() {
    let dir = TempDir::new().unwrap();
    let store = TaskStore::new(dir.path().to_path_buf());

    let task = Task {
        id: "1".to_string(),
        subject: "Add auth endpoint".to_string(),
        status: TaskStatus::Pending,
        description: Some("Implement OAuth2 flow".to_string()),
        blocked_by: vec![],
        channel: Some("auth".to_string()),
        pr: None,
        agent_name: "ghost-town".to_string(),
        agent_type: "midtown-code-author".to_string(),
        session_id: None,
        parent: None,
        message_id: None,
        thread_id: None,
        model: None,
        plan: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    store.save(&task).unwrap();
    let loaded = store.load("1").unwrap();
    assert_eq!(loaded.id, "1");
    assert_eq!(loaded.subject, "Add auth endpoint");
    assert_eq!(loaded.agent_name, "ghost-town");
    assert_eq!(loaded.agent_type, "midtown-code-author");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test test_save_and_load_task_round_trip`
Expected: FAIL — module `task_store` does not exist

- [ ] **Step 3: Write Task struct and TaskStore**

```rust
// src/task_store.rs
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub subject: String,
    pub status: TaskStatus,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub pr: Option<u64>,
    pub agent_name: String,
    pub agent_type: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// Lightweight index entry for fast lookups without reading task files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskIndexEntry {
    pub status: TaskStatus,
    pub parent: Option<String>,
    pub agent_name: String,
}

/// Persistent task storage — one JSON file per task.
pub struct TaskStore {
    tasks_dir: PathBuf,
}

impl TaskStore {
    pub fn new(tasks_dir: PathBuf) -> Self {
        Self { tasks_dir }
    }

    /// Save a task to disk. Sets `updated_at` automatically.
    pub fn save(&self, task: &Task) -> crate::Result<()> {
        std::fs::create_dir_all(&self.tasks_dir)?;
        let path = self.tasks_dir.join(format!("{}.json", task.id));
        let mut task = task.clone();
        task.updated_at = Utc::now();
        let json = serde_json::to_string_pretty(&task)?;
        // Atomic write via temp file + rename
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Load a single task by ID.
    pub fn load(&self, id: &str) -> crate::Result<Task> {
        let path = self.tasks_dir.join(format!("{}.json", id));
        let json = std::fs::read_to_string(&path)?;
        let task: Task = serde_json::from_str(&json)?;
        Ok(task)
    }

    /// Load all tasks from disk.
    pub fn load_all(&self) -> Vec<Task> {
        let Ok(entries) = std::fs::read_dir(&self.tasks_dir) else {
            return Vec::new();
        };
        entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "json"))
            .filter_map(|e| {
                std::fs::read_to_string(e.path())
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
            })
            .collect()
    }

    /// Build index from all tasks on disk.
    pub fn build_index(&self) -> std::collections::HashMap<String, TaskIndexEntry> {
        self.load_all()
            .into_iter()
            .map(|t| {
                (
                    t.id.clone(),
                    TaskIndexEntry {
                        status: t.status,
                        parent: t.parent.clone(),
                        agent_name: t.agent_name.clone(),
                    },
                )
            })
            .collect()
    }

    /// Check if an agent_name is already in use by any active (non-completed) task.
    pub fn is_name_in_use(&self, name: &str) -> bool {
        self.load_all()
            .iter()
            .any(|t| t.agent_name == name && t.status != TaskStatus::Completed)
    }
}
```

- [ ] **Step 4: Add mod declarations**

Add `pub mod task_store;` to `src/lib.rs`. Add `#[path = "task_store_tests.rs"] #[cfg(test)] mod tests;` inside `src/task_store.rs` (at the bottom of the file).

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test test_save_and_load_task_round_trip`
Expected: PASS

- [ ] **Step 6: Write additional tests**

Tests for: `load_all`, `build_index`, `is_name_in_use`, missing file error, `updated_at` auto-set behavior.

- [ ] **Step 7: Run all tests**

Run: `cargo test task_store`
Expected: All PASS

- [ ] **Step 8: Commit**

```bash
git add src/task_store.rs src/task_store_tests.rs src/lib.rs
git commit -m "feat: Add TaskStore with new Task struct and file-per-task persistence"
```

---

## Task 2: SessionRecord Changes

Update `SessionRecord` to use `agent_type` and single `name` field. This is the other foundational struct change.

**Files:**
- Modify: `src/daemon/state.rs:80-175` — SessionRecord struct
- Modify: `src/daemon/state.rs` — any tests referencing old fields

- [ ] **Step 1: Write failing test for new SessionRecord fields**

```rust
// In state_tests.rs or equivalent
#[test]
fn test_session_record_new_fields() {
    let record = SessionRecord {
        agent_type: "midtown-code-author".to_string(),
        name: "ghost-town".to_string(),
        restart_count: 0,
        ..Default::default()
    };
    assert_eq!(record.agent_type, "midtown-code-author");
    assert_eq!(record.name, "ghost-town");
    assert!(!record.is_fork_session());
}

#[test]
fn test_is_fork_session_with_agent_type() {
    let record = SessionRecord {
        agent_type: "midtown-channel-lead".to_string(),
        bound_thread_id: Some("thread-123".to_string()),
        ..Default::default()
    };
    assert!(record.is_fork_session());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test test_session_record_new_fields test_is_fork_session_with_agent_type`
Expected: FAIL — fields don't exist

- [ ] **Step 3: Update SessionRecord struct**

In `src/daemon/state.rs`, modify `SessionRecord`:
- Replace `coworker_type: String` → `agent_type: String`
- Remove `is_reviewer: bool`
- Replace `current_name: Option<String>` + `preferred_name: Option<String>` → `name: String`
- Add `restart_count: u32` with `#[serde(default)]`
- Update `Default` impl: `agent_type: "midtown-code-author".to_string()`, `name: String::new()`, `restart_count: 0`
- Update `is_fork_session()`: change `self.coworker_type == "channel-lead"` to `self.agent_type == "midtown-channel-lead"`

- [ ] **Step 4: Fix all compilation errors**

Search for all references to the removed/renamed fields. Key locations:
- `src/daemon/state.rs` — `coworker_type`, `is_reviewer`, `current_name`, `preferred_name`
- `src/daemon/mod.rs` — spawn_coworker creates SessionRecord
- `src/daemon/effects.rs` — effect handlers create/update SessionRecords
- `src/daemon/dispatch.rs` — checks coworker_type and is_reviewer
- `src/daemon/health.rs` — health checks reference is_reviewer
- `src/daemon/snapshot.rs` — collect_world_snapshot reads coworker_type
- `src/daemon/rpc_coworker.rs` — status handlers check coworker_type
- `src/daemon/rpc_session.rs` — fork creation sets coworker_type
- `src/daemon/rpc_task.rs` — task handlers check is_reviewer
- `src/daemon/rpc_auth.rs` — auth switch checks coworker_type
- `src/daemon/startup.rs` — session recovery checks is_reviewer
- `src/web.rs` — web status references

For each: replace `coworker_type` with `agent_type`, replace `is_reviewer` checks with `agent_type == "midtown-code-reviewer"`, replace `current_name`/`preferred_name` with `name`.

**Important:** This is a large mechanical change. Work file by file. Use `cargo check` frequently to find remaining errors.

**Transition note:** `CoworkerRole` still exists at this point (removed in Task 4). Where callers currently set `coworker_type` from a `CoworkerRole`, use `role.agent_name().to_string()` to get the agent type string (e.g., `"midtown-code-author"`). This temporary bridge avoids hard-coding strings that Task 4 will clean up when it removes `CoworkerRole` entirely.

- [ ] **Step 5: Run full test suite**

Run: `cargo test`
Expected: All PASS (some tests may need field name updates)

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: Replace coworker_type/is_reviewer/current_name with agent_type/name on SessionRecord"
```

---

## Task 3: Agent Type to ExecutionRole Mapping

Add the function that maps agent type strings to `ExecutionRole`, replacing `CoworkerRole::execution_role()`.

**Files:**
- Modify: `src/config.rs` — add `execution_role_for_agent_type()` function
- Create: `src/config_tests.rs` or add to existing test file

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn test_execution_role_for_agent_type() {
    assert_eq!(
        execution_role_for_agent_type("midtown-code-author"),
        ExecutionRole::Coworker,
    );
    assert_eq!(
        execution_role_for_agent_type("midtown-code-reviewer"),
        ExecutionRole::Reviewer,
    );
    assert_eq!(
        execution_role_for_agent_type("midtown-channel-lead"),
        ExecutionRole::ChannelLead,
    );
    assert_eq!(
        execution_role_for_agent_type("midtown-project-lead"),
        ExecutionRole::Lead,
    );
    // User-defined agents default to Coworker
    assert_eq!(
        execution_role_for_agent_type("my-custom-agent"),
        ExecutionRole::Coworker,
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test test_execution_role_for_agent_type`
Expected: FAIL

- [ ] **Step 3: Implement the mapping function**

```rust
// In src/config.rs
pub fn execution_role_for_agent_type(agent_type: &str) -> ExecutionRole {
    match agent_type {
        "midtown-code-author" => ExecutionRole::Coworker,
        "midtown-code-reviewer" => ExecutionRole::Reviewer,
        "midtown-channel-lead" => ExecutionRole::ChannelLead,
        "midtown-project-lead" => ExecutionRole::Lead,
        _ => {
            // User-defined agent types default to Coworker.
            // ExecutionRole::Specialized and ExecutionRole::HeadlessExecute are
            // not reachable through agent_type mapping — they are used by other
            // code paths (e.g., one-off headless executions) and remain unchanged.
            ExecutionRole::Coworker
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test test_execution_role_for_agent_type`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: Add execution_role_for_agent_type mapping function"
```

---

## Task 4: Unified LaunchConfig Constructor

Replace the five `LaunchConfig` constructors with a single `LaunchConfig::new()` plus builder methods.

**Files:**
- Modify: `src/launch.rs:24-680` — remove `CoworkerRole`, replace constructors
- Modify: `src/launch.rs` — update `to_headless_config()` to use agent_type string

- [ ] **Step 1: Write failing test for new constructor**

```rust
#[test]
fn test_launch_config_new() {
    let config = LaunchConfig::new(
        "ghost-town",
        "midtown-code-author",
        std::path::PathBuf::from("/tmp/worktree"),
        "Work on auth endpoint",
        None,
    );
    assert_eq!(config.name, "ghost-town");
    assert_eq!(config.agent_type, "midtown-code-author");
    assert_eq!(config.working_dir, Some("/tmp/worktree".to_string()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test test_launch_config_new`
Expected: FAIL

- [ ] **Step 3: Add `agent_type` field and `new()` constructor to LaunchConfig**

In `src/launch.rs`:
- Add `agent_type: String` field to `LaunchConfig` struct
- Add `system_prompt_extra: Option<String>` field
- Implement `LaunchConfig::new()` that:
  - Resolves `ExecutionRole` from agent_type via `config::execution_role_for_agent_type()`
  - Resolves model and auth_provider from the execution role via existing config functions
  - Sets `session_mode: SessionMode::Fresh`
  - Sets all other fields to defaults
- Add builder methods: `.with_task_id()`, `.with_pr_number()`, `.with_bound_thread_id()`, `.with_channel()`, `.with_session_mode()`, `.with_model()`

- [ ] **Step 4: Update `to_headless_config()` to use `agent_type` field**

Replace `self.role.agent_name()` with `&self.agent_type` when setting the `--agent` flag. Remove the `CoworkerRole`-based branching in system prompt selection — use `agent_type` string matching instead.

- [ ] **Step 5: Migrate all callers of old constructors**

Search for all calls to `LaunchConfig::coworker()`, `LaunchConfig::reviewer()`, `LaunchConfig::resume_reviewer()`, `LaunchConfig::lead()`, `LaunchConfig::channel_lead()`. Replace each with `LaunchConfig::new()` plus appropriate builder calls. Key files:
- `src/daemon/effects.rs` — `Effect::SpawnCoworker` handler, `Effect::SpawnForTask` handler
- `src/daemon/dispatch.rs` — `build_spawn_effects()`, `dispatch_unowned_pending_tasks()`
- `src/daemon/health.rs` — `check_and_respawn_dead_processes()`, `build_reviewer_respawn_effects()`, `ensure_lead_alive()`
- `src/daemon/rpc_coworker.rs` — `handle_lead_spawn()`
- `src/daemon/rpc_session.rs` — `handle_session_detach()`, `handle_session_clear()`
- `src/daemon/rpc_auth.rs` — `handle_auth_switch()`
- `src/daemon/rpc_task.rs` — `deliver_task_prompt()`
- `src/daemon/chat.rs` — `mention_action_to_effects()`
- `src/daemon/startup.rs` — `recover_from_session_records()`
- `src/daemon/mod.rs` — `expedite_lead_respawn_on_user_message()`

**Representative migrations:**

Worker spawn in `dispatch.rs` (was `LaunchConfig::coworker(name, dir_key, session_mode, prompt, task_id)`):
```rust
LaunchConfig::new(
    task.agent_name,                  // name from task, not avenue pool
    &task.agent_type,                 // e.g., "midtown-code-author"
    worktree_path,
    prompt,
    None,
)
.with_task_id(task.id.clone())
.with_channel(task.channel.clone())
```

Reviewer spawn in `health.rs` (was `LaunchConfig::reviewer(name, dir_key, pr_number, restart_count, auth_provider)`):
```rust
LaunchConfig::new(
    task.agent_name,                  // name from the review task
    "midtown-code-reviewer",
    worktree_path,
    reviewer_launch_prompt(pr_number, restart_count, auth_provider, None),
    None,
)
.with_pr_number(pr_number)
.with_task_id(task.id.clone())
```

Channel lead spawn (was `LaunchConfig::channel_lead(channel_name, dir_key, session_mode, domain_context, agents_md)`):
```rust
LaunchConfig::new(
    channel_lead_session_name(&channel_name),
    "midtown-channel-lead",
    repo_root,
    initial_prompt,
    Some(domain_context),  // channel notes + AGENTS.md via system_prompt_extra
)
.with_channel(channel_name.clone())
```

- [ ] **Step 6: Remove `CoworkerRole` enum and old constructors**

Delete `CoworkerRole` enum, its `impl` block (`agent_name()`, `execution_role()`, `agent_role()`, `runtime_context()`, `render_system_prompt()`), and all five old constructors.

Move any system prompt rendering logic that was in `CoworkerRole` methods to standalone functions keyed by agent_type string.

- [ ] **Step 7: Fix compilation errors and run tests**

Run: `cargo check` repeatedly. Then: `cargo test`
Expected: All PASS

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: Unify LaunchConfig into single constructor, remove CoworkerRole"
```

---

## Task 5: Remove Avenue Names

Remove the avenue name pool and `CoworkerManager`'s name allocation logic.

**Files:**
- Modify: `src/coworker.rs` — remove `AVENUE_NAMES`, `OVERFLOW_NAMES`, `next_available_name()`, `next_available_name_excluding()`, `is_coworker_name()`
- Modify: `src/config.rs` — remove avenue name validation in channel name checks
- Modify: `src/daemon/dispatch.rs` — remove name allocation from dispatch

- [ ] **Step 1: Remove name constants and allocation functions from coworker.rs**

Delete `AVENUE_NAMES`, `OVERFLOW_NAMES`, `next_available_name()`, `next_available_name_excluding()`, `is_coworker_name()`.

- [ ] **Step 2: Update all callers that allocated avenue names or checked `is_coworker_name`**

Search for `next_available_name`, `next_available_name_excluding`, and `is_coworker_name`. All call sites:

**`next_available_name` / `next_available_name_excluding`:**
- `src/daemon/dispatch.rs` — `dispatch_unowned_pending_tasks()`: replace with task's `agent_name`

**`is_coworker_name` (5 call sites — all must be updated):**
- `src/rules.rs:898` — `decide_pending_task_action()`: this checks if a name is a coworker (vs. external). Replace with a check against the task store or session records — "does this name belong to an active session?"
- `src/rules.rs:1081` — `decide_orphan_recovery()`: same pattern. **Important:** `rules.rs` functions must remain pure (no I/O). Add the needed data to `WorldSnapshot` during `collect_world_snapshot()` — e.g., a `HashSet<String>` of active session names.
- `src/config.rs:1220` — `ensure_project_config()`: rejects avenue names as channel names. Remove this check entirely (names are no longer from a fixed pool).
- `src/daemon/effects.rs:1115` — `send_session_nudge()`: checks if sender is a coworker. Replace with session name lookup.
- `src/daemon/rpc_task.rs:857` — `deliver_task_prompt()`: checks if name is a coworker. Replace with session name lookup.

- [ ] **Step 3: Fix compilation errors and run tests**

Run: `cargo check` then `cargo test`
Expected: All PASS

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor: Remove avenue name pool, names now come from tasks"
```

---

## Task 6: Unify Fork Spawn Path

Merge `build_fork_config()` / `create_fork_session()` into the standard `LaunchConfig` + `spawn_coworker()` path.

**Files:**
- Modify: `src/daemon/rpc_session.rs:1068-1240` — remove `build_fork_config()`, update `create_fork_session()` to use `LaunchConfig::new()`
- Modify: `src/daemon/effects.rs` — update `respawn_fork()` to use unified path
- Modify: `src/daemon/rpc_session.rs:1654+` — update `handle_session_fork()`

- [ ] **Step 1: Write failing test for fork using LaunchConfig**

```rust
#[test]
fn test_fork_uses_channel_lead_agent_type() {
    // Fork sessions should use midtown-channel-lead via --agent
    let config = LaunchConfig::new(
        "ghost-town",
        "midtown-channel-lead",
        std::path::PathBuf::from("/tmp/repo"),
        "Investigate auth issue",
        Some("Domain context here".to_string()),
    )
    .with_bound_thread_id("thread-123".to_string());

    assert_eq!(config.agent_type, "midtown-channel-lead");
    assert_eq!(config.bound_thread_id, Some("thread-123".to_string()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test test_fork_uses_channel_lead_agent_type`
Expected: FAIL

- [ ] **Step 3: Update create_fork_session to use LaunchConfig**

In `src/daemon/rpc_session.rs`, rewrite `create_fork_session()`:
- Build a `LaunchConfig::new()` with `agent_type: "midtown-channel-lead"`
- Pass domain context via `system_prompt_extra`
- Set `bound_thread_id` via builder method
- Call `spawn_coworker()` instead of the custom fork spawn path
- Keep `slugify_fork_hint()` as the name resolution fallback when no `--name` is provided

- [ ] **Step 4: Update respawn_fork in effects.rs**

In `src/daemon/effects.rs`, update `respawn_fork()` to build a `LaunchConfig` and call `spawn_coworker()` instead of calling `build_fork_config()` directly.

- [ ] **Step 5: Remove build_fork_config()**

Delete `build_fork_config()` from `src/daemon/rpc_session.rs`. Keep `slugify_fork_hint()` (still needed as name fallback).

- [ ] **Step 6: Update fork-related tests**

Update tests in `src/daemon/rpc_session_tests.rs` that called `build_fork_config()`. Tests for `slugify_fork_hint()` stay as-is.

- [ ] **Step 7: Fix compilation errors and run tests**

Run: `cargo check` then `cargo test`
Expected: All PASS

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: Merge fork spawn path into unified LaunchConfig flow"
```

---

## Task 7: Task RPC — Write to New Storage

Update `task.create` and `task.update` to use `TaskStore` and the new Task struct.

**Files:**
- Modify: `src/daemon/rpc_task.rs:281-420` — `handle_task_create()` uses TaskStore
- Modify: `src/daemon/rpc_task.rs:510+` — `handle_task_update()` drops `owner`, uses TaskStore
- Modify: `src/daemon/rpc.rs:526-580` — add `agent_name` param, remove `execution_skill`
- Modify: `src/bin/midtown/cli/task.rs` — CLI arg changes

- [ ] **Step 1: Write failing test for task creation with agent_name**

```rust
#[test]
fn test_task_create_requires_agent_name() {
    // task.create without agent_name should fail
    // task.create with agent_name should succeed and persist to new storage
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test test_task_create_requires_agent_name`
Expected: FAIL

- [ ] **Step 3: Update handle_task_create**

In `src/daemon/rpc.rs`, add `agent_name` param extraction for `task.create`, remove `execution_skill`. In `src/daemon/rpc_task.rs`:
- Require `agent_name` parameter; return error if missing
- Check `TaskStore::is_name_in_use()` — reject with error on collision
- Create `task_store::Task` with all fields
- Call `task_store.save()` instead of `create_task_for_repo()`
- Update task index in `DaemonPersistentState`
- Stop writing to the scattered HashMap fields (`task_channel`, `task_model`, etc.)

- [ ] **Step 4: Update handle_task_update**

- Remove `owner` parameter
- Add `session_id`, `message_id`, `thread_id` as updatable fields
- Load task from `TaskStore`, update mutable fields, save back
- Reject attempts to update immutable fields (`agent_name`, `agent_type`, `parent`)

- [ ] **Step 5: Update CLI task create args**

In `src/bin/midtown/cli/task.rs`:
- Add `--agent-name` argument (required)
- Remove `--execution-skill` argument
- Remove `--owner` from update subcommand

- [ ] **Step 6: Run tests**

Run: `cargo test`
Expected: All PASS

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat: Task create/update uses new TaskStore with agent_name"
```

---

## Task 8: Eliminate DaemonPersistentState HashMap Fields

Remove the scattered `task_*` HashMap fields and update all readers to use `TaskStore`.

**Files:**
- Modify: `src/daemon/state.rs:183+` — remove HashMap fields
- Modify: `src/daemon/snapshot.rs` — `collect_world_snapshot()` reads from TaskStore
- Modify: `src/daemon/dispatch.rs` — reads task metadata from TaskStore
- Modify: `src/daemon/effects.rs` — reads task metadata from TaskStore
- Modify: `src/daemon/rpc_task.rs` — `handle_task_metadata()`, `handle_task_done()`

- [ ] **Step 1: Add TaskStore to DaemonState**

Add a `task_store: TaskStore` field to `DaemonState` (in `src/daemon/mod.rs`), initialized with the project tasks directory.

- [ ] **Step 2: Update snapshot collection**

In `src/daemon/snapshot.rs`, update `collect_world_snapshot()` to read task metadata from `DaemonState::task_store` instead of the HashMap fields on persistent state.

- [ ] **Step 3: Update dispatch to read from TaskStore**

In `src/daemon/dispatch.rs`, replace all reads from `ps.task_channel`, `ps.task_model`, `ps.task_plan`, etc. with `state.task_store.load(task_id)`.

- [ ] **Step 4: Update effects to read from TaskStore**

In `src/daemon/effects.rs`, replace HashMap reads with TaskStore reads.

- [ ] **Step 5: Update remaining RPC handlers**

In `src/daemon/rpc_task.rs`, update `handle_task_metadata()`, `handle_task_done()`, `handle_task_claim()` to use TaskStore.

- [ ] **Step 6: Wire up task index write-through**

Add a `task_index: HashMap<String, TaskIndexEntry>` field to `DaemonPersistentState` (with `#[serde(default)]`). Update `TaskStore::save()` to accept a mutable reference to the index and update it on every write. Alternatively, have the RPC handlers update the index after each `task_store.save()` call. The index is reconciled from disk on daemon startup via `task_store.build_index()`.

- [ ] **Step 7: Remove HashMap fields from DaemonPersistentState**

Remove from `src/daemon/state.rs`:
- `task_channel`, `task_model`, `task_plan`, `task_execution_skill`, `task_thread_id`, `task_message_id`, `task_parent`, `task_agent_type`, `task_pr_number`

For `task_placeholder_comment_id`: find all readers (search for `task_placeholder_comment_id`). These callers currently look up a GitHub comment ID by task ID. Remove the HashMap and update callers to find the placeholder comment via GitHub API search using the session ID stored in comment frontmatter. If this is complex, leave a `// TODO: migrate to frontmatter lookup` comment and file a follow-up.

Keep: `task_session_spans`, and move `task_restart_count` to `SessionRecord` (done in Task 2).

Add `#[serde(default)]` to the new `task_index` field so deserialization of old state files doesn't break.

- [ ] **Step 8: Fix compilation errors and run tests**

Run: `cargo check` then `cargo test`
Expected: All PASS

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor: Eliminate task_* HashMaps from DaemonPersistentState, use TaskStore"
```

---

## Task 9: Migration

Migrate existing tasks from `~/.claude/tasks/` to `~/.midtown/<project>/tasks/` on daemon startup.

**Files:**
- Create: `src/daemon/migration.rs` — migration logic
- Create: `src/daemon/migration_tests.rs` — tests
- Modify: `src/daemon/mod.rs` — call migration on startup

- [ ] **Step 1: Write failing test for migration**

```rust
#[test]
fn test_migrate_old_task_to_new_format() {
    // Set up old-format task JSON in temp dir
    // Set up old-format DaemonPersistentState with task_channel, task_agent_type, etc.
    // Run migration
    // Verify new Task struct has all fields populated
    // Verify agent_name defaults from owner
    // Verify agent_type defaults to midtown-code-author
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test test_migrate_old_task_to_new_format`
Expected: FAIL

- [ ] **Step 3: Implement migration function**

```rust
// src/daemon/migration.rs

/// Migrate tasks from ~/.claude/tasks/midtown-<repo>/ to ~/.midtown/<project>/tasks/.
/// Reads old DaemonPersistentState HashMap fields to populate new Task fields.
/// Idempotent — skips tasks that already exist in the new location.
///
/// The old Task format is defined in `src/tasks.rs` as `crate::tasks::Task` with fields:
///   { id, subject, status, owner: Option<String>, description, blocked_by, channel, pr, created_at }
/// Load old tasks using `crate::tasks::read_tasks_for_repo(dir_key)`.
///
/// The old HashMap fields come from DaemonPersistentState (loaded from state.json):
///   task_channel, task_model, task_plan, task_thread_id, task_message_id,
///   task_parent, task_agent_type, task_pr_number
/// These are deserialized using serde's #[serde(default)] so they'll be empty HashMaps
/// if the old state didn't have them.
pub fn migrate_tasks_if_needed(
    old_tasks: &[crate::tasks::Task],
    old_state: &DaemonPersistentState,  // still has old HashMap fields during migration
    task_store: &crate::task_store::TaskStore,
) -> Vec<String> {
    // For each old task:
    //   - Skip if already exists in task_store
    //   - Map owner → agent_name (or slugify subject if no owner)
    //   - Map task_agent_type → agent_type (default "midtown-code-author")
    //   - Merge task_channel, task_model, task_plan, task_thread_id,
    //     task_message_id, task_parent, task_pr_number
    //   - Set created_at from old created_at or now, updated_at to now
    //   - Save to task_store
    // Return list of migrated task IDs
}
```

- [ ] **Step 4: Wire migration into daemon startup**

In `src/daemon/mod.rs`, call `migrate_tasks_if_needed()` during daemon initialization, before the main event loop.

- [ ] **Step 5: Run tests**

Run: `cargo test migration`
Expected: All PASS

- [ ] **Step 6: Commit**

```bash
git add src/daemon/migration.rs src/daemon/migration_tests.rs src/daemon/mod.rs
git commit -m "feat: One-time migration of tasks from ~/.claude/tasks/ to ~/.midtown/<project>/tasks/"
```

---

## Task 10: Update Agent Definitions

Update lead agent definitions to pass `--agent-name` when creating tasks.

**Files:**
- Modify: `agents/lead-common.md` — update task creation instructions
- Modify: `agents/definitions/midtown-project-lead.md` — if task creation instructions exist
- Modify: `agents/definitions/midtown-channel-lead.md` — if task creation instructions exist

- [ ] **Step 1: Update lead-common.md task creation instructions**

Add `--agent-name` to task creation examples. The lead must provide a creative name when creating tasks:

```markdown
midtown task create "Add OAuth2 endpoint" --agent-name "phantom-gate" --agent-type "midtown-code-author" --channel auth
```

- [ ] **Step 2: Update fork naming instructions if needed**

Fork naming instructions in `lead-common.md` (lines 70-93) should remain — the `--name` flag on `midtown agent fork` is unchanged.

- [ ] **Step 3: Commit**

```bash
git add agents/
git commit -m "docs: Update agent definitions to pass --agent-name on task creation"
```

---

## Task 11: Update CLAUDE.md with Session Taxonomy

Document the session taxonomy in CLAUDE.md as specified.

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add session taxonomy section**

Add after the "Conventions" section:

```markdown
## Session Taxonomy

Three session types, each bound to exactly one thing:

- **Lead** → bound to a **channel**. Named after the channel. Agent type: `midtown-channel-lead` (or `midtown-project-lead` for main).
- **Fork** → bound to a **thread**. Named by the lead's `--name` flag (slugify fallback). Agent type: `midtown-channel-lead`.
- **Worker** → bound to a **task**. Named by the task's `agent_name`. Agent type from the task's `agent_type` field.

**Invariants:**
- One-to-one mapping between tasks and worker sessions.
- Forks never have tasks — they are thread-bound research sessions.
- `agent_type` refers to the agent definition passed to `--agent` (e.g., `midtown-code-author`). It is NOT the session name.
- `agent_name` is the creative session name (e.g., `ghost-town`). It is NOT the agent definition.
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: Add session taxonomy to CLAUDE.md"
```

---

## Task 12: Clippy, Fmt, and Full Test Pass

Final cleanup and verification.

**Files:** All modified files

- [ ] **Step 1: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: No warnings

- [ ] **Step 2: Run fmt**

Run: `cargo fmt --all -- --check`
Expected: No formatting issues

- [ ] **Step 3: Run full test suite**

Run: `cargo test`
Expected: All PASS

- [ ] **Step 4: Run coverage diff**

Run: `./scripts/coverage-diff.sh`
Review uncovered lines in changed files. New code should have reasonable coverage.

- [ ] **Step 5: Fix any issues found**

Address clippy warnings, test failures, or coverage gaps.

- [ ] **Step 6: Final commit if needed**

```bash
git add -A
git commit -m "chore: Fix clippy warnings and test cleanup"
```
