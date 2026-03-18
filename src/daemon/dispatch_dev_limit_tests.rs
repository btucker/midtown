//! Tests for task-based limit enforcement in task dispatch.
//!
//! These tests verify that the task limit (max_in_progress_tasks) correctly
//! governs dispatch decisions. The old coworker-count-based limits
//! (REVIEW_HEADROOM, is_at_dev_limit, is_at_coworker_limit) have been
//! replaced by a single task-count-based limit.

#[test]
fn test_task_limit_semantics() {
    // max_in_progress_tasks is a single limit shared by all task types.
    // No separate dev/reviewer caps or REVIEW_HEADROOM.
    let max_in_progress_tasks: usize = 8;
    let in_progress_count: usize = 7;

    let is_at_limit = in_progress_count >= max_in_progress_tasks;
    assert!(!is_at_limit, "7 < 8 → not at limit");

    let in_progress_count: usize = 8;
    let is_at_limit = in_progress_count >= max_in_progress_tasks;
    assert!(is_at_limit, "8 >= 8 → at limit");
}

#[test]
fn test_spawn_count_within_tick() {
    // This test documents the expected behavior for spawn limiting within a tick.
    //
    // Scenario: 7 in-progress tasks, 3 pending unowned tasks.
    // With max_in_progress_tasks=8:
    // - Snapshot shows is_at_task_limit = false (7 < 8)
    // - Loop processes tasks one by one
    //
    // Without per-spawn counter: Loop checks is_at_task_limit ONCE, spawns all 3 tasks.
    // Result: 7 + 3 = 10 in-progress, exceeding limit of 8.
    //
    // With per-spawn counter: after each spawn decision, re-check:
    //   - spawns_this_tick = 0
    //   - Process task 1: spawns_this_tick = 1, total = 8 (at cap, STOP)
    //   - Tasks 2 and 3 deferred to next tick

    let in_progress_count = 7;
    let pending_count = 3;
    let task_cap = 8;

    // Without per-spawn counter: all tasks spawn
    let spawned_without_counter = pending_count; // 3
    let total_without_counter = in_progress_count + spawned_without_counter; // 10
    assert!(
        total_without_counter > task_cap,
        "Bug: spawning exceeds task limit"
    );

    // With per-spawn counter: only spawn until cap
    let spawned_with_counter = (task_cap - in_progress_count).min(pending_count); // 1
    let total_with_counter = in_progress_count + spawned_with_counter; // 8
    assert_eq!(
        total_with_counter, task_cap,
        "Fix: spawning stops at task limit"
    );
}

#[test]
fn test_spawn_limit_edge_cases() {
    let task_cap: usize = 8;

    // Edge case 1: Already at cap
    let in_progress = 8;
    let allowed = task_cap.saturating_sub(in_progress);
    assert_eq!(allowed, 0, "No spawns allowed when at cap");

    // Edge case 2: One below cap
    let in_progress = 7;
    let allowed = task_cap.saturating_sub(in_progress);
    assert_eq!(allowed, 1, "Exactly 1 spawn allowed when 1 below cap");

    // Edge case 3: Empty (no in-progress tasks)
    let in_progress = 0;
    let allowed = task_cap.saturating_sub(in_progress);
    assert_eq!(
        allowed, 8,
        "Up to task_cap spawns allowed when starting from 0"
    );
}

/// Build a minimal running Coworker for testing.
fn make_running_coworker(name: &str) -> crate::coworker::Coworker {
    crate::coworker::Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        status: crate::coworker::CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: "default".to_string(),
    }
}

/// Build a minimal pending task for testing.
fn make_pending_task(id: &str) -> crate::tasks::Task {
    crate::tasks::Task {
        id: id.to_string(),
        subject: format!("Task {}", id),
        status: crate::tasks::TaskStatus::Pending,
        owner: None,
        blocked_by: vec![],
        description: None,
        channel: None,
        pr: None,
        created_at: Some(std::time::SystemTime::now()),
    }
}

