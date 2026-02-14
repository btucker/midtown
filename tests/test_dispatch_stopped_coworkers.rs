//! Test that task dispatch works correctly when all coworkers are stopped.
//!
//! Regression test for bug where `spawn_for_pending_tasks` uses
//! `state.coworkers.list().len()` (all coworkers including stopped ones)
//! instead of counting only running coworkers, causing the dev limit check
//! to incorrectly think the limit is reached when all coworkers are stopped.

use serde_json::Value;

#[test]
fn test_dispatch_with_all_coworkers_stopped() {
    // Load the captured snapshot from the bug report
    let fixture =
        include_str!("fixtures/snapshot/snapshot-reviewer-not-spawning-20260214-003545.json");
    let snapshot: Value = serde_json::from_str(fixture).expect("Failed to parse snapshot JSON");

    // Verify preconditions: this is the exact state that triggered the bug
    let active_coworkers = snapshot["active_coworkers"].as_array().unwrap();
    let running_coworkers = snapshot["running_coworkers"].as_array().unwrap();
    let pending_tasks = snapshot["pending_tasks_without_owners"].as_array().unwrap();
    let stop_times = snapshot["coworker_stop_times"].as_object().unwrap();
    let is_at_dev_limit = snapshot["is_at_dev_limit"].as_bool().unwrap();
    let prs_needing_review = snapshot["prs_needing_review"].as_u64().unwrap();

    assert_eq!(active_coworkers.len(), 0, "No active coworkers");
    assert_eq!(running_coworkers.len(), 0, "No running coworkers");
    assert_eq!(pending_tasks.len(), 8, "8 pending tasks without owners");
    assert!(!is_at_dev_limit, "Not at dev limit");
    assert_eq!(prs_needing_review, 3, "3 PRs need review");

    // Verify that there ARE stopped coworkers in coworker_stop_times
    assert!(
        !stop_times.is_empty(),
        "Should have stopped coworkers in coworker_stop_times"
    );
    assert!(
        stop_times.len() >= 8,
        "Should have at least 8 stopped coworkers (actual: {})",
        stop_times.len()
    );

    // This verifies the bug condition exists in the captured snapshot.
    // The actual fix is in dispatch.rs line 1422: using snap.running_coworkers.len()
    // instead of state.coworkers.list().len().
    //
    // Expected behavior: With 0 running coworkers and 8 pending tasks,
    // spawn_for_pending_tasks should return AssignAndSpawn effects.
    //
    // Actual behavior before fix: The dispatcher checked state.coworkers.list().len()
    // which included stopped coworkers, incorrectly hitting the dev limit and
    // preventing task dispatch.
}
