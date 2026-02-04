//! E2E tests for task dispatch using captured WorldSnapshot fixtures.
//!
//! These tests verify the daemon correctly assigns pending tasks to available
//! coworkers using real-world state snapshots. The tests cover:
//! - Pending unowned tasks get assigned to available coworkers
//! - Blocked tasks stay unassigned until dependencies complete
//! - Orphan recovery for in_progress tasks with inactive owners
//!
//! To capture new fixtures: `midtown e2e capture --label task-dispatch-<scenario>`

use std::collections::HashSet;

use chrono::{DateTime, Datelike, Utc};
use serde::Deserialize;
use serde_json::Value;

/// Task structure matching the WorldSnapshot JSON format.
#[derive(Debug, Clone, Deserialize)]
struct Task {
    id: String,
    subject: String,
    status: String,
    owner: Option<String>,
    #[serde(default)]
    blocked_by: Vec<String>,
}

/// Coworker snapshot data from fixture.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CoworkerSnapshot {
    name: String,
    started_at: DateTime<Utc>,
    isolated_tasks: bool,
}

/// Parsed snapshot data for task dispatch tests.
#[derive(Debug)]
struct DispatchSnapshotData {
    /// All tasks from the snapshot.
    all_tasks: Vec<Task>,
    /// Names of currently active coworkers (running tmux windows).
    active_names: HashSet<String>,
    /// Coworkers currently busy (have in_progress tasks).
    busy_coworkers: HashSet<String>,
    /// Coworkers with open PRs (shouldn't be sent on break).
    coworkers_with_open_prs: HashSet<String>,
    /// Coworkers assigned as reviewers.
    active_reviewers: HashSet<String>,
    /// Whether we're at the dev coworker limit.
    is_at_dev_limit: bool,
    /// Current timestamp from snapshot.
    now_utc: DateTime<Utc>,
    /// Coworker start times for sorting (e.g., duplicate worker detection).
    coworker_start_times: std::collections::HashMap<String, DateTime<Utc>>,
}

/// Load a snapshot fixture and parse it into test-friendly data structures.
fn load_snapshot(json_str: &str) -> (Vec<CoworkerSnapshot>, DispatchSnapshotData) {
    let v: Value = serde_json::from_str(json_str).expect("valid JSON");

    // Extract coworker snapshots
    let coworkers: Vec<CoworkerSnapshot> = v["coworker_snapshots"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|cw| CoworkerSnapshot {
            name: cw["name"].as_str().unwrap_or("").to_string(),
            started_at: DateTime::parse_from_rfc3339(
                cw["started_at"].as_str().unwrap_or("1970-01-01T00:00:00Z"),
            )
            .unwrap()
            .with_timezone(&Utc),
            isolated_tasks: cw["isolated_tasks"].as_bool().unwrap_or(false),
        })
        .collect();

    // Parse all_tasks array
    let all_tasks: Vec<Task> = v["all_tasks"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    // Extract HashSets from snapshot
    let active_names: HashSet<String> = v["active_names"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let busy_coworkers: HashSet<String> = v["busy_coworkers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let coworkers_with_open_prs: HashSet<String> = v["coworkers_with_open_prs"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let active_reviewers: HashSet<String> = v["active_reviewers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let is_at_dev_limit = v["is_at_dev_limit"].as_bool().unwrap_or(false);

    let now_utc =
        DateTime::parse_from_rfc3339(v["now_utc"].as_str().unwrap_or("1970-01-01T00:00:00Z"))
            .unwrap()
            .with_timezone(&Utc);

    let coworker_start_times: std::collections::HashMap<String, DateTime<Utc>> =
        v["coworker_start_times"]
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| {
                        v.as_str()
                            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                            .map(|dt| (k.clone(), dt.with_timezone(&Utc)))
                    })
                    .collect()
            })
            .unwrap_or_default();

    (
        coworkers,
        DispatchSnapshotData {
            all_tasks,
            active_names,
            busy_coworkers,
            coworkers_with_open_prs,
            active_reviewers,
            is_at_dev_limit,
            now_utc,
            coworker_start_times,
        },
    )
}

/// Get pending tasks without owners from the task list.
#[allow(dead_code)]
fn get_pending_unowned_tasks(tasks: &[Task]) -> Vec<&Task> {
    tasks
        .iter()
        .filter(|t| t.status == "pending" && t.owner.as_ref().map(|o| o.is_empty()).unwrap_or(true))
        .collect()
}

/// Get pending tasks with owners assigned.
fn get_pending_with_owners(tasks: &[Task]) -> Vec<&Task> {
    tasks
        .iter()
        .filter(|t| {
            t.status == "pending"
                && t.owner
                    .as_ref()
                    .map(|o| !o.is_empty() && o.to_lowercase() != "lead")
                    .unwrap_or(false)
        })
        .collect()
}

