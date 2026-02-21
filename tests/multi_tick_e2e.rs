//! E2E tests for multi-tick daemon behavior.
//!
//! These tests verify that daemon decision functions don't produce duplicate
//! effects across multiple ticks. Common bugs caught by these tests:
//!
//! - Task dispatched on tick 1 → re-dispatched on tick 2 (duplicate spawn)
//! - Reviewer spawned on tick 1 → re-spawned on tick 2 (double spawn)
//! - Orphan recovered on tick 1 → re-recovered on tick 2
//! - Merge task created on tick 1 → duplicated on tick 2
//!
//! Run with: `cargo test --test multi_tick_e2e`

mod multi_tick_harness;

use midtown::daemon::{DaemonEvent, Effect};
use midtown::launch::SessionMode;
use multi_tick_harness::MultiTickHarness;
use serde_json::json;

/// Test that reset_orphaned_tasks doesn't produce duplicate resets.
///
/// Bug scenario: Task is orphaned (owner not running) → reset on tick 1.
/// On tick 2, the same task shouldn't be reset again.
#[test]
fn test_no_duplicate_orphan_resets() {
    let fixture = include_str!(
        "fixtures/snapshot/snapshot-tool-names-must-be-unique-all-stuck-20260211-030435.json"
    );
    let mut harness = MultiTickHarness::from_json(fixture).unwrap();

    // Tick 1: Reset orphaned tasks
    let effects1 = harness.tick(&DaemonEvent::TaskDispatchTick);

    let reset_count_1 = effects1
        .iter()
        .filter(|e| matches!(e, Effect::ResetTaskToPending { .. }))
        .count();

    println!("Tick 1: {} reset effects", reset_count_1);

    // Tick 2: Should not produce duplicate resets
    let effects2 = harness.tick(&DaemonEvent::TaskDispatchTick);

    let reset_count_2 = effects2
        .iter()
        .filter(|e| matches!(e, Effect::ResetTaskToPending { .. }))
        .count();

    println!("Tick 2: {} reset effects", reset_count_2);

    assert_eq!(
        reset_count_2, 0,
        "Tick 2 should not re-reset tasks that were already reset in tick 1"
    );
}

/// Test that idle shutdown doesn't fire repeatedly for the same coworker.
///
/// Bug scenario: Coworker is idle → shutdown on tick 1.
/// On tick 2, the same coworker shouldn't be shut down again.
#[test]
fn test_no_duplicate_idle_shutdowns() {
    let fixture =
        include_str!("fixtures/snapshot/snapshot-reviewer-not-spawning-20260214-003545.json");
    let mut harness = MultiTickHarness::from_json(fixture).unwrap();

    // Tick 1: Check for idle coworkers
    let effects1 = harness.tick(&DaemonEvent::SessionMonitorTick);

    let shutdown_count_1 = effects1
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::ShutdownCoworker { .. } | Effect::ShutdownCoworkerWithCallbacks { .. }
            )
        })
        .count();

    println!("Tick 1: {} shutdown effects", shutdown_count_1);

    // Tick 2: Should not re-shutdown the same coworkers
    let effects2 = harness.tick(&DaemonEvent::SessionMonitorTick);

    let shutdown_count_2 = effects2
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::ShutdownCoworker { .. } | Effect::ShutdownCoworkerWithCallbacks { .. }
            )
        })
        .count();

    println!("Tick 2: {} shutdown effects", shutdown_count_2);

    assert_eq!(
        shutdown_count_2, 0,
        "Tick 2 should not re-shutdown coworkers that were already shut down in tick 1"
    );
}

