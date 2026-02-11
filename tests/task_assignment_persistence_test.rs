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
use midtown::daemon::build_task_assignments_from_tasks;
use midtown::tasks::{Task, TaskStatus};

/// Test that demonstrates the bug: task assignments lost after daemon restart.
///
/// The captured snapshot shows the bug state:
/// - `all_tasks`: Many tasks with owner field set (lexington, park, york, vernon, etc.)
/// - `coworker_task_assignments`: Only has {"park": "1136"}
///
/// This test verifies that we can DETECT the bug (assignments were lost).
/// The actual FIX is tested in the tests below.
#[test]
fn test_task_assignments_lost_after_restart() {
    // Load the captured snapshot from the actual daemon restart (BEFORE fix was applied)
    let snapshot_json = include_str!(
        "fixtures/snapshot/snapshot-assignments-lost-after-restart-20260211-033718.json"
    );
    let snapshot: serde_json::Value = serde_json::from_str(snapshot_json).unwrap();

    // Extract coworker_task_assignments (the in-memory map after restart, before fix)
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

    // Verify that the bug exists in the captured snapshot:
    // We SHOULD have 7 assignments but only have 1.
    // This proves the bug was real before the fix.
    assert_eq!(
        in_progress_with_owners.len(),
        7,
        "Expected 7 in_progress tasks with owners"
    );
    assert_eq!(
        assignments_map.len(),
        1,
        "Before the fix, only 1 assignment survived daemon restart (this is the bug)"
    );
}

/// Test that the actual `build_task_assignments_from_tasks()` function correctly
/// restores assignments from Task structs.
///
/// This exercises the real restoration logic (not an inline simulation), catching
/// bugs in struct handling, filtering, or the Entry API.
#[test]
fn test_build_task_assignments_from_tasks() {
    // Parse snapshot tasks into real Task structs
    let snapshot_json = include_str!(
        "fixtures/snapshot/snapshot-assignments-lost-after-restart-20260211-033718.json"
    );
    let snapshot: serde_json::Value = serde_json::from_str(snapshot_json).unwrap();

    let tasks: Vec<Task> = snapshot["all_tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| serde_json::from_value(t.clone()).unwrap())
        .collect();

    // Call the actual production function
    let assignments = build_task_assignments_from_tasks(&tasks);

    // Collect expected owners from snapshot for comparison
    let expected_owners: Vec<String> = snapshot["all_tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| {
            t["status"].as_str() == Some("in_progress")
                && t["owner"].as_str().is_some_and(|o| !o.is_empty())
        })
        .map(|t| t["owner"].as_str().unwrap().to_lowercase())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    // Every unique owner with an in_progress task should have an assignment
    assert_eq!(
        assignments.len(),
        expected_owners.len(),
        "Should have one assignment per unique owner with in_progress tasks"
    );

    for owner in &expected_owners {
        assert!(
            assignments.contains_key(owner),
            "Owner {} should have a restored assignment",
            owner
        );
        // The assigned task_id should be non-empty
        assert!(
            !assignments[owner].task_id.is_empty(),
            "Task ID for owner {} should be non-empty",
            owner
        );
    }

    // Completed/pending tasks should NOT contribute assignments
    assert!(
        !assignments.contains_key("lead"),
        "Lead (non-coworker owner) should not appear unless they have an in_progress task"
    );
}

/// Test that duplicate coworker assignments (same coworker, multiple in_progress tasks)
/// are handled correctly: only the first task is kept, subsequent tasks are skipped.
///
/// The snapshot contains york with 2 in_progress tasks (1112 and 1125).
/// The production code uses Entry API to keep only the first one encountered.
#[test]
fn test_duplicate_coworker_assignment_keeps_first_task() {
    // Create tasks where one coworker has multiple in_progress tasks
    let tasks = vec![
        Task {
            id: "100".to_string(),
            subject: "First task for york".to_string(),
            status: TaskStatus::InProgress,
            owner: Some("york".to_string()),
            description: None,
            blocked_by: vec![],
            channel: None,
            created_at: None,
        },
        Task {
            id: "200".to_string(),
            subject: "Second task for york".to_string(),
            status: TaskStatus::InProgress,
            owner: Some("york".to_string()),
            description: None,
            blocked_by: vec![],
            channel: None,
            created_at: None,
        },
        Task {
            id: "300".to_string(),
            subject: "Task for park".to_string(),
            status: TaskStatus::InProgress,
            owner: Some("park".to_string()),
            description: None,
            blocked_by: vec![],
            channel: None,
            created_at: None,
        },
    ];

    let assignments = build_task_assignments_from_tasks(&tasks);

    // york should have exactly 1 assignment (the first task encountered)
    assert_eq!(
        assignments.len(),
        2,
        "Should have 2 assignments (york and park), not 3"
    );
    assert_eq!(
        assignments["york"].task_id, "100",
        "york should be assigned the first task (100), not the second (200)"
    );
    assert_eq!(assignments["park"].task_id, "300");
}

/// Test edge cases: empty owner, no owner, non-in_progress tasks are excluded.
#[test]
fn test_build_task_assignments_filters_correctly() {
    let tasks = vec![
        // In-progress with owner → should be included
        Task {
            id: "1".to_string(),
            subject: "Active task".to_string(),
            status: TaskStatus::InProgress,
            owner: Some("lexington".to_string()),
            description: None,
            blocked_by: vec![],
            channel: None,
            created_at: None,
        },
        // Completed with owner → should be excluded
        Task {
            id: "2".to_string(),
            subject: "Done task".to_string(),
            status: TaskStatus::Completed,
            owner: Some("park".to_string()),
            description: None,
            blocked_by: vec![],
            channel: None,
            created_at: None,
        },
        // Pending with owner → should be excluded
        Task {
            id: "3".to_string(),
            subject: "Pending task".to_string(),
            status: TaskStatus::Pending,
            owner: Some("madison".to_string()),
            description: None,
            blocked_by: vec![],
            channel: None,
            created_at: None,
        },
        // In-progress with no owner → should be excluded
        Task {
            id: "4".to_string(),
            subject: "Unowned task".to_string(),
            status: TaskStatus::InProgress,
            owner: None,
            description: None,
            blocked_by: vec![],
            channel: None,
            created_at: None,
        },
        // In-progress with empty owner → should be excluded
        Task {
            id: "5".to_string(),
            subject: "Empty owner task".to_string(),
            status: TaskStatus::InProgress,
            owner: Some("".to_string()),
            description: None,
            blocked_by: vec![],
            channel: None,
            created_at: None,
        },
    ];

    let assignments = build_task_assignments_from_tasks(&tasks);

    assert_eq!(
        assignments.len(),
        1,
        "Only the active in_progress task with a non-empty owner should be included"
    );
    assert!(assignments.contains_key("lexington"));
    assert_eq!(assignments["lexington"].task_id, "1");
}

/// Test that owner names are case-insensitive (lowercased in the map).
#[test]
fn test_owner_names_are_lowercased() {
    let tasks = vec![Task {
        id: "42".to_string(),
        subject: "Mixed case owner".to_string(),
        status: TaskStatus::InProgress,
        owner: Some("Broadway".to_string()),
        description: None,
        blocked_by: vec![],
        channel: None,
        created_at: None,
    }];

    let assignments = build_task_assignments_from_tasks(&tasks);

    assert!(
        assignments.contains_key("broadway"),
        "Owner name should be lowercased in the map"
    );
    assert!(
        !assignments.contains_key("Broadway"),
        "Original case should not be a key"
    );
}