/// Build a minimal WorldSnapshot for task limit tests.
///
/// When `is_at_task_limit` is true, populates `in_progress_tasks` from running
/// coworkers and sets `max_in_progress_tasks` equal to that count, so the
/// per-spawn counter in `dispatch_unowned_pending_tasks` agrees with the flag.
/// When false, leaves `in_progress_tasks` empty and uses the default cap.
fn make_task_limit_snapshot(
    running: Vec<crate::coworker::Coworker>,
    pending_tasks: Vec<crate::tasks::Task>,
    is_at_task_limit: bool,
) -> crate::daemon::snapshot::WorldSnapshot {
    let active_names: std::collections::HashSet<String> =
        running.iter().map(|cw| cw.name.to_lowercase()).collect();
    let mut snap = crate::daemon::snapshot::minimal_snapshot_for_test();
    snap.coworkers.running_coworkers = running.clone();
    snap.coworkers.active_names = active_names;
    snap.is_at_task_limit = is_at_task_limit;
    if is_at_task_limit {
        // Populate in_progress_tasks so per-spawn counter also blocks dispatch.
        let in_progress_tasks: Vec<(String, String, String)> = running
            .iter()
            .enumerate()
            .map(|(i, cw)| (format!("{}", i), format!("Task {}", i), cw.name.clone()))
            .collect();
        snap.max_in_progress_tasks = in_progress_tasks.len();
        snap.in_progress_tasks = in_progress_tasks;
    }
    snap.pending_tasks_without_owners = pending_tasks;
    snap
}

/// Make a test DaemonState with the given max_in_progress_tasks setting.
fn make_test_state_with_max(max_in_progress_tasks: usize) -> crate::daemon::DaemonState {
    use std::process::Command;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("temp dir");
    Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git commit");

    let wm = crate::worktree::WorktreeManager::new(temp_dir.path().to_path_buf()).expect("wm");
    let cm = crate::coworker::CoworkerManager::new(wm);

    let base_dir = temp_dir.path().to_path_buf();
    std::mem::forget(temp_dir);

    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    crate::daemon::DaemonState::new(
        "/tmp/test.sock".into(),
        cm,
        crate::paths::ProjectPaths::with_project_name("test-repo", "test-repo"),
        vec![base_dir.clone()],
        channel_router,
        None,
        max_in_progress_tasks,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state")
}

/// When the lead is running, dispatch should still work if task limit is not hit.
///
/// The lead session name is the repo name ("test-repo"), not "lead".
/// Previously this tested coworker-count exclusion. Now we test that
/// the task-count-based limit works regardless of which coworkers are running.
#[test]
fn test_lead_does_not_affect_task_limit_dispatch() {
    let state = make_test_state_with_max(8);

    // Register 7 real coworkers in CoworkerManager so next_available_name()
    // skips them and returns the 8th available name for the new task.
    for name in &[
        "lexington",
        "park",
        "madison",
        "broadway",
        "amsterdam",
        "columbus",
        "riverside",
    ] {
        state
            .coworkers
            .insert_for_testing(make_running_coworker(name));
    }

    // 7 real coworkers + 1 headless lead = 8 total in running_coworkers
    let running = vec![
        make_running_coworker("test-repo"),
        make_running_coworker("lexington"),
        make_running_coworker("park"),
        make_running_coworker("madison"),
        make_running_coworker("broadway"),
        make_running_coworker("amsterdam"),
        make_running_coworker("columbus"),
        make_running_coworker("riverside"),
    ];

    let pending = vec![make_pending_task("99")];

    // Task limit is based on in-progress task count, not coworker count.
    // is_at_task_limit=false means we're below the task limit.
    let snap = make_task_limit_snapshot(running, pending, false);

    let effects = crate::daemon::dispatch::spawn_for_pending_tasks(&snap, &state);

    let has_task_effect = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::AssignAndSpawn { .. }
                | crate::daemon::effects::Effect::SpawnCoworkerWithCallbacks { .. }
                | crate::daemon::effects::Effect::NudgeSessionWithCallbacks { .. }
        )
    });
    assert!(
        has_task_effect,
        "Expected a task dispatch effect when below task limit. Effects: {:?}",
        effects
    );
}