/// Test that merged PR cleanup doesn't repeat across ticks.
///
/// Bug scenario: PR is merged → cleanup on tick 1.
/// On tick 2, the same PR shouldn't be cleaned up again.
///
/// Uses snapshot mutation to inject a merged PR with a matching branch entry,
/// since `collect_merged_pr_cleanup_effects` requires both `merged_pr_numbers`
/// and `merged_pr_branches` to produce `CleanupMergedWorktree` effects.
#[test]
fn test_no_duplicate_pr_cleanup() {
    let fixture =
        include_str!("fixtures/snapshot/snapshot-reviewer-not-spawning-20260214-003545.json");
    let mut harness = MultiTickHarness::from_json(fixture).unwrap();

    // Inject a merged PR with a matching branch entry so
    // collect_merged_pr_cleanup_effects produces effects
    harness.snapshot_mut().merged_pr_numbers.insert(9999);
    harness
        .snapshot_mut()
        .merged_pr_branches
        .insert(9999, "test-branch".to_string());

    // Tick 1: Collect merged PR cleanup effects
    let effects1 = harness.tick(&DaemonEvent::PrPollTick);

    let cleanup_count_1 = effects1
        .iter()
        .filter(|e| matches!(e, Effect::CleanupMergedWorktree { .. }))
        .count();

    println!("Tick 1: {} CleanupMergedWorktree effects", cleanup_count_1);
    assert!(
        cleanup_count_1 > 0,
        "Tick 1 should produce at least one CleanupMergedWorktree effect for the injected PR"
    );

    // Tick 2: Should not re-clean up the same PRs
    let effects2 = harness.tick(&DaemonEvent::PrPollTick);

    let cleanup_count_2 = effects2
        .iter()
        .filter(|e| matches!(e, Effect::CleanupMergedWorktree { .. }))
        .count();

    println!("Tick 2: {} CleanupMergedWorktree effects", cleanup_count_2);

    assert_eq!(
        cleanup_count_2, 0,
        "Tick 2 should not re-clean up PRs that were already cleaned up in tick 1"
    );
}

/// Test that reconcile_orphaned_prs doesn't send duplicate lead nudges.
///
/// Bug scenario: Orphaned PR found → lead nudged on tick 1.
/// On tick 2, the same PR should not nudge the lead again.
///
/// Uses snapshot mutation to create an orphaned PR (reviewed, CI green,
/// no task association) that `reconcile_orphaned_prs` will act on.
#[test]
fn test_no_duplicate_orphaned_pr_tasks() {
    let fixture =
        include_str!("fixtures/snapshot/snapshot-reviewer-not-spawning-20260214-003545.json");
    let mut harness = MultiTickHarness::from_json(fixture).unwrap();

    // Inject an orphaned PR: reviewed, CI green, has a coworker branch prefix, no task.
    // open_prs_data is Vec<serde_json::Value> matching the GitHub API shape.
    let orphan_pr = json!({
        "number": 8888,
        "title": "feat: Test orphan PR [Midtown !999]",
        "headRefName": "broadway/test-orphan",
        "isDraft": false,
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"}
        ]
    });
    harness.snapshot_mut().open_prs_data.push(orphan_pr);
    harness.snapshot_mut().reviewed_prs.insert(8888);

    // Tick 1: Reconcile should nudge the lead for the orphaned PR (not create a task)
    let effects1 = harness.tick(&DaemonEvent::PrPollTick);

    let nudge_count_1 = effects1
        .iter()
        .filter(|e| matches!(e, Effect::NudgeChannelLead { .. }))
        .count();

    println!("Tick 1: {} NudgeChannelLead effects", nudge_count_1);
    assert!(
        nudge_count_1 > 0,
        "Tick 1 should nudge the lead for the orphaned PR"
    );

    let no_task_created_1 = effects1
        .iter()
        .all(|e| !matches!(e, Effect::CreateTask { .. }));
    assert!(no_task_created_1, "Tick 1 should NOT create a task");

    // Tick 2: Should not nudge the lead again for the same PR
    // (harness applies RecordOrphanedPrLeadNudge to update orphaned_pr_lead_nudges_sent)
    let effects2 = harness.tick(&DaemonEvent::PrPollTick);

    let nudge_count_2 = effects2
        .iter()
        .filter(|e| matches!(e, Effect::NudgeChannelLead { .. }))
        .count();

    println!("Tick 2: {} NudgeChannelLead effects", nudge_count_2);

    assert_eq!(
        nudge_count_2, 0,
        "Tick 2 should not nudge the lead again for the same orphaned PR"
    );
}

/// Test that stuck reviewer restart doesn't loop infinitely.
///
/// Bug scenario: Reviewer is stuck → restarted on tick 1.
/// On tick 2, if the reviewer is still stuck, it should use backoff,
/// not immediately restart again (preventing infinite loops).
#[test]
fn test_stuck_reviewer_backoff() {
    let fixture = include_str!(
        "fixtures/snapshot/snapshot-reviewer-assignment-stuck-pr-1164-20260217-020843.json"
    );
    let mut harness = MultiTickHarness::from_json(fixture).unwrap();

    // Tick 1: Check for stuck reviewers
    let effects1 = harness.tick(&DaemonEvent::SessionMonitorTick);

    let restart_count_1 = effects1
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::ShutdownCoworker { .. } | Effect::ShutdownCoworkerWithCallbacks { .. }
            )
        })
        .count();

    println!("Tick 1: {} reviewer restarts", restart_count_1);

    // Tick 2: Should apply backoff and not immediately restart again
    let effects2 = harness.tick(&DaemonEvent::SessionMonitorTick);

    let restart_count_2 = effects2
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::ShutdownCoworker { .. } | Effect::ShutdownCoworkerWithCallbacks { .. }
            )
        })
        .count();

    println!("Tick 2: {} reviewer restarts", restart_count_2);

    assert_eq!(
        restart_count_2, 0,
        "Tick 2 should not immediately restart a stuck reviewer due to backoff"
    );
}