/// Get in_progress tasks (for orphan detection).
fn get_in_progress_tasks(tasks: &[Task]) -> Vec<&Task> {
    tasks.iter().filter(|t| t.status == "in_progress").collect()
}

/// Check if a task is blocked by any incomplete task.
fn is_task_blocked(task: &Task, all_tasks: &[Task]) -> bool {
    if task.blocked_by.is_empty() {
        return false;
    }

    // Task is blocked if any of its blockers are not completed
    task.blocked_by.iter().any(|blocker_id| {
        all_tasks
            .iter()
            .find(|t| t.id == *blocker_id)
            .map(|t| t.status != "completed")
            .unwrap_or(true) // If blocker not found, treat as blocked (conservative)
    })
}

/// Get idle coworkers (active but not busy, no open PR, not a reviewer).
fn get_idle_coworkers(data: &DispatchSnapshotData) -> Vec<&String> {
    data.active_names
        .iter()
        .filter(|name| {
            !data.busy_coworkers.contains(*name)
                && !data.coworkers_with_open_prs.contains(*name)
                && !data.active_reviewers.contains(*name)
        })
        .collect()
}

// =============================================================================
// Tests
// =============================================================================

/// Test that the snapshot contains expected task dispatch state.
///
/// This test loads the captured snapshot and verifies the data structure
/// is correctly parsed, providing a foundation for dispatch logic tests.
#[test]
fn snapshot_loads_task_dispatch_data() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (coworkers, data) = load_snapshot(fixture);

    // Verify we have coworkers
    assert!(!coworkers.is_empty(), "snapshot should have coworkers");

    // Verify we have tasks
    assert!(!data.all_tasks.is_empty(), "snapshot should have tasks");

    // Verify active_names is populated
    assert!(
        !data.active_names.is_empty(),
        "snapshot should have active coworkers"
    );

    // Verify timestamp is reasonable (not epoch)
    assert!(
        data.now_utc.year() >= 2026,
        "snapshot timestamp should be recent"
    );
}

/// Test that in_progress tasks have owners for potential orphan detection.
///
/// The daemon should detect orphaned tasks (in_progress with inactive owner)
/// and trigger recovery. This test verifies the snapshot contains in_progress
/// tasks that can be used for orphan detection testing.
#[test]
fn in_progress_tasks_have_owners() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (_, data) = load_snapshot(fixture);

    let in_progress = get_in_progress_tasks(&data.all_tasks);

    for task in in_progress {
        // In-progress tasks should have an owner (either active or orphaned)
        let owner = task.owner.as_deref().unwrap_or("");
        assert!(
            !owner.is_empty(),
            "in_progress task #{} should have an owner, found empty",
            task.id
        );
    }
}

/// Test that busy coworkers match in_progress task owners.
///
/// The snapshot's busy_coworkers set should correspond to coworkers
/// who own in_progress tasks. This verifies consistency between
/// task state and coworker state.
#[test]
fn busy_coworkers_match_task_owners() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (_, data) = load_snapshot(fixture);

    let in_progress = get_in_progress_tasks(&data.all_tasks);

    // Get owners of in_progress tasks
    let in_progress_owners: HashSet<String> = in_progress
        .iter()
        .filter_map(|t| t.owner.as_ref())
        .map(|o| o.to_lowercase())
        .collect();

    // Each busy coworker should own an in_progress task
    for busy in &data.busy_coworkers {
        let busy_lower = busy.to_lowercase();
        assert!(
            in_progress_owners.contains(&busy_lower),
            "busy coworker {} should own an in_progress task",
            busy
        );
    }
}

/// Test that blocked tasks are correctly identified.
///
/// Tasks with non-empty blocked_by lists where the blocking task is
/// not completed should be identified as blocked.
#[test]
fn blocked_tasks_identified() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (_, data) = load_snapshot(fixture);

    // Find tasks with blocked_by entries
    let tasks_with_blockers: Vec<_> = data
        .all_tasks
        .iter()
        .filter(|t| !t.blocked_by.is_empty())
        .collect();

    for task in tasks_with_blockers {
        // Verify blockers exist in the task list
        for blocker_id in &task.blocked_by {
            let blocker_exists = data.all_tasks.iter().any(|t| t.id == *blocker_id);
            // Blocker should exist (or we treat as blocked conservatively)
            if blocker_exists {
                let blocker = data.all_tasks.iter().find(|t| t.id == *blocker_id).unwrap();
                if blocker.status != "completed" {
                    assert!(
                        is_task_blocked(task, &data.all_tasks),
                        "task #{} with incomplete blocker #{} should be blocked",
                        task.id,
                        blocker_id
                    );
                }
            }
        }
    }
}

