# Unified Agent Sessions Design

## Problem

The codebase has divergent spawn paths, naming schemes, and type tracking for different session types (workers, reviewers, forks, channel leads). This creates confusion, duplicated logic, and makes it hard to reason about sessions uniformly.

Key issues:
- **Four separate `LaunchConfig` constructors** plus a completely separate fork spawn path (`build_fork_config()` / `create_fork_session()`)
- **Avenue names** randomly assigned from a pool, unrelated to the work being done
- **`coworker_type`** is a stringly-typed field (`"dev"`, `"reviewer"`, `"channel-lead"`) that duplicates what the agent type already expresses
- **`agent_name`** is overloaded — means agent definition in `launch.rs` but session name in `TaskSessionSpan`
- **Task metadata scattered across `DaemonPersistentState`** — `task_channel`, `task_model`, `task_plan`, `task_thread_id`, `task_message_id`, `task_parent`, `task_agent_type`, `task_pr_number` are all separate `HashMap`s rather than fields on the Task struct
- **Task storage** uses Claude Code's `~/.claude/tasks/` format, constraining our schema

## Session Taxonomy

Three session types, each bound to exactly one thing:

| Session type | Bound to | Name source | Agent type (`--agent`) |
|---|---|---|---|
| **Lead** | Channel | Channel name | Config-defined (default `midtown-channel-lead`, `midtown-project-lead` for main) |
| **Fork** | Thread | Lead's `--name` flag, slugify fallback | `midtown-channel-lead` |
| **Worker** | Task | `agent_name` from the task | `agent_type` from the task (e.g., `midtown-code-author`, `midtown-code-reviewer`, or user-defined) |

**Invariants:**
- One-to-one mapping between tasks and worker sessions. A task has exactly one worker session. A worker session works on exactly one task.
- Forks never have tasks. They are thread-bound research sessions.

This taxonomy must be documented in CLAUDE.md.

## Task Struct

The `Task` struct is redesigned. All task metadata that was previously scattered across `HashMap`s on `DaemonPersistentState` moves onto the struct directly.

```rust
pub struct Task {
    pub id: String,
    pub subject: String,
    pub status: TaskStatus,
    pub description: Option<String>,
    pub blocked_by: Vec<String>,
    pub channel: Option<String>,
    pub pr: Option<u64>,
    pub agent_name: String,             // Creative session name, set by lead at creation
    pub agent_type: String,             // Agent definition for --agent flag, set by lead
    pub session_id: Option<String>,     // Bound session, set by daemon at spawn
    pub parent: Option<String>,         // Parent task ID (e.g., review task is child of dev task)
    pub message_id: Option<String>,     // Channel message that spawned this task
    pub thread_id: Option<String>,      // Thread the task is bound to
    pub model: Option<String>,          // Model override for this task's session
    pub plan: Option<String>,           // Path to execution plan
    pub created_at: DateTime<Utc>,      // Set at creation
    pub updated_at: DateTime<Utc>,      // Updated on any mutation
}
```

`TaskStatus` remains unchanged (`Pending`, `InProgress`, `Completed`).

**Immutable after creation:** `id`, `agent_name`, `agent_type`, `parent`, `created_at`.

**Mutable via `task.update`:** `subject`, `status`, `description`, `blocked_by`, `channel`, `pr`, `session_id`, `message_id`, `thread_id`, `model`, `plan`.

**`updated_at`** is set automatically by the task persistence layer (`save_task()`) on every write, not by individual callers.

### Fields removed from Task
- `owner` — redundant with `agent_name` and the 1:1 task-session binding

### Fields changed
- `created_at` moves from `Option<std::time::SystemTime>` with `#[serde(skip)]` (populated from filesystem metadata) to a serialized `DateTime<Utc>`

### HashMap fields eliminated from DaemonPersistentState
These maps are replaced by fields on the Task struct:
- `task_channel` → `channel`
- `task_model` → `model`
- `task_plan` → `plan`
- `task_thread_id` → `thread_id`
- `task_message_id` → `message_id`
- `task_parent` → `parent`
- `task_agent_type` → `agent_type`
- `task_pr_number` → `pr`