/// When task limit is reached, no dispatch should occur.
#[test]
fn test_no_dispatch_at_task_limit() {
    let state = make_test_state_with_max(8);

    for name in &[
        "lexington",
        "park",
        "madison",
        "broadway",
        "amsterdam",
        "columbus",
        "riverside",
        "york",
    ] {
        state
            .coworkers
            .insert_for_testing(make_running_coworker(name));
    }

    let running = vec![
        make_running_coworker("lexington"),
        make_running_coworker("park"),
        make_running_coworker("madison"),
        make_running_coworker("broadway"),
        make_running_coworker("amsterdam"),
        make_running_coworker("columbus"),
        make_running_coworker("riverside"),
        make_running_coworker("york"),
    ];

    let pending = vec![make_pending_task("99")];

    // is_at_task_limit=true: 8 in-progress tasks >= max_in_progress_tasks=8
    let snap = make_task_limit_snapshot(running, pending, true);

    let effects = crate::daemon::dispatch::spawn_for_pending_tasks(&snap, &state);

    let has_task_effect = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::AssignAndSpawn { .. }
                | crate::daemon::effects::Effect::SpawnCoworkerWithCallbacks { .. }
                | crate::daemon::effects::Effect::NudgeSessionWithCallbacks { .. }
        )
    });
    assert!(
        !has_task_effect,
        "Expected no task dispatch when at task limit. Effects: {:?}",
        effects
    );
}

/// When a legacy "lead" session is running, dispatch should work normally
/// based on task count, not coworker count.
#[test]
fn test_dispatch_with_legacy_lead() {
    let state = make_test_state_with_max(3);

    // Register 2 real coworkers in CoworkerManager
    for name in &["york", "madison"] {
        state
            .coworkers
            .insert_for_testing(make_running_coworker(name));
    }

    // 3 sessions in the snapshot: "lead" (legacy) + 2 devs
    let running = vec![
        make_running_coworker("lead"),
        make_running_coworker("york"),
        make_running_coworker("madison"),
    ];

    let pending = vec![make_pending_task("99")];

    // is_at_task_limit=false: task limit governs, not coworker count
    let snap = make_task_limit_snapshot(running, pending, false);

    let effects = crate::daemon::dispatch::spawn_for_pending_tasks(&snap, &state);

    let has_task_effect = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::AssignAndSpawn { .. }
                | crate::daemon::effects::Effect::SpawnCoworkerWithCallbacks { .. }
                | crate::daemon::effects::Effect::NudgeSessionWithCallbacks { .. }
        )
    });
    assert!(
        has_task_effect,
        "Expected task dispatch: task limit not reached. Effects: {:?}",
        effects
    );
}

/// Merged-PR auto-complete should emit CompleteTask even when at task limit.
///
/// Before the fix, the loop `break`ed at the task limit gate, preventing
/// merged-PR auto-complete from running for any tasks beyond the limit.
#[test]
fn test_merged_pr_autocomplete_runs_at_capacity() {
    let state = make_test_state_with_max(2);

    for name in &["lexington", "park"] {
        state
            .coworkers
            .insert_for_testing(make_running_coworker(name));
    }

    let running = vec![
        make_running_coworker("lexington"),
        make_running_coworker("park"),
    ];

    // Task 99 is a regular pending task (will be deferred at capacity).
    // Task 100 has pr=Some(42) and merged_pr_numbers contains 42,
    // so it should be auto-completed regardless of capacity.
    let mut task_100 = make_pending_task("100");
    task_100.pr = Some(42);

    let pending = vec![make_pending_task("99"), task_100];

    let mut snap = make_task_limit_snapshot(running, pending, true);
    snap.pr.merged_pr_numbers.insert(42);

    let effects = crate::daemon::dispatch::spawn_for_pending_tasks(&snap, &state);

    let has_complete = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::CompleteTask { task_id, .. } if task_id == "100"
        )
    });
    assert!(
        has_complete,
        "Expected CompleteTask for merged-PR task even at capacity. Effects: {:?}",
        effects
    );

    // Should NOT have any spawn/nudge effects (at capacity for non-cleanup work).
    let has_spawn = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::AssignAndSpawn { .. }
                | crate::daemon::effects::Effect::SpawnCoworkerWithCallbacks { .. }
                | crate::daemon::effects::Effect::NudgeSessionWithCallbacks { .. }
        )
    });
    assert!(
        !has_spawn,
        "Should not spawn/nudge when at task limit. Effects: {:?}",
        effects
    );
}