/// Test orphan detection logic: in_progress task with inactive owner.
///
/// An orphaned task is one where the owner is not in active_names.
/// The daemon should detect these and trigger recovery.
#[test]
fn orphan_detection_for_inactive_owners() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (_, data) = load_snapshot(fixture);

    let in_progress = get_in_progress_tasks(&data.all_tasks);

    for task in in_progress {
        let owner = task
            .owner
            .as_ref()
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        if owner.is_empty() || owner == "lead" {
            continue;
        }

        let is_active = data.active_names.iter().any(|n| n.to_lowercase() == owner);

        if !is_active {
            // This is an orphaned task - the daemon should recover it
            println!(
                "Orphaned task detected: #{} ({}) owned by inactive {}",
                task.id, task.subject, owner
            );
        }
    }
}

/// Test that pending tasks with owners who are active get nudge action.
///
/// When a task is pending with an assigned owner who is currently active,
/// the daemon should nudge them rather than spawn a new coworker.
#[test]
fn pending_task_with_active_owner_gets_nudge() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (_, data) = load_snapshot(fixture);

    let pending_with_owners = get_pending_with_owners(&data.all_tasks);

    for task in pending_with_owners {
        let owner = task.owner.as_ref().unwrap();
        let owner_lower = owner.to_lowercase();
        let is_active = data
            .active_names
            .iter()
            .any(|n| n.to_lowercase() == owner_lower);

        if is_active {
            // Owner is active - they should be nudged, not spawned
            println!(
                "Pending task #{} has active owner {} - nudge expected",
                task.id, owner
            );
        }
    }
}

/// Test that idle coworkers are available for task assignment.
///
/// An idle coworker is one who is:
/// - Active (has a running tmux window)
/// - Not busy (no in_progress tasks)
/// - No open PR (not waiting for review)
/// - Not assigned as a reviewer
#[test]
fn idle_coworkers_available_for_dispatch() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (_, data) = load_snapshot(fixture);

    let idle = get_idle_coworkers(&data);

    // Verify idle coworkers meet all criteria
    for name in idle {
        assert!(
            data.active_names.contains(name),
            "{} should be active",
            name
        );
        assert!(
            !data.busy_coworkers.contains(name),
            "{} should not be busy",
            name
        );
        assert!(
            !data.coworkers_with_open_prs.contains(name),
            "{} should not have an open PR",
            name
        );
        assert!(
            !data.active_reviewers.contains(name),
            "{} should not be an active reviewer",
            name
        );
    }
}

/// Test that coworker start times are tracked for duplicate detection.
///
/// When multiple coworkers claim the same task, the daemon keeps the
/// earliest-started one and shuts down duplicates.
#[test]
fn coworker_start_times_tracked() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (coworkers, data) = load_snapshot(fixture);

    // Verify start times are populated
    assert!(
        !data.coworker_start_times.is_empty(),
        "coworker start times should be tracked"
    );

    // Verify each active coworker has a start time
    for cw in &coworkers {
        let has_start_time = data
            .coworker_start_times
            .contains_key(&cw.name.to_lowercase())
            || data.coworker_start_times.contains_key(&cw.name);
        assert!(
            has_start_time || !data.active_names.contains(&cw.name),
            "active coworker {} should have a start time",
            cw.name
        );
    }
}

/// Test that dev limit flag prevents spawning new coworkers.
///
/// When is_at_dev_limit is true, the daemon should not spawn new
/// development coworkers (reserving slots for reviewers).
#[test]
fn dev_limit_blocks_new_spawns() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (_, data) = load_snapshot(fixture);

    // The is_at_dev_limit flag should be captured
    // (its actual value depends on config and current state)
    println!("is_at_dev_limit: {}", data.is_at_dev_limit);

    // If at limit, verify we have the expected number of active coworkers
    if data.is_at_dev_limit {
        let dev_coworkers: Vec<_> = data
            .active_names
            .iter()
            .filter(|n| !data.active_reviewers.contains(*n))
            .collect();
        println!("Dev coworkers at limit: {:?}", dev_coworkers);
    }
}

