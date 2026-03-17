# Dispatch Cleanup Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Consolidate 3 spawn effect variants into one `SpawnForTask`, extract shared helpers in dispatch.rs, merge prompt functions, and delete dead code.

**Architecture:** Add `SpawnForTask` effect that handles name allocation + task ownership + spawn in one place. Introduce `SpawnDecision` struct in dispatch.rs with a single `build_spawn_effects()` function. Migrate all 6 spawn paths to produce `SpawnDecision` values. Then remove the old effect variants.

**Tech Stack:** Rust

---

## Chunk 1: Add SpawnForTask effect + executor

### Task 1: Add SpawnForTask effect variant alongside existing ones

The new variant coexists with the old ones initially. No callers yet — we add those in later tasks.

**Files:**
- Modify: `src/daemon/effects.rs` (Effect enum, extract_claimed_task_ids, executor)

- [ ] **Step 1: Add the variant to the Effect enum**

In `src/daemon/effects.rs`, add after `AssignAndSpawn` (around line 354):

```rust
    /// Unified spawn effect for tasks.
    ///
    /// Allocates a coworker name (preferring `preferred_name` if available),
    /// writes task ownership + in_progress status to disk, then spawns.
    /// On success, executes `on_success` effects. On failure, resets the
    /// task to pending and executes `on_failure` effects.
    ///
    /// Replaces `SpawnCoworkerWithCallbacks`, `AssignAndSpawn`, and `SpawnSession`.
    SpawnForTask {
        task_id: String,
        dir_key: String,
        preferred_name: Option<String>,
        config: crate::launch::LaunchConfig,
        on_success: Vec<Effect>,
        on_failure: Vec<Effect>,
    },
```

- [ ] **Step 2: Update extract_claimed_task_ids_from_effects**

In `extract_claimed_task_ids_from_effects` (line ~795), add a match arm:

```rust
            Effect::SpawnForTask { task_id, .. } => {
                ids.insert(task_id.clone());
            }
```

- [ ] **Step 3: Add the executor arm**

In the `execute_effects` function, add a handler for `SpawnForTask`. This combines the logic from `AssignAndSpawn` (task ownership, in_progress) and `SpawnSession` (name allocation, displaced session cleanup, session record, thread binding). The full executor logic:

1. Allocate name from pool (prefer `preferred_name`)
2. Update config with allocated name
3. Spawn via `state.spawn_coworker(&config)`
4. On success: update session record task_id, set task owner on disk, transition to in_progress, post DM separator, execute `on_success` callbacks
5. On failure: clear in-flight, execute `on_failure` callbacks

Model this closely on the existing `AssignAndSpawn` executor (lines 1727-1797) but with the name allocation from `SpawnSession` (lines 3002-3016). For the session record update, use the same pattern as `AssignAndSpawn` lines 1743-1761 (look up session by name, set task_id).

For displaced session cleanup (from `SpawnSession` lines 3056-3071), include it when the allocated name differs from `preferred_name` — this handles cases where the preferred name was taken and a different name was allocated.

- [ ] **Step 4: Verify compilation**

```bash
cargo check 2>&1 | tail -5
```

