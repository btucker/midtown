# Simplify dispatch.rs and spawn effects

## Motivation

`dispatch.rs` (2526 lines) grew organically as session-based dispatch was added alongside older orphan recovery. The result is 6 near-identical spawn paths, 4 different spawn effect variants that do essentially the same thing, and duplicated helper logic. This refactoring consolidates without changing behavior.

## Effect consolidation

### Remove

- `SpawnCoworkerWithCallbacks` — used by orphan recovery + session dispatch + owned pending
- `AssignAndSpawn` — like above but writes task ownership first
- `SpawnSession` — defers name allocation to executor

### Add one

```rust
SpawnForTask {
    task_id: String,
    dir_key: String,
    preferred_name: Option<String>,
    config: LaunchConfig,
    on_success: Vec<Effect>,
    on_failure: Vec<Effect>,
}
```

The executor:
1. Picks `preferred_name` if available, otherwise allocates from the name pool
2. Writes task ownership to disk (what `AssignAndSpawn` did — now all spawns do this)
3. Spawns the coworker

Name allocation always happens at execution time in one place. Dispatch code never calls `next_available_name_excluding`.

### Keep as-is

- `NudgeSessionWithCallbacks` — nudging a running session is fundamentally different from spawning
- `TaskPrompt` — sending a message to an already-running session

## SpawnDecision

All dispatch paths produce the same normalized struct:

```rust
struct SpawnDecision {
    task_id: String,
    session_mode: SessionMode,      // Fresh or ResumeSession(id)
    preferred_name: Option<String>,  // hint: use this name if available
    cooldown_category: String,       // "orphan_spawn", "session_dispatch", etc.
}
```

Everything else is looked up from the snapshot at effect-build time:
- `task_subject` — from `snap.all_tasks`
- `channel` — from `snap.task_channel`
- `agent_type` — from `snap.task_agent_type_map`
- `model` — from `snap.task_model_map`
- `plan` — from `snap.task_plan_map`

One function builds a `SpawnForTask` effect from a `SpawnDecision` + snapshot. This replaces the 6 duplicated worktree-prep → config → spawn blocks.

## Dispatch consolidation

### Shared helpers to extract

- `compute_recently_stopped(snap) -> HashSet<String>` — duplicated verbatim in `check_and_recover_orphans_impl` and `reset_orphaned_tasks`
- `build_spawn_effects(decision, snap, dir_key) -> Vec<Effect>` — single spawn builder that handles worktree prep, config, model/channel/agent_type lookup, success/failure callbacks

### Merge prompts

Merge `coworker_recovery_prompt` into `coworker_task_prompt` in `agents.rs`. When `session_mode` is `ResumeSession`, append the "your previous session was interrupted" note. One prompt function, not two.

### Delete dead code

- `gather_discovered_coworker_nudges` — legacy no-op
- `decide_discovered_coworker_nudges` — test-only helper for the above
- `find_session_for_task` (test-only wrapper) — callers use `snap.find_session_for_task()` directly

## File structure

Keep `dispatch.rs` as a single file. The consolidation should reduce it to ~1800 lines — large but not unreasonable, and splitting into sub-modules adds indirection without enough benefit given the reduced duplication.

## What does NOT change

- The dispatch decision logic (which task to pick, grouping, priority ordering)
- `NudgeSessionWithCallbacks` / `TaskPrompt` effects
- The tick pipeline call order in `events.rs`
- `dispatch_priority.rs` (new module from the prior PR)
- Task data model on disk
- Session record structure (denormalization cleanup like `is_reviewer` is out of scope)
