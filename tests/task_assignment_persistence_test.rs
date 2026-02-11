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
use std::collections::HashMap;

/// Test that demonstrates task assignments lost after daemon restart.
///
/// The captured snapshot shows:
/// - `all_tasks`: Many tasks with owner field set (lexington, park, york, vernon, etc.)
/// - `coworker_task_assignments`: Only has {"park": "1136"}
///
/// Expected: coworker_task_assignments should include ALL in-progress tasks with owners.
/// Actual: Only one assignment survives the restart.
#[test]
fn test_task_assignments_lost_after_restart() {
    // Load the captured snapshot from the actual daemon restart
    let snapshot_json = include_str!(
        "fixtures/snapshot/snapshot-assignments-lost-after-restart-20260211-033718.json"
    );
    let snapshot: serde_json::Value = serde_json::from_str(snapshot_json).unwrap();

    // Extract coworker_task_assignments (the in-memory map after restart)
    let assignments_map = snapshot["coworker_task_assignments"].as_object().unwrap();

    // Extract all tasks with owners and in_progress status
    let tasks = snapshot["all_tasks"].as_array().unwrap();
    let in_progress_with_owners: Vec<(String, String)> = tasks
        .iter()
        .filter(|task| {
            task["status"].as_str() == Some("in_progress") && task["owner"].as_str().is_some()
        })
        .map(|task| {
            let task_id = task["id"].as_str().unwrap().to_string();
            let owner = task["owner"].as_str().unwrap().to_string();
            (task_id, owner)
        })
        .collect();

    println!(
        "Found {} in_progress tasks with owners in snapshot",
        in_progress_with_owners.len()
    );
    println!(
        "But coworker_task_assignments only has {} entries",
        assignments_map.len()
    );

    // Print some examples of missing assignments
    println!("\nExamples of tasks that have owners but are not in the assignment map:");
    for (task_id, owner) in in_progress_with_owners.iter().take(10) {
        if !assignments_map.contains_key(owner) {
            let task = tasks
                .iter()
                .find(|t| t["id"].as_str() == Some(task_id))
                .unwrap();
            let subject = task["subject"].as_str().unwrap();
            println!("  Task !{}: {} (owner: {})", task_id, subject, owner);
        }
    }

    // The bug: after restart, most assignments are lost
    // This assertion SHOULD fail with the current code
    assert!(
        assignments_map.len() >= in_progress_with_owners.len(),
        "Expected at least {} assignments, but found only {}. \
         Task ownership from disk was not restored to the in-memory map after daemon restart.",
        in_progress_with_owners.len(),
        assignments_map.len()
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