/// Test that usage limit nudges don't fire repeatedly.
///
/// Bug scenario: Coworker hits usage limit → nudge scheduled on tick 1.
/// On tick 2, the nudge shouldn't be scheduled again because
/// `usage_limit_nudge_scheduled` is now true.
///
/// Uses snapshot mutation to set `has_usage_limit: true` on a coworker,
/// since the fixture doesn't have any coworkers with usage limits.
#[test]
fn test_usage_limit_nudge_dedup() {
    let fixture = include_str!(
        "fixtures/snapshot/snapshot-reviewer-assignment-stuck-pr-1164-20260217-020843.json"
    );
    let mut harness = MultiTickHarness::from_json(fixture).unwrap();

    // Inject a coworker with has_usage_limit: true so check_for_usage_limits
    // will actually produce SetUsageLimitNudge
    if let Some((_name, health)) = harness
        .snapshot_mut()
        .headless_process_health
        .iter_mut()
        .find(|(_, h)| h.is_alive)
    {
        health.has_usage_limit = true;
    }
    // Ensure the flag starts as false
    harness.snapshot_mut().usage_limit_nudge_scheduled = false;

    // Tick 1: Check for usage limits — should produce SetUsageLimitNudge
    let effects1 = harness.tick(&DaemonEvent::SessionMonitorTick);

    let nudge_count_1 = effects1
        .iter()
        .filter(|e| matches!(e, Effect::SetUsageLimitNudge { .. }))
        .count();

    println!("Tick 1: {} usage limit nudge effects", nudge_count_1);
    assert!(
        nudge_count_1 > 0,
        "Tick 1 should produce a SetUsageLimitNudge effect for the usage-limited coworker"
    );

    // Verify the harness applied the effect
    assert!(
        harness.snapshot().usage_limit_nudge_scheduled,
        "After tick 1, usage_limit_nudge_scheduled should be true"
    );

    // Tick 2: Should not re-schedule the nudge
    let effects2 = harness.tick(&DaemonEvent::SessionMonitorTick);

    let nudge_count_2 = effects2
        .iter()
        .filter(|e| matches!(e, Effect::SetUsageLimitNudge { .. }))
        .count();

    println!("Tick 2: {} usage limit nudge effects", nudge_count_2);

    assert_eq!(
        nudge_count_2, 0,
        "Tick 2 should not re-schedule a usage limit nudge — flag is already set"
    );
}