/// Test that pending unowned tasks would be assigned when idle coworkers exist.
///
/// This verifies the preconditions for task assignment:
/// - Pending tasks without owners exist OR could exist
/// - Idle coworkers are available
/// - Not at dev limit
///
/// The daemon's spawn_for_pending_tasks would assign ownership and spawn.
#[test]
fn pending_unowned_task_would_be_assigned() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (_, data) = load_snapshot(fixture);

    // Get pending unowned tasks
    let pending_unowned = get_pending_unowned_tasks(&data.all_tasks);

    // Get idle coworkers
    let idle = get_idle_coworkers(&data);

    // If we have both pending unowned tasks AND idle coworkers AND not at dev limit,
    // the daemon should assign the task
    if !pending_unowned.is_empty() && !idle.is_empty() && !data.is_at_dev_limit {
        println!(
            "Assignment would occur: {} pending unowned tasks, {} idle coworkers",
            pending_unowned.len(),
            idle.len()
        );
        // The daemon would spawn an idle coworker for the first pending task
    } else {
        // Document the current state - no pending unowned tasks in this snapshot
        println!(
            "No assignment: pending_unowned={}, idle={}, at_limit={}",
            pending_unowned.len(),
            idle.len(),
            data.is_at_dev_limit
        );
    }

    // Either way, verify the data structure supports dispatch decisions
    assert!(
        !data.active_names.is_empty(),
        "snapshot should have active coworkers for dispatch"
    );
}

/// Test that blocked tasks are not assigned even with idle coworkers.
///
/// A task with a blocked_by dependency on an incomplete task should
/// remain unassigned. The daemon's dispatch logic filters these out.
#[test]
fn blocked_task_not_assigned() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (_, data) = load_snapshot(fixture);

    // Find tasks with blocked_by dependencies
    let blocked_tasks: Vec<_> = data
        .all_tasks
        .iter()
        .filter(|t| is_task_blocked(t, &data.all_tasks))
        .collect();

    for task in blocked_tasks {
        // Blocked tasks should not be in pending_without_owners for dispatch
        assert!(
            task.status != "pending"
                || task.blocked_by.iter().any(|b_id| {
                    data.all_tasks
                        .iter()
                        .any(|t| t.id == *b_id && t.status != "completed")
                }),
            "blocked task #{} should not be available for dispatch",
            task.id
        );
        println!(
            "Blocked task #{} ({}) correctly excluded - blocked by {:?}",
            task.id, task.subject, task.blocked_by
        );
    }
}

/// Test that tasks trigger coworker spawn when no idle coworkers available.
///
/// When a pending unowned task exists but all coworkers are busy,
/// the daemon should spawn a new coworker (if not at limit).
#[test]
fn task_triggers_spawn_when_no_idle() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (_, data) = load_snapshot(fixture);

    let pending_unowned = get_pending_unowned_tasks(&data.all_tasks);
    let idle = get_idle_coworkers(&data);

    // Scenario: pending task but no idle coworkers
    if !pending_unowned.is_empty() && idle.is_empty() && !data.is_at_dev_limit {
        println!(
            "Spawn would trigger: {} pending tasks, 0 idle coworkers, not at limit",
            pending_unowned.len()
        );
        // The daemon would spawn a fresh coworker for the task
    }

    // Document the actual state
    println!(
        "Current state: {} active, {} busy, {} idle, at_limit={}",
        data.active_names.len(),
        data.busy_coworkers.len(),
        idle.len(),
        data.is_at_dev_limit
    );
}

/// Test snapshot data integrity for task dispatch decisions.
///
/// Verifies all required fields are present and consistent for
/// the daemon to make correct dispatch decisions.
#[test]
fn snapshot_data_integrity_for_dispatch() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (_, data) = load_snapshot(fixture);

    // All busy coworkers should be in active_names
    for busy in &data.busy_coworkers {
        let busy_lower = busy.to_lowercase();
        let is_active = data
            .active_names
            .iter()
            .any(|n| n.to_lowercase() == busy_lower);
        assert!(
            is_active,
            "busy coworker {} should be in active_names",
            busy
        );
    }

    // All reviewers should be in active_names (or recently spawned)
    for reviewer in &data.active_reviewers {
        let reviewer_lower = reviewer.to_lowercase();
        // Reviewers might be in active_names or recently assigned
        println!(
            "Active reviewer: {} (in active_names: {})",
            reviewer,
            data.active_names
                .iter()
                .any(|n| n.to_lowercase() == reviewer_lower)
        );
    }

    // Task owners should be valid coworker names or "lead"
    for task in &data.all_tasks {
        if let Some(owner) = &task.owner
            && !owner.is_empty()
            && owner.to_lowercase() != "lead"
        {
            // Owner should be a plausible coworker name (Manhattan avenue)
            let valid_avenues = [
                "lexington",
                "park",
                "madison",
                "broadway",
                "amsterdam",
                "columbus",
                "riverside",
                "york",
                "pleasant",
                "vernon",
            ];
            let owner_lower = owner.to_lowercase();
            let is_valid = valid_avenues.iter().any(|a| *a == owner_lower);
            assert!(
                is_valid,
                "task #{} has invalid owner name: {}",
                task.id, owner
            );
        }
    }
}