Expected: compiles (the new variant is unused but that's OK — no warnings since it has fields).

- [ ] **Step 5: Commit**

```bash
git add src/daemon/effects.rs
git commit -m "feat: add SpawnForTask unified spawn effect variant"
```

### Task 2: Add SpawnDecision and build_spawn_effects in dispatch.rs

**Files:**
- Modify: `src/daemon/dispatch.rs`

- [ ] **Step 1: Add SpawnDecision struct and build function**

Near the top of `src/daemon/dispatch.rs` (after the existing helpers around line 80), add:

```rust
/// A normalized spawn decision. All dispatch paths produce this struct;
/// `build_spawn_effects` converts it to effects.
struct SpawnDecision {
    task_id: String,
    session_mode: crate::launch::SessionMode,
    preferred_name: Option<String>,
    cooldown_category: String,
}

/// Convert a SpawnDecision into spawn effects by looking up all task
/// metadata from the snapshot.
fn build_spawn_effects(
    decision: &SpawnDecision,
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    // Look up task metadata from snapshot
    let task = snap
        .all_tasks
        .iter()
        .find(|t| t.id == decision.task_id);
    let task_subject = task.map(|t| t.subject.as_str()).unwrap_or("(unknown)");

    let channel = snap.task_channel.get(&decision.task_id).cloned()
        .or_else(|| task.and_then(|t| t.channel.clone()));
    let agent_type = snap.task_agent_type_map.get(&decision.task_id).cloned();

    // Build prompt
    let plan_section = build_plan_prompt_section(&decision.task_id, snap);
    let is_resume = matches!(decision.session_mode, crate::launch::SessionMode::ResumeSession(_));
    let prompt = if is_resume {
        crate::agents::coworker_recovery_prompt(&decision.task_id, task_subject, &plan_section)
    } else {
        crate::agents::coworker_task_prompt(&decision.task_id, task_subject, &plan_section)
    };

    // Prepare worktree
    let wt = prepare_task_worktree(
        &decision.task_id,
        task_subject,
        state.paths.dir_key(),
        snap,
    );

    // Build launch config
    let mut config = crate::launch::LaunchConfig::coworker(
        String::new(), // name allocated at execution time
        state.paths.dir_key().to_string(),
        decision.session_mode.clone(),
        Some(prompt),
        Some(decision.task_id.clone()),
    );
    config.working_dir = Some(wt.path);
    config.channel = channel;
    config.apply_task_model(&snap.task_model_map, &decision.task_id);
    if let Some(ref at) = agent_type {
        config.agent_name_override = Some(at.clone());
    }

    // Build success/failure callbacks
    let mut on_success = spawn_success_effects(
        String::new(), // placeholder — executor fills in after name allocation
        decision.task_id.clone(),
        wt.worktree_id,
        format!(
            "Spawned coworker for task !{} ({})",
            decision.task_id, task_subject
        ),
    );
    on_success.push(effects::Effect::RecordCooldown {
        category: decision.cooldown_category.clone(),
        key: "global".to_string(),
    });

    let on_failure = spawn_failure_effects(
        String::new(), // placeholder
        decision.task_id.clone(),
        state.paths.dir_key().to_string(),
        format!(
            "Task !{} reset to pending — could not spawn (backing off for {}s)",
            decision.task_id,
            SPAWN_FAILURE_COOLDOWN.as_secs()
        ),
    );

    // Combine: worktree pre-spawn + spawn effect
    let mut all_effects = wt.pre_spawn_effects;
    all_effects.push(effects::Effect::SpawnForTask {
        task_id: decision.task_id.clone(),
        dir_key: state.paths.dir_key().to_string(),
        preferred_name: decision.preferred_name.clone(),
        config,
        on_success,
        on_failure,
    });
    all_effects
}
```

Note: The `agent_name_override` field on `LaunchConfig` may not exist yet. Check if it does; if not, the agent_type can be applied via `config.role` or a new field. The implementer should read `LaunchConfig` to find the right mechanism.

- [ ] **Step 2: Add compute_recently_stopped helper**

Extract the duplicated recently-stopped computation into a helper:

```rust
/// Compute the set of coworker names that stopped within the grace period.
fn compute_recently_stopped(snap: &snapshot::WorldSnapshot) -> HashSet<String> {
    let grace_period = chrono::Duration::seconds(ORPHAN_RECOVERY_GRACE_PERIOD.as_secs() as i64);
    snap.coworkers
        .coworker_stop_times
        .iter()
        .filter(|(_, stop_time)| snap.now_utc.signed_duration_since(**stop_time) < grace_period)
        .map(|(name, _)| name.clone())
        .collect()
}
```

- [ ] **Step 3: Verify compilation**

```bash
cargo check 2>&1 | tail -5
```

- [ ] **Step 4: Commit**

```bash
git add src/daemon/dispatch.rs
git commit -m "feat: add SpawnDecision struct and build_spawn_effects helper"
```

## Chunk 2: Migrate spawn paths to SpawnDecision

### Task 3: Migrate check_and_recover_orphans to SpawnDecision

**Files:**
- Modify: `src/daemon/dispatch.rs` (`check_and_recover_orphans_impl`, lines ~433-662)

This function currently has TWO spawn paths (session resume at ~546-599, fresh spawn at ~604-661). Both should produce a `SpawnDecision` and call `build_spawn_effects`.

- [ ] **Step 1: Replace the session-resume spawn block**

Find the session-resume path (around lines 546-599, starting with `let session_record = snap.find_session_for_task`). Replace the worktree-prep → config → spawn block with:

```rust
    let session_record = snap.find_session_for_task(&recovery.task_id);
    let (session_mode, preferred_name) = if let Some(record) = session_record
        && !record.is_running
        && !record.is_reviewer
    {
        (
            crate::launch::SessionMode::ResumeSession(record.session_id.clone()),
            record.preferred_name.clone(),
        )
    } else {
        (crate::launch::SessionMode::Fresh, Some(recovery.owner.clone()))
    };

    let decision = SpawnDecision {
        task_id: recovery.task_id.clone(),
        session_mode,
        preferred_name,
        cooldown_category: "orphan_spawn".to_string(),
    };
    return build_spawn_effects(&decision, snap, state);
```

This replaces BOTH the session-resume and fresh-spawn paths with a single decision.

- [ ] **Step 2: Delete the old fresh-spawn path**

The code after the session-resume block (lines ~604-661, the "Fresh spawn path") is now dead code. Delete it.

- [ ] **Step 3: Replace the inline recently_stopped computation**

Replace the inline computation (lines ~477-484) with a call to `compute_recently_stopped(snap)`.

- [ ] **Step 4: Run tests**

```bash
cargo test --lib -- check_and_recover_orphans 2>&1 | tail -20
```

Fix any failures — the tests construct specific effect types that may need updating.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/dispatch.rs
git commit -m "refactor: migrate orphan recovery to SpawnDecision"
```

### Task 4: Migrate dispatch_via_sessions to SpawnDecision

**Files:**
- Modify: `src/daemon/dispatch.rs` (`dispatch_via_sessions_inner`, lines ~705-949)

The session dispatch loop has one spawn path for stopped sessions (~838-941).

- [ ] **Step 1: Replace the stopped-session spawn block**

Find the block that builds a `SpawnCoworkerWithCallbacks` for stopped sessions. Replace with:

```rust
    let decision = SpawnDecision {
        task_id: task_id.clone(),
        session_mode: crate::launch::SessionMode::ResumeSession(record.session_id.clone()),
        preferred_name: record.preferred_name.clone(),
        cooldown_category: "session_dispatch".to_string(),
    };
    effects.extend(build_spawn_effects(&decision, snap, state));
    break; // one spawn per tick
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib -- dispatch_via_sessions 2>&1 | tail -20
```

- [ ] **Step 3: Commit**

```bash
git add src/daemon/dispatch.rs
git commit -m "refactor: migrate session dispatch to SpawnDecision"
```

### Task 5: Migrate dispatch_owned_pending_tasks to SpawnDecision

**Files:**
- Modify: `src/daemon/dispatch.rs` (`dispatch_owned_pending_tasks`, lines ~1276-1484)

This function has one spawn path for owned pending tasks (~1426-1473).

- [ ] **Step 1: Replace the spawn block**

Find the `SpawnCoworkerWithCallbacks` construction. Replace with:

```rust
    let decision = SpawnDecision {
        task_id: tid.clone(),
        session_mode: crate::launch::SessionMode::Fresh,
        preferred_name: Some(owner_name.clone()),
        cooldown_category: "task_dispatch".to_string(),
    };
    effects.extend(build_spawn_effects(&decision, snap, state));
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib -- dispatch_owned 2>&1 | tail -20
```

- [ ] **Step 3: Commit**

```bash
git add src/daemon/dispatch.rs
git commit -m "refactor: migrate owned pending dispatch to SpawnDecision"
```

### Task 6: Migrate dispatch_unowned_pending_tasks to SpawnDecision

**Files:**
- Modify: `src/daemon/dispatch.rs` (`dispatch_unowned_pending_tasks`, lines ~1544-2161)

This is the largest function (618 lines) with THREE spawn paths:
1. Session resume (~1689-1747, `SpawnSession`)
2. Reviewer spawn (~1951-2078, `AssignAndSpawn`)
3. Regular task spawn (~2080-2156, `AssignAndSpawn`)

- [ ] **Step 1: Replace session resume path**

Find the `SpawnSession` effect construction. Replace with a `SpawnDecision`:

```rust
    let decision = SpawnDecision {
        task_id: task.id.clone(),
        session_mode: crate::launch::SessionMode::ResumeSession(session_id.clone()),
        preferred_name: preferred_name.clone(),
        cooldown_category: "session_dispatch".to_string(),
    };
    effects.extend(build_spawn_effects(&decision, snap, state));
```

- [ ] **Step 2: Replace reviewer spawn path**

Find the reviewer `AssignAndSpawn` construction. Replace with:

```rust
    let decision = SpawnDecision {
        task_id: task.id.clone(),
        session_mode: crate::launch::SessionMode::Fresh,
        preferred_name: None,
        cooldown_category: "task_dispatch".to_string(),
    };
    effects.extend(build_spawn_effects(&decision, snap, state));
```

Note: reviewer-specific effects (like `CreateTaskSessionSpan` with `agent_type: "reviewer"`) may need to be preserved in the `on_success` callbacks. Check what the reviewer path currently includes beyond the spawn and ensure `build_spawn_effects` handles it, or add reviewer-specific callbacks after.

- [ ] **Step 3: Replace regular task spawn path**

Same pattern as reviewer but without reviewer-specific effects.

- [ ] **Step 4: Run tests**

```bash
cargo test --lib -- dispatch_unowned 2>&1 | tail -20
cargo test --lib 2>&1 | tail -10
```

- [ ] **Step 5: Commit**

```bash
git add src/daemon/dispatch.rs
git commit -m "refactor: migrate unowned pending dispatch to SpawnDecision"
```

## Chunk 3: Remove old effects + cleanup

### Task 7: Remove old effect variants and their executors

**Files:**
- Modify: `src/daemon/effects.rs`

Now that all callers use `SpawnForTask`, remove the old variants.

- [ ] **Step 1: Remove SpawnCoworkerWithCallbacks variant and executor**

Delete the variant from the `Effect` enum and its handler from `execute_effects`. Update `extract_claimed_task_ids_from_effects` to remove the match arm.

- [ ] **Step 2: Remove AssignAndSpawn variant and executor**

Same treatment.

- [ ] **Step 3: Remove SpawnSession variant and executor**

Same treatment.

- [ ] **Step 4: Verify compilation and run full test suite**

```bash
cargo check 2>&1 | tail -5
cargo test --lib 2>&1 | tail -10
```

Any test that constructs old effect types will fail. Fix by converting to `SpawnForTask`.

- [ ] **Step 5: Check for references in other files**

Search for `SpawnCoworkerWithCallbacks`, `AssignAndSpawn`, `SpawnSession` across the codebase. Update any remaining references (pr.rs reviewer spawning, test files, etc.).

```bash
grep -r "SpawnCoworkerWithCallbacks\|AssignAndSpawn\|SpawnSession" --include='*.rs' src/ tests/
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: remove SpawnCoworkerWithCallbacks, AssignAndSpawn, SpawnSession effect variants"
```

### Task 8: Merge prompt functions and delete dead code

**Files:**
- Modify: `src/agents.rs` (merge prompts)
- Modify: `src/daemon/dispatch.rs` (delete dead code)

- [ ] **Step 1: Merge coworker_recovery_prompt into coworker_task_prompt**

In `src/agents.rs`, modify `coworker_task_prompt` to accept an optional `is_resume: bool` parameter. When true, include the "your previous session was interrupted" sentence. Then delete `coworker_recovery_prompt`.

Update all callers (should just be `build_spawn_effects` at this point).

- [ ] **Step 2: Delete dead code in dispatch.rs**

- `gather_discovered_coworker_nudges` (legacy no-op, line ~960)
- `decide_discovered_coworker_nudges` (test-only helper, line ~973)
- `find_session_for_task` (test-only wrapper, line ~301) — callers should use `snap.find_session_for_task()` directly

- [ ] **Step 3: Replace duplicated recently_stopped in reset_orphaned_tasks**

In `reset_orphaned_tasks` (line ~2414), replace the inline recently_stopped computation with `compute_recently_stopped(snap)`.

- [ ] **Step 4: Run full test suite + clippy**

```bash
cargo test --lib 2>&1 | tail -10
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -10
```

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: merge prompts, delete dead code, extract compute_recently_stopped"
```

### Task 9: Final verification

- [ ] **Step 1: Run full test suite**

```bash
cargo test 2>&1 | tail -10
```

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -10
```

- [ ] **Step 3: Run fmt**

```bash
cargo fmt --all -- --check
```

- [ ] **Step 4: Check line count reduction**

```bash
wc -l src/daemon/dispatch.rs src/daemon/effects.rs
```

Expected: dispatch.rs should be noticeably smaller (target: ~1800 lines, down from 2526).

- [ ] **Step 5: Commit any cleanup**

```bash
git add -A
git commit -m "chore: final cleanup after dispatch consolidation"
```
