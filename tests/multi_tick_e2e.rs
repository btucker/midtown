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

/// Test that reset_orphaned_tasks doesn't produce duplicate resets.
///
/// Bug scenario: Task is orphaned (owner not running) → reset on tick 1.
/// On tick 2, the same task shouldn't be reset again.
#[test]
fn test_no_duplicate_orphan_resets() {
    // Load a snapshot with orphaned tasks
    let fixture = include_str!(
        "fixtures/snapshot/snapshot-tool-names-must-be-unique-all-stuck-20260211-030435.json"
    );
    let mut harness = MultiTickHarness::from_json(fixture).unwrap();

    // Tick 1: Reset orphaned tasks
    let effects1 = harness.tick(&DaemonEvent::TaskDispatchTick);

    // Count reset effects
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

    // After effects are applied from tick 1, tick 2 should not re-reset the same tasks.
    // The harness simulates applying resets, so tasks that were reset in tick 1
    // should be pending (with no owner) in tick 2.
    //
    // If the decision function has a bug and doesn't check current state properly,
    // it would try to reset the same task again.
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

    // The harness simulates removing coworkers from active_names when they're shut down.
    // Tick 2 should not attempt to shut down coworkers that were already removed.
    assert_eq!(
        shutdown_count_2, 0,
        "Tick 2 should not re-shutdown coworkers that were already shut down in tick 1"
    );
}

/// Test that merged PR cleanup doesn't duplicate task completion effects.
///
/// Bug scenario: PR is merged → task completed on tick 1.
/// On tick 2, the same task shouldn't be completed again.
#[test]
fn test_no_duplicate_pr_cleanup() {
    let fixture =
        include_str!("fixtures/snapshot/snapshot-reviewer-not-spawning-20260214-003545.json");
    let mut harness = MultiTickHarness::from_json(fixture).unwrap();

    // Tick 1: Collect merged PR cleanup effects
    let effects1 = harness.tick(&DaemonEvent::PrPollTick);

    let complete_count_1 = effects1
        .iter()
        .filter(|e| matches!(e, Effect::CompleteTask { .. }))
        .count();

    println!("Tick 1: {} complete task effects", complete_count_1);

    // Tick 2: Should not re-complete the same tasks
    let effects2 = harness.tick(&DaemonEvent::PrPollTick);

    let complete_count_2 = effects2
        .iter()
        .filter(|e| matches!(e, Effect::CompleteTask { .. }))
        .count();

    println!("Tick 2: {} complete task effects", complete_count_2);

    // The harness marks tasks as completed when CompleteTask effects are applied.
    // Tick 2 should not try to complete tasks that are already completed.
    assert_eq!(
        complete_count_2, 0,
        "Tick 2 should not re-complete tasks that were already completed in tick 1"
    );
}

/// Test that reviewer spawn doesn't duplicate across ticks.
///
/// Bug scenario: PR needs review → reviewer assigned on tick 1.
/// On tick 2, another reviewer shouldn't be assigned to the same PR.
#[test]
fn test_no_duplicate_reviewer_spawns() {
    let fixture =
        include_str!("fixtures/snapshot/snapshot-reviewer-not-spawning-20260214-003545.json");
    let mut harness = MultiTickHarness::from_json(fixture).unwrap();

    // Tick 1: Reconcile orphaned PRs (may spawn reviewers)
    let effects1 = harness.tick(&DaemonEvent::PrPollTick);

    let assign_count_1 = effects1
        .iter()
        .filter(|e| matches!(e, Effect::AssignReviewer { .. }))
        .count();

    println!("Tick 1: {} reviewer assignments", assign_count_1);

    // Tick 2: Should not re-assign reviewers to the same PRs
    let effects2 = harness.tick(&DaemonEvent::PrPollTick);

    let assign_count_2 = effects2
        .iter()
        .filter(|e| matches!(e, Effect::AssignReviewer { .. }))
        .count();

    println!("Tick 2: {} reviewer assignments", assign_count_2);

    // The harness tracks reviewer assignments. After tick 1 assigns a reviewer,
    // tick 2 should see that the PR already has a reviewer and not assign another.
    //
    // Note: This depends on the decision function checking reviewer_pr_assignments.
    // If it's buggy and doesn't check, it would assign again.
    assert_eq!(
        assign_count_2, 0,
        "Tick 2 should not re-assign reviewers to PRs that already have reviewers from tick 1"
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

    // The stuck reviewer logic includes cooldowns and restart count limits.
    // Even if the reviewer is still stuck after tick 1, tick 2 should respect
    // cooldowns and backoff instead of restarting immediately.
    //
    // Note: This test depends on the decision function tracking restart counts
    // and implementing backoff. If buggy, it would restart every tick.
    assert_eq!(
        restart_count_2, 0,
        "Tick 2 should not immediately restart a stuck reviewer due to backoff"
    );
}

/// Test that usage limit nudges don't spam every tick.
///
/// Bug scenario: Coworker hits usage limit → nudge scheduled on tick 1.
/// On tick 2, the nudge shouldn't be scheduled again immediately.
#[test]
fn test_usage_limit_nudge_dedup() {
    let fixture = include_str!(
        "fixtures/snapshot/snapshot-reviewer-assignment-stuck-pr-1164-20260217-020843.json"
    );
    let mut harness = MultiTickHarness::from_json(fixture).unwrap();

    // Tick 1: Check for usage limits
    let effects1 = harness.tick(&DaemonEvent::SessionMonitorTick);

    let nudge_count_1 = effects1
        .iter()
        .filter(|e| matches!(e, Effect::SetUsageLimitNudge { .. }))
        .count();

    println!("Tick 1: {} usage limit nudge effects", nudge_count_1);

    // Tick 2: Should not re-schedule the same nudge
    let effects2 = harness.tick(&DaemonEvent::SessionMonitorTick);

    let nudge_count_2 = effects2
        .iter()
        .filter(|e| matches!(e, Effect::SetUsageLimitNudge { .. }))
        .count();

    println!("Tick 2: {} usage limit nudge effects", nudge_count_2);

    // The usage_limit_nudge_scheduled flag in WorldSnapshot tracks whether
    // a nudge is already scheduled. Tick 2 should see this and not schedule again.
    //
    // Note: The harness doesn't currently mutate usage_limit_nudge_scheduled,
    // so this test may pass trivially. To make it meaningful, we'd need to
    // simulate the effect of SetUsageLimitNudge.
    assert_eq!(
        nudge_count_2, 0,
        "Tick 2 should not re-schedule a usage limit nudge that's already scheduled"
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

    // After the first tick resolves issues, subsequent ticks should produce
    // fewer effects. By tick 5, we should be stable (0 effects).
    //
    // If there's a bug causing an infinite loop, effect counts won't decrease.
    let last_count = effect_counts.last().unwrap();
    assert_eq!(
        *last_count, 0,
        "After 5 ticks, the system should stabilize with 0 effects (actual: {})",
        last_count
    );
}