### Fields dropped entirely
- `task_execution_skill` — no longer needed
- `task_placeholder_comment_id` — can be found via PR comment frontmatter containing the session ID

### Fields that stay on DaemonPersistentState
- `task_session_spans` — observability data, not per-task metadata
- `task_restart_count` — moves to `SessionRecord` (it's about the session, not the task)

## Task Storage

Tasks move from Claude Code's storage (`~/.claude/tasks/midtown-<repo>/`) to Midtown's own storage:

- **Location:** `~/.midtown/<project>/tasks/<task-id>.json`
- **Format:** One JSON file per task, Midtown's own schema
- **Index:** The daemon's persistent state holds a lightweight task index for fast lookups without directory scanning. Updated write-through on every task mutation. Reconciled against task files on daemon startup.
  ```rust
  struct TaskIndexEntry {
      pub status: TaskStatus,
      pub parent: Option<String>,
      pub agent_name: String,
  }
  // Stored as HashMap<String, TaskIndexEntry> keyed by task ID
  ```
- **Migration:** One-time migration of existing tasks from `~/.claude/tasks/midtown-<repo>/` on daemon startup. The daemon reads the old-format `DaemonPersistentState` (including all HashMap fields) one last time to populate Task structs, then drops the HashMap fields from serialized state going forward. Running sessions are unaffected — they are recorded in `SessionRecord` and resumed normally after restart. Field mapping:
  - `owner` → `agent_name` (existing owner value becomes the name; tasks without an owner get a name derived from the task subject via `slugify`)
  - `agent_type` defaults to `"midtown-code-author"` for migrated tasks without an explicit type in `task_agent_type`
  - Fields from `DaemonPersistentState` maps (`task_channel`, `task_model`, `task_plan`, `task_thread_id`, `task_message_id`, `task_parent`, `task_pr_number`) are merged onto the migrated task
  - `created_at` populated from file metadata, `updated_at` set to migration time

## Unified Launch Path

All session types launch through a single path. The five current constructors (`LaunchConfig::coworker()`, `::reviewer()`, `::resume_reviewer()`, `::lead()`, `::channel_lead()`) and the separate fork path (`build_fork_config()` / `create_fork_session()`) collapse into one constructor:

```rust
LaunchConfig::new(
    name: String,                        // Session name
    agent_type: String,                  // --agent flag value
    working_dir: PathBuf,                // Where to run
    initial_prompt: String,              // What to work on
    system_prompt_extra: Option<String>, // Additional context (domain notes, channel AGENTS.md, etc.)
)
```

Additional fields (`model`, `auth_provider`, `session_mode`, `task_id`, `pr_number`, `bound_thread_id`, `channel`) are set via builder methods or resolved internally:
- `model` and `auth_provider` are resolved from the `agent_type` via `ExecutionRole` → config lookup (see Config & Model Resolution)
- `session_mode` defaults to `Fresh`, overridden to `ResumeSession(id)` for session resumes
- `task_id`, `pr_number`, `bound_thread_id`, `channel` are set by the caller based on context

### What gets removed

- `CoworkerRole` enum — replaced by the `agent_type` string (which maps directly to `--agent`)
- `build_fork_config()` / `create_fork_session()` — forks go through the unified path
- `LaunchConfig::coworker()`, `::reviewer()`, `::resume_reviewer()`, `::lead()`, `::channel_lead()` — replaced by single constructor
- Avenue name pool: `AVENUE_NAMES`, `OVERFLOW_NAMES`, `next_available_name()`, `next_available_name_excluding()`
- `slugify_fork_hint()` — kept only as a fallback for forks without a lead-provided `--name`

### What stays but adapts

- `spawn_coworker()` becomes the single spawn entry point (renamed to `spawn_agent_session()` in the fast-follow)
- Worktree creation remains an upstream concern — callers pass in `working_dir`
- Session resume via `SessionMode::ResumeSession(id)` is orthogonal to this change

### Fork sessions

Fork sessions now go through the same launch path. The only structural difference is that a fork's `SessionRecord` has a `bound_thread_id`. Forks use `midtown-channel-lead` as their agent type (via `--agent`), replacing the current inline `system_prompt` approach. Domain context from channel notes and AGENTS.md content are passed via `system_prompt_extra`.

### Name resolution and uniqueness

1. **Workers:** `agent_name` from the task (required, set by lead at creation)
2. **Forks:** Lead's `--name` flag, with `slugify_fork_hint()` as fallback
3. **Channel leads:** Channel name

**Uniqueness is enforced at creation time.** For workers, `task.create` rejects the request if `agent_name` is already in use by another active task or session. For forks, `session.fork` rejects if the resolved name collides. This keeps task `agent_name` immutable and always exactly matching the session name — no suffix logic needed.

## SessionRecord Changes

### Replace
- `coworker_type: String` (`"dev"`, `"reviewer"`, `"channel-lead"`) → `agent_type: String` (stores actual agent definition, e.g., `"midtown-code-author"`)
- `is_reviewer: bool` → removed, redundant with `agent_type`
- `current_name` / `preferred_name` → single `name: String` (names are stable, no pool to allocate from)

**Note:** Code that matches on `coworker_type` string values (e.g., `is_fork_session()` checking `== "channel-lead"`) must be updated to match the new `agent_type` values (e.g., `== "midtown-channel-lead"`).

### Add
- `restart_count: u32` — moved from `DaemonPersistentState::task_restart_count`

### Keep as-is
- `session_id` — true identity
- `task_id` — denormalized for fast lookup without hitting task storage (bidirectional: task has `session_id`, session has `task_id`)
- `working_dir`, `branch`, `pr_number`, `initial_prompt`, `is_running`, `created_at`, `resume_on_startup`, `bound_thread_id`, `last_active`, `purpose`, `pid`, `channel`, `provider`, `platform`, `profile`

## Config & Model Resolution

Agent types map to `ExecutionRole`, which feeds into the existing model size (small/medium/large) and provider pool machinery. The `ExecutionRole` enum and all downstream config resolution remain unchanged.

**Built-in mappings:**
- `midtown-code-author` → `ExecutionRole::Coworker`
- `midtown-code-reviewer` → `ExecutionRole::Reviewer`
- `midtown-channel-lead` → `ExecutionRole::ChannelLead`
- `midtown-project-lead` → `ExecutionRole::Lead`

**User-defined agent types** specify their execution role in config, defaulting to `Coworker` if unspecified.

## CLI Changes

The `midtown task create` RPC and CLI command already support `--agent-type`, `--thread-id`, `--parent`, `--model`, and `--plan`. Changes needed:

**`task.create`:**
- Add `--agent-name` parameter (required for new tasks)
- Enforce uniqueness of `agent_name` against active tasks and sessions; reject with error on collision
- Remove `--execution-skill` parameter
- Write to `~/.midtown/<project>/tasks/` instead of `~/.claude/tasks/`
- Store all metadata on the Task struct directly instead of in separate `DaemonPersistentState` maps

**`task.update`:**
- Remove `--owner` parameter
- Add `--session-id`, `--message-id`, `--thread-id` as updatable fields
- `agent_name`, `agent_type`, and `parent` are not updatable (immutable after creation)

**Agent definitions:** The `midtown-project-lead` and `midtown-channel-lead` agent definitions must be updated to pass `--agent-name` when creating tasks via `midtown task create`.

## Terminology (Fast-Follow PR)

Rename `coworker` → `agent_session` throughout the codebase in a separate PR after the structural changes land:

- `CoworkerManager` → `AgentSessionManager`
- `spawn_coworker()` → `spawn_agent_session()`
- `coworker.rs` → `agent_session.rs`
- Workflow events: `coworker.idle` → `agent_session.idle`
- Web message types: `coworker_status` → `agent_session_status`

## Out of Scope

- Changes to the project lead (human-facing terminal session) — stays as-is with `midtown-project-lead`
- Claude Code's internal `TaskCreate`/`TaskUpdate` tools — agents continue using those independently for their own task tracking
- Web UI changes beyond adapting to new field names