/// Test the two-tick auto-detach → respawn sequence for a stale attached lead.
///
/// This is the core scenario the fix addresses: the lead is stuck in "attached" state
/// because the interactive session ended without a detach call (crash/SSH disconnect).
///
/// Expected sequence:
/// - Tick 1: `detect_stale_attached_sessions` emits `AutoDetachCoworker { name: "lead" }`.
///   `ensure_lead_alive` sees lead still in `attached_coworkers` (same immutable snapshot),
///   so it does NOT spawn the lead. Harness applies `AutoDetachCoworker`, clearing the entry.
/// - Tick 2: `detect_stale_attached_sessions` emits nothing (entry gone).
///   `ensure_lead_alive` sees lead not in `attached_coworkers` and not running, so it spawns.
#[test]
fn test_auto_detach_stale_lead_then_respawn() {
    let fixture =
        include_str!("fixtures/snapshot/snapshot-reviewer-not-spawning-20260214-003545.json");
    let mut harness = MultiTickHarness::from_json(fixture).unwrap();

    // Set up: lead is attached but stale (15 min ago, past the 10-min ATTACH_TIMEOUT).
    // Also ensure lead is not running so ensure_lead_alive would spawn it once detached.
    // After the rename, the lead session name equals the repo name (e.g. "midtown").
    let lead_name = harness.snapshot().repo_name.clone();
    let stale_attach_time = harness.snapshot().now_utc - chrono::Duration::minutes(15);
    harness
        .snapshot_mut()
        .attached_coworkers
        .insert(lead_name.clone(), stale_attach_time);
    harness.snapshot_mut().active_names.remove(&lead_name);
    harness
        .snapshot_mut()
        .active_coworkers
        .retain(|c| !c.name.eq_ignore_ascii_case(&lead_name));
    harness
        .snapshot_mut()
        .running_coworkers
        .retain(|c| !c.name.eq_ignore_ascii_case(&lead_name));

    // Tick 1: detect_stale_attached_sessions emits AutoDetachCoworker;
    //         ensure_lead_alive sees lead as still attached (same snapshot) and does NOT spawn.
    let effects1 = harness.tick(&DaemonEvent::TaskDispatchTick);

    let auto_detach_count = effects1
        .iter()
        .filter(|e| matches!(e, Effect::AutoDetachCoworker { name } if name.eq_ignore_ascii_case(&lead_name)))
        .count();
    let spawn_count_1 = effects1
        .iter()
        .filter(
            |e| matches!(e, Effect::SpawnCoworker(c) if c.name.eq_ignore_ascii_case(&lead_name)),
        )
        .count();

    assert_eq!(
        auto_detach_count, 1,
        "Tick 1 should emit AutoDetachCoworker for stale lead"
    );
    assert_eq!(
        spawn_count_1, 0,
        "Tick 1 should NOT spawn lead — respawn happens on the next tick after detach"
    );

    // After tick 1, harness applied AutoDetachCoworker, so lead is no longer in attached_coworkers.
    assert!(
        !harness
            .snapshot()
            .attached_coworkers
            .contains_key(&lead_name),
        "After tick 1, lead should be removed from attached_coworkers"
    );

    // Tick 2: lead not attached, not running → ensure_lead_alive spawns it.
    let effects2 = harness.tick(&DaemonEvent::TaskDispatchTick);

    let auto_detach_count_2 = effects2
        .iter()
        .filter(|e| matches!(e, Effect::AutoDetachCoworker { name } if name.eq_ignore_ascii_case(&lead_name)))
        .count();
    let spawn_count_2 = effects2
        .iter()
        .filter(
            |e| matches!(e, Effect::SpawnCoworker(c) if c.name.eq_ignore_ascii_case(&lead_name)),
        )
        .count();

    assert_eq!(
        auto_detach_count_2, 0,
        "Tick 2 should not emit AutoDetachCoworker again (entry already cleared)"
    );
    assert_eq!(
        spawn_count_2, 1,
        "Tick 2 should spawn the lead now that it is no longer attached"
    );
}

/// Test multi-tick behavior with a long sequence (5 ticks).
///
/// Verifies that effects monotonically decrease (or stay at zero) as state stabilizes.
/// This catches runaway loops where effects keep being produced indefinitely.
#[test]
fn test_multi_tick_stabilization() {
    let fixture =
        include_str!("fixtures/snapshot/snapshot-reviewer-not-spawning-20260214-003545.json");
    let mut harness = MultiTickHarness::from_json(fixture).unwrap();

    let mut effect_counts = Vec::new();

    for i in 1..=5 {
        let effects = harness.tick(&DaemonEvent::TaskDispatchTick);
        effect_counts.push(effects.len());
        println!("Tick {}: {} effects", i, effects.len());
    }

    let last_count = effect_counts.last().unwrap();
    assert_eq!(
        *last_count, 0,
        "After 5 ticks, the system should stabilize with 0 effects (actual: {})",
        last_count
    );
}

// ── Session-centric dispatch tests ──────────────────────────────────────────

/// Test that dispatch_via_sessions skips tasks whose sessions are already running.
///
/// Bug scenario: Task is in_progress and has a running SessionRecord.
/// dispatch_via_sessions should see `record.is_running == true` and skip the task —
/// no `SpawnCoworkerWithCallbacks` should be emitted.
#[test]
fn test_session_dispatch_skips_running_session() {
    let mut harness = MultiTickHarness::new_minimal();
    let task_id = "task-session-running".to_string();

    // Add an in_progress task with owner "lexington"
    harness.snapshot_mut().in_progress_tasks.push((
        task_id.clone(),
        "Build feature X".to_string(),
        "lexington".to_string(),
    ));

    // Create a running session for that task
    harness.create_session("session-abc", &task_id, Some("lexington"));

    let effects = harness.tick(&DaemonEvent::TaskDispatchTick);

    // dispatch_via_sessions should NOT emit SpawnCoworkerWithCallbacks because
    // the session is already running (is_running == true).
    let resume_spawns: Vec<_> = effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnCoworkerWithCallbacks { config, .. }
                    if matches!(config.session_mode, SessionMode::ResumeSession(_))
            )
        })
        .collect();

    assert!(
        resume_spawns.is_empty(),
        "Should not spawn a session-resume when the session is already running (got {} effects)",
        resume_spawns.len()
    );

    let any_lexington_spawn = effects.iter().any(|e| match e {
        Effect::SpawnCoworkerWithCallbacks { config, .. } => config.name == "lexington",
        Effect::SpawnCoworker(config) => config.name == "lexington",
        _ => false,
    });
    assert!(
        !any_lexington_spawn,
        "Should emit no spawn at all for a running session"
    );
}

