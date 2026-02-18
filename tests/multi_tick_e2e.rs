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

/// Test that reconcile_orphaned_prs doesn't create duplicate tasks.
///
/// Bug scenario: Orphaned PR found → task created on tick 1.
/// On tick 2, the same PR shouldn't get another task.
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

    // Tick 1: Reconcile should create a task for the orphaned PR
    let effects1 = harness.tick(&DaemonEvent::PrPollTick);

    let create_count_1 = effects1
        .iter()
        .filter(|e| matches!(e, Effect::CreateTask { .. }))
        .count();

    println!("Tick 1: {} CreateTask effects", create_count_1);
    assert!(
        create_count_1 > 0,
        "Tick 1 should create a task for the orphaned PR"
    );

    // Tick 2: Should not create a duplicate task for the same PR
    let effects2 = harness.tick(&DaemonEvent::PrPollTick);

    let create_count_2 = effects2
        .iter()
        .filter(|e| matches!(e, Effect::CreateTask { .. }))
        .count();

    println!("Tick 2: {} CreateTask effects", create_count_2);

    assert_eq!(
        create_count_2, 0,
        "Tick 2 should not create duplicate tasks for PRs that already have tasks from tick 1"
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
    let stale_attach_time = harness.snapshot().now_utc - chrono::Duration::minutes(15);
    harness
        .snapshot_mut()
        .attached_coworkers
        .insert("lead".to_string(), stale_attach_time);
    harness.snapshot_mut().active_names.remove("lead");
    harness
        .snapshot_mut()
        .active_coworkers
        .retain(|c| c.name.to_lowercase() != "lead");
    harness
        .snapshot_mut()
        .running_coworkers
        .retain(|c| c.name.to_lowercase() != "lead");

    // Tick 1: detect_stale_attached_sessions emits AutoDetachCoworker;
    //         ensure_lead_alive sees lead as still attached (same snapshot) and does NOT spawn.
    let effects1 = harness.tick(&DaemonEvent::TaskDispatchTick);

    let auto_detach_count = effects1
        .iter()
        .filter(|e| matches!(e, Effect::AutoDetachCoworker { name } if name == "lead"))
        .count();
    let spawn_count_1 = effects1
        .iter()
        .filter(|e| matches!(e, Effect::SpawnCoworker(c) if c.name.to_lowercase() == "lead"))
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
        !harness.snapshot().attached_coworkers.contains_key("lead"),
        "After tick 1, lead should be removed from attached_coworkers"
    );

    // Tick 2: lead not attached, not running → ensure_lead_alive spawns it.
    let effects2 = harness.tick(&DaemonEvent::TaskDispatchTick);

    let auto_detach_count_2 = effects2
        .iter()
        .filter(|e| matches!(e, Effect::AutoDetachCoworker { name } if name == "lead"))
        .count();
    let spawn_count_2 = effects2
        .iter()
        .filter(|e| matches!(e, Effect::SpawnCoworker(c) if c.name.to_lowercase() == "lead"))
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