/// When all coworker name slots are exhausted but an idle coworker exists,
/// the loop should reuse the idle coworker via nudge instead of breaking.
#[test]
fn test_idle_coworker_reuse_when_no_fresh_slots() {
    use crate::coworker::{AVENUE_NAMES, OVERFLOW_NAMES};

    // All 16 names are active. "park" is idle (not busy).
    let all_names: Vec<&str> = AVENUE_NAMES
        .iter()
        .chain(OVERFLOW_NAMES.iter())
        .copied()
        .collect();
    let state = make_test_state_with_max(20); // high limit so task cap isn't the blocker

    // Register all names in CoworkerManager so next_available_name_excluding returns None.
    for name in &all_names {
        state
            .coworkers
            .insert_for_testing(make_running_coworker(name));
    }

    let running: Vec<_> = all_names.iter().map(|n| make_running_coworker(n)).collect();
    let pending = vec![make_pending_task("99")];

    let mut snap = make_task_limit_snapshot(running, pending, false);
    // Only 2 in-progress tasks (well below task_cap=20).
    snap.in_progress_tasks = vec![
        (
            "1".to_string(),
            "Task 1".to_string(),
            "lexington".to_string(),
        ),
        ("2".to_string(), "Task 2".to_string(), "madison".to_string()),
    ];
    snap.max_in_progress_tasks = 20;
    // All names are active (in the snapshot).
    snap.coworkers.active_names = all_names.iter().map(|s| s.to_string()).collect();
    // park is active but NOT busy — it's idle.
    snap.busy_coworkers = all_names
        .iter()
        .filter(|n| **n != "park")
        .map(|s| s.to_string())
        .collect();
    // Need name_session_map for the nudge effect.
    snap.name_session_map
        .insert("park".to_string(), "session-park".to_string());

    let effects = crate::daemon::dispatch::spawn_for_pending_tasks(&snap, &state);

    let has_nudge = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::NudgeSessionWithCallbacks { session_id, .. }
                if session_id == "session-park"
        )
    });
    assert!(
        has_nudge,
        "Expected NudgeSessionWithCallbacks for idle coworker 'park'. Effects: {:?}",
        effects
    );
}

/// When at capacity AND no idle coworkers, should not spawn but should still
/// process remaining tasks for cleanup (merged-PR auto-complete, etc.).
#[test]
fn test_no_dispatch_when_capacity_and_no_idle() {
    let state = make_test_state_with_max(2);

    for name in &["lexington", "park"] {
        state
            .coworkers
            .insert_for_testing(make_running_coworker(name));
    }

    let running = vec![
        make_running_coworker("lexington"),
        make_running_coworker("park"),
    ];

    // Two pending tasks. First is a regular task (deferred at capacity).
    // Second has a merged PR (should be auto-completed).
    let mut task_200 = make_pending_task("200");
    task_200.pr = Some(55);

    let pending = vec![make_pending_task("199"), task_200];

    let mut snap = make_task_limit_snapshot(running, pending, true);
    snap.pr.merged_pr_numbers.insert(55);

    let effects = crate::daemon::dispatch::spawn_for_pending_tasks(&snap, &state);

    // No spawn effects.
    let has_spawn = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::AssignAndSpawn { .. }
                | crate::daemon::effects::Effect::SpawnCoworkerWithCallbacks { .. }
                | crate::daemon::effects::Effect::NudgeSessionWithCallbacks { .. }
        )
    });
    assert!(
        !has_spawn,
        "Should not spawn when at capacity with no idle coworkers. Effects: {:?}",
        effects
    );

    // But merged-PR task should still be completed.
    let has_complete = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::CompleteTask { task_id, .. } if task_id == "200"
        )
    });
    assert!(
        has_complete,
        "Merged-PR auto-complete should work even at capacity. Effects: {:?}",
        effects
    );
}
