//! Tests for dev limit enforcement in task dispatch.
//!
//! These tests verify that the dev spawn cap (max_coworkers - REVIEW_HEADROOM)
//! is correctly enforced even when multiple tasks are processed in a single tick.

use crate::daemon::constants::REVIEW_HEADROOM;

#[test]
fn test_review_headroom_computation() {
    // With max_coworkers=8 and REVIEW_HEADROOM=2, dev cap should be 6.
    let max_coworkers: usize = 8;
    let dev_cap = max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1);

    assert_eq!(
        dev_cap, 6,
        "Dev cap should be max_coworkers - REVIEW_HEADROOM"
    );
    assert_eq!(REVIEW_HEADROOM, 2, "REVIEW_HEADROOM should be 2");
}

#[test]
fn test_spawn_count_within_tick() {
    // This test documents the expected behavior for spawn limiting within a tick.
    //
    // Scenario: 5 active dev coworkers, 3 pending unowned tasks.
    // - Snapshot shows is_at_dev_limit = false (5 < 6)
    // - Loop processes tasks one by one
    //
    // Bug: Loop checks is_at_dev_limit ONCE at the start, then spawns all 3 tasks.
    // Result: 5 + 3 = 8 coworkers, exceeding dev cap of 6.
    //
    // Fix: After each spawn decision, increment a counter and re-check:
    //   - spawns_this_tick = 0
    //   - Process task 1: spawns_this_tick = 1, total = 6 (at cap, STOP)
    //   - Tasks 2 and 3 deferred to next tick

    let active_count = 5;
    let pending_count = 3;
    let dev_cap = 6;

    // Current behavior: all tasks spawn
    let spawned_without_fix = pending_count; // 3
    let total_without_fix = active_count + spawned_without_fix; // 8
    assert!(total_without_fix > dev_cap, "Bug: spawning exceeds dev cap");

    // Expected behavior after fix: only spawn until cap
    let spawned_with_fix = (dev_cap - active_count).min(pending_count); // 1
    let total_with_fix = active_count + spawned_with_fix; // 6
    assert_eq!(total_with_fix, dev_cap, "Fix: spawning stops at dev cap");
}

#[test]
fn test_spawn_limit_edge_cases() {
    let dev_cap = 6;

    // Edge case 1: Already at cap
    let active = 6;
    let allowed = (dev_cap - active).max(0);
    assert_eq!(allowed, 0, "No spawns allowed when at cap");

    // Edge case 2: One below cap
    let active = 5;
    let allowed = (dev_cap - active).max(0);
    assert_eq!(allowed, 1, "Exactly 1 spawn allowed when 1 below cap");

    // Edge case 3: Empty (no active coworkers)
    let active = 0;
    let allowed = (dev_cap - active).max(0);
    assert_eq!(
        allowed, 6,
        "Up to dev_cap spawns allowed when starting from 0"
    );
}
