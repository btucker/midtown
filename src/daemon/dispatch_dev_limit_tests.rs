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

/// When task limit is reached and all coworkers are busy, no dispatch should occur.
#[test]
fn test_no_dispatch_at_task_limit() {
    let state = make_test_state_with_max(8);

    let names = [
        "lexington",
        "park",
        "madison",
        "broadway",
        "amsterdam",
        "columbus",
        "riverside",
        "york",
    ];

    for name in &names {
        state
            .coworkers
            .insert_for_testing(make_running_coworker(name));
    }

    let running: Vec<_> = names.iter().map(|n| make_running_coworker(n)).collect();

    let pending = vec![make_pending_task("99")];

    // is_at_task_limit=true: 8 in-progress tasks >= max_in_progress_tasks=8
    // All coworkers are busy — no idle coworker reuse possible.
    let mut snap = make_task_limit_snapshot(running, pending, true);
    for name in &names {
        snap.busy_coworkers.insert(name.to_string());
    }

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
        "Expected no task dispatch when at task limit with all coworkers busy. Effects: {:?}",
        effects
    );
}

/// When at task limit, merged-PR auto-complete should still run.
/// The loop should continue past the task limit gate for cleanup operations.
#[test]
fn test_merged_pr_autocomplete_runs_at_task_limit() {
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

    // Create a pending task linked to a merged PR
    let mut merged_task = make_pending_task("42");
    merged_task.pr = Some(100);

    let pending = vec![merged_task];

    // At task limit (2/2), but merged-PR auto-complete should still fire
    let mut snap = make_task_limit_snapshot(running, pending, true);
    snap.pr.merged_pr_numbers.insert(100);

    let effects = crate::daemon::dispatch::spawn_for_pending_tasks(&snap, &state);

    let has_complete = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::CompleteTask { task_id, .. } if task_id == "42"
        )
    });
    assert!(
        has_complete,
        "Expected merged-PR auto-complete to run even at task limit. Effects: {:?}",
        effects
    );
}

/// When at task limit with idle coworkers, tasks should be assigned to idle coworkers.
#[test]
fn test_idle_coworker_reuse_at_task_limit() {
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

    let pending = vec![make_pending_task("99")];

    // At task limit (2/2), but park is idle (not in busy_coworkers)
    let mut snap = make_task_limit_snapshot(running, pending, true);
    // Mark lexington as busy, park stays idle
    snap.busy_coworkers.insert("lexington".to_string());
    // Don't mark park as busy — it's idle and should be reusable

    let effects = crate::daemon::dispatch::spawn_for_pending_tasks(&snap, &state);

    let has_nudge = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::NudgeSessionWithCallbacks { .. }
        )
    });
    assert!(
        has_nudge,
        "Expected idle coworker to be nudged with new task at task limit. Effects: {:?}",
        effects
    );
}

/// When at task limit with NO idle coworkers, tasks should be deferred.
#[test]
fn test_no_idle_coworkers_defers_at_task_limit() {
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

    let pending = vec![make_pending_task("99")];

    // At task limit (2/2), all coworkers busy
    let mut snap = make_task_limit_snapshot(running, pending, true);
    snap.busy_coworkers.insert("lexington".to_string());
    snap.busy_coworkers.insert("park".to_string());

    let effects = crate::daemon::dispatch::spawn_for_pending_tasks(&snap, &state);

    let has_task_effect = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::AssignAndSpawn { .. }
                | crate::daemon::effects::Effect::NudgeSessionWithCallbacks { .. }
        )
    });
    assert!(
        !has_task_effect,
        "Expected no dispatch when at task limit and all coworkers busy. Effects: {:?}",
        effects
    );
}

/// Reviewer tasks should NOT reuse idle coworkers — they need fresh isolated worktrees.
#[test]
fn test_reviewer_task_deferred_at_task_limit() {
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

    let mut reviewer_task = make_pending_task("99");
    reviewer_task.pr = Some(200);
    let pending = vec![reviewer_task];

    // At task limit (2/2), park is idle, but task is a reviewer task
    let mut snap = make_task_limit_snapshot(running, pending, true);
    snap.busy_coworkers.insert("lexington".to_string());
    // Mark task as reviewer type
    snap.task_agent_type_map
        .insert("99".to_string(), "midtown-code-reviewer".to_string());

    let effects = crate::daemon::dispatch::spawn_for_pending_tasks(&snap, &state);

    let has_task_effect = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::AssignAndSpawn { .. }
                | crate::daemon::effects::Effect::NudgeSessionWithCallbacks { .. }
        )
    });
    assert!(
        !has_task_effect,
        "Expected reviewer task to be deferred at task limit (no idle reuse). Effects: {:?}",
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