/// Test that dispatch_via_sessions resumes stopped sessions.
///
/// Bug scenario: Task is in_progress, SessionRecord exists but `is_running == false`.
/// dispatch_via_sessions should emit `SpawnCoworkerWithCallbacks` with
/// `session_mode == SessionMode::ResumeSession(...)`.
#[test]
fn test_session_dispatch_resumes_stopped_session() {
    let mut harness = MultiTickHarness::new_minimal();
    let task_id = "task-session-stopped".to_string();

    // Add an in_progress task with owner "park"
    harness.snapshot_mut().in_progress_tasks.push((
        task_id.clone(),
        "Fix bug Y".to_string(),
        "park".to_string(),
    ));

    // Create a session then stop it
    harness.create_session("session-xyz", &task_id, Some("park"));
    harness.stop_session("session-xyz");

    let effects = harness.tick(&DaemonEvent::TaskDispatchTick);

    // dispatch_via_sessions should emit SpawnCoworkerWithCallbacks with ResumeSession
    // because the session exists but is stopped.
    let resume_spawns: Vec<_> = effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnCoworkerWithCallbacks { config, .. }
                    if matches!(config.session_mode, SessionMode::ResumeSession(_))
            )
        })
        .collect();

    assert!(
        !resume_spawns.is_empty(),
        "Should emit SpawnCoworkerWithCallbacks(ResumeSession) for a stopped session"
    );
}

/// Test that dispatch_via_sessions doesn't produce duplicate resumes across ticks.
///
/// Bug scenario: A stopped session is resumed on tick 1. After applying the effect,
/// the session is running. On tick 2, dispatch_via_sessions should see
/// `record.is_running == true` and not re-spawn.
///
/// The harness's `tick()` automatically applies effects via `apply_effects()`. The
/// `SpawnCoworkerWithCallbacks` handler in `apply_effects` now updates
/// `sessions[id].is_running = true` when `session_mode == ResumeSession`, so no
/// manual `resume_session` call is needed between ticks.
#[test]
fn test_session_dispatch_no_duplicate_resumes_across_ticks() {
    let mut harness = MultiTickHarness::new_minimal();
    let task_id = "task-dedup".to_string();

    // Add an in_progress task with owner "madison"
    harness.snapshot_mut().in_progress_tasks.push((
        task_id.clone(),
        "Dedup test".to_string(),
        "madison".to_string(),
    ));

    // Create a session then stop it — this is the state before tick 1
    harness.create_session("session-dedup", &task_id, Some("madison"));
    harness.stop_session("session-dedup");

    // Tick 1: should produce a SpawnCoworkerWithCallbacks(ResumeSession) effect.
    // tick() automatically calls apply_effects(), which updates sessions[id].is_running
    // to true so dispatch_via_sessions sees the session as running on tick 2.
    let effects1 = harness.tick(&DaemonEvent::TaskDispatchTick);

    let spawn_count_1 = effects1
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnCoworkerWithCallbacks { config, .. }
                    if matches!(config.session_mode, SessionMode::ResumeSession(_))
            )
        })
        .count();

    println!("Tick 1: {} session-resume spawn effects", spawn_count_1);
    assert!(
        spawn_count_1 > 0,
        "Tick 1 should produce a session-resume spawn for the stopped session"
    );

    // Tick 2: apply_effects already marked the session as running via the
    // SpawnCoworkerWithCallbacks handler — no manual resume_session call needed.
    let effects2 = harness.tick(&DaemonEvent::TaskDispatchTick);

    let spawn_count_2 = effects2
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnCoworkerWithCallbacks { config, .. }
                    if matches!(config.session_mode, SessionMode::ResumeSession(_))
            )
        })
        .count();

    println!("Tick 2: {} session-resume spawn effects", spawn_count_2);
    assert_eq!(
        spawn_count_2, 0,
        "Tick 2 should not re-spawn an already-running session (is_running == true)"
    );
}
