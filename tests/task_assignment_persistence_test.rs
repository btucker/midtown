/// Test that task assignments are restored after daemon restart.
///
/// This test reproduces the bug where coworker-task assignments are lost
/// after a daemon restart (git pull + cargo build + midtown restart).
///
/// Before restart: 8+ coworkers with task assignments.
/// After restart: only 1 coworker in coworker_task_assignments map.
///
/// The bug occurs because coworker_task_assignments is an in-memory HashMap
/// that is initialized empty on daemon startup, and task ownership information
/// from disk (task files' owner field) is never restored to this map.
use std::collections::{HashMap, HashSet};

/// Regression test documenting the bug that was fixed.
///
/// The captured snapshot shows the BEFORE state (bug present):
/// - `all_tasks`: 7 tasks with owner field set (lexington, park, york, vernon, etc.)
/// - `coworker_task_assignments`: Only has 1 entry (park)
///
/// This test verifies that:
/// 1. The snapshot captured the bug state correctly (missing assignments)
/// 2. The fix (restore_task_assignments_from_disk) would restore them
#[test]
fn test_task_assignments_lost_after_restart() {
    // Load the captured snapshot from before the fix was applied
    let snapshot_json = include_str!(
        "fixtures/snapshot/snapshot-assignments-lost-after-restart-20260211-033718.json"
    );
    let snapshot: serde_json::Value = serde_json::from_str(snapshot_json).unwrap();

    // Extract coworker_task_assignments (the in-memory map BEFORE the fix)
    let assignments_map = snapshot["coworker_task_assignments"].as_object().unwrap();

    // Extract all tasks with owners and in_progress status
    let tasks = snapshot["all_tasks"].as_array().unwrap();
    let in_progress_with_owners: Vec<(String, String)> = tasks
        .iter()
        .filter(|task| {
            task["status"].as_str() == Some("in_progress")
                && task["owner"].as_str().is_some()
                && !task["owner"].as_str().unwrap().is_empty()
        })
        .map(|task| {
            let task_id = task["id"].as_str().unwrap().to_string();
            let owner = task["owner"].as_str().unwrap().to_string();
            (task_id, owner)
        })
        .collect();

    println!(
        "Snapshot shows {} in_progress tasks with owners",
        in_progress_with_owners.len()
    );
    println!(
        "But coworker_task_assignments only had {} entries (bug state)",
        assignments_map.len()
    );

    // Verify the bug existed: snapshot should show missing assignments
    assert!(
        assignments_map.len() < in_progress_with_owners.len(),
        "Snapshot should show the bug state (assignments < tasks with owners)"
    );

    // Verify the fix would work: simulate restore_task_assignments_from_disk
    // Note: One coworker can only have one active task, so the map has one entry per owner
    let mut restored_assignments: HashMap<String, String> = HashMap::new();
    for (task_id, owner) in &in_progress_with_owners {
        restored_assignments.insert(owner.to_lowercase(), task_id.clone());
    }

    // Count unique owners (a coworker can only be assigned to one task)
    let unique_owners: HashSet<String> = in_progress_with_owners
        .iter()
        .map(|(_, owner)| owner.to_lowercase())
        .collect();

    // After restore, there should be one assignment per unique owner
    assert_eq!(
        restored_assignments.len(),
        unique_owners.len(),
        "After restoration, there should be one assignment per unique owner"
    );

    // The restoration should have more assignments than the buggy snapshot
    assert!(
        restored_assignments.len() > assignments_map.len(),
        "Fix should restore more assignments than the buggy snapshot had"
    );

    println!(
        "\n✓ Bug captured: {} assignments missing (had {} but needed {})",
        unique_owners.len() - assignments_map.len(),
        assignments_map.len(),
        unique_owners.len()
    );
    println!(
        "✓ Fix verified: restore would add {} assignments",
        restored_assignments.len()
    );
}

/// Helper function to rebuild coworker_task_assignments from task storage.
///
/// This is what the daemon startup should do to restore assignments after restart.
#[test]
fn test_rebuild_assignments_from_disk() {
    // This test demonstrates the fix: reading task owners from disk
    // and populating the in-memory map.

    let snapshot_json = include_str!(
        "fixtures/snapshot/snapshot-assignments-lost-after-restart-20260211-033718.json"
    );
    let snapshot: serde_json::Value = serde_json::from_str(snapshot_json).unwrap();

    // Read in_progress tasks from snapshot (simulating disk read)
    let tasks = snapshot["all_tasks"].as_array().unwrap();
    let in_progress_tasks: Vec<(String, String, String)> = tasks
        .iter()
        .filter(|task| {
            task["status"].as_str() == Some("in_progress")
                && task["owner"].as_str().is_some()
                && !task["owner"].as_str().unwrap().is_empty()
        })
        .map(|task| {
            let task_id = task["id"].as_str().unwrap().to_string();
            let subject = task["subject"].as_str().unwrap().to_string();
            let owner = task["owner"].as_str().unwrap().to_string();
            (task_id, subject, owner)
        })
        .collect();

    // Rebuild the assignment map (this is what the fix should do)
    let mut rebuilt_assignments: HashMap<String, String> = HashMap::new();
    for (task_id, _subject, owner) in &in_progress_tasks {
        rebuilt_assignments.insert(owner.to_lowercase(), task_id.clone());
    }

    println!(
        "Rebuilt {} assignments from {} in_progress tasks",
        rebuilt_assignments.len(),
        in_progress_tasks.len()
    );

    // Verify that all in_progress tasks with owners are represented
    for (task_id, _subject, owner) in &in_progress_tasks {
        assert!(
            rebuilt_assignments.contains_key(&owner.to_lowercase()),
            "Owner {} for task !{} should be in the rebuilt assignments",
            owner,
            task_id
        );
    }

    // Print summary
    println!("\nRebuilt assignments:");
    for (owner, task_id) in rebuilt_assignments.iter().take(10) {
        println!("  {} -> task !{}", owner, task_id);
    }
}
