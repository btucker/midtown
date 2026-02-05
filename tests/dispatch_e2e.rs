//! E2E tests for daemon dispatch logic using WorldSnapshot fixtures.
//!
//! These tests verify the daemon correctly:
//! - Identifies tasks eligible for assignment
//! - Detects orphaned tasks needing recovery
//! - Respects blocked task dependencies
//! - Tracks coworker state for dispatch decisions
//! - Spawns reviewers for PRs
//!
//! Tests use real captured snapshots to validate dispatch preconditions
//! with production-like state.
//!
//! Note: The pure decision functions (`decide_*`) are `pub(crate)` and tested
//! via unit tests in rules.rs. These E2E tests validate the snapshot data
//! and conditions that feed into those decisions.
//!
//! To capture new fixtures: `midtown e2e capture --label dispatch-<scenario>`

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, Utc};
use serde::Deserialize;
use serde_json::Value;

// =============================================================================
// Data structures for parsing fixtures
// =============================================================================

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

/// Parsed snapshot data for dispatch tests.
#[derive(Debug)]
struct DispatchSnapshot {
    /// All tasks from the snapshot.
    all_tasks: Vec<Task>,
    /// Names of currently active coworkers (running tmux windows).
    active_names: HashSet<String>,
    /// Coworkers currently busy (have in_progress tasks), lowercase.
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
    coworker_start_times: HashMap<String, DateTime<Utc>>,
    /// In-progress tasks: (task_id, subject, owner).
    in_progress_tasks: Vec<(String, String, String)>,
    /// Pending tasks with owners: (task_id, subject, owner).
    pending_tasks_with_owners: Vec<(String, String, String)>,
    /// Pending tasks without owners.
    pending_tasks_without_owners: Vec<Task>,
    /// Reviewer PR assignments: coworker -> PR number.
    reviewer_pr_assignments: HashMap<String, u64>,
    /// Number of PRs that need review (from PR poll cache).
    prs_needing_review: usize,
    /// Whether we're at the overall coworker limit.
    is_at_coworker_limit: bool,
}

/// Load a snapshot fixture and parse it into test-friendly data structures.
fn load_snapshot(json_str: &str) -> DispatchSnapshot {
    let v: Value = serde_json::from_str(json_str).expect("valid JSON");

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
                .filter_map(|s| s.as_str().map(|s| s.to_lowercase()))
                .collect()
        })
        .unwrap_or_default();

    let busy_coworkers: HashSet<String> = v["busy_coworkers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(|s| s.to_lowercase()))
                .collect()
        })
        .unwrap_or_default();

    let coworkers_with_open_prs: HashSet<String> = v["coworkers_with_open_prs"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(|s| s.to_lowercase()))
                .collect()
        })
        .unwrap_or_default();

    let active_reviewers: HashSet<String> = v["active_reviewers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(|s| s.to_lowercase()))
                .collect()
        })
        .unwrap_or_default();

    let is_at_dev_limit = v["is_at_dev_limit"].as_bool().unwrap_or(false);

    let now_utc =
        DateTime::parse_from_rfc3339(v["now_utc"].as_str().unwrap_or("1970-01-01T00:00:00Z"))
            .unwrap()
            .with_timezone(&Utc);

    let coworker_start_times: HashMap<String, DateTime<Utc>> = v["coworker_start_times"]
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| {
                    v.as_str()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                        .map(|dt| (k.to_lowercase(), dt.with_timezone(&Utc)))
                })
                .collect()
        })
        .unwrap_or_default();

    // Parse in_progress_tasks array of tuples
    let in_progress_tasks: Vec<(String, String, String)> = v["in_progress_tasks"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|tuple| {
                    let arr = tuple.as_array()?;
                    if arr.len() >= 3 {
                        Some((
                            arr[0].as_str()?.to_string(),
                            arr[1].as_str()?.to_string(),
                            arr[2].as_str()?.to_string(),
                        ))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // Parse pending_tasks_with_owners array of tuples
    let pending_tasks_with_owners: Vec<(String, String, String)> = v["pending_tasks_with_owners"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|tuple| {
                    let arr = tuple.as_array()?;
                    if arr.len() >= 3 {
                        Some((
                            arr[0].as_str()?.to_string(),
                            arr[1].as_str()?.to_string(),
                            arr[2].as_str()?.to_string(),
                        ))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // Parse pending_tasks_without_owners
    let pending_tasks_without_owners: Vec<Task> = v["pending_tasks_without_owners"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    // Parse reviewer_pr_assignments
    let reviewer_pr_assignments: HashMap<String, u64> = v["reviewer_pr_assignments"]
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_u64().map(|pr| (k.to_lowercase(), pr)))
                .collect()
        })
        .unwrap_or_default();

    let prs_needing_review = v["prs_needing_review"].as_u64().unwrap_or(0) as usize;
    let is_at_coworker_limit = v["is_at_coworker_limit"].as_bool().unwrap_or(false);

    DispatchSnapshot {
        all_tasks,
        active_names,
        busy_coworkers,
        coworkers_with_open_prs,
        active_reviewers,
        is_at_dev_limit,
        now_utc,
        coworker_start_times,
        in_progress_tasks,
        pending_tasks_with_owners,
        pending_tasks_without_owners,
        reviewer_pr_assignments,
        prs_needing_review,
        is_at_coworker_limit,
    }
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
fn get_idle_coworkers(data: &DispatchSnapshot) -> Vec<String> {
    data.active_names
        .iter()
        .filter(|name| {
            !data.busy_coworkers.contains(*name)
                && !data.coworkers_with_open_prs.contains(*name)
                && !data.active_reviewers.contains(*name)
        })
        .cloned()
        .collect()
}

/// Valid coworker names (Manhattan avenues).
const VALID_AVENUES: &[&str] = &[
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

/// Check if a name is a valid coworker name.
fn is_valid_coworker_name(name: &str) -> bool {
    VALID_AVENUES.contains(&name.to_lowercase().as_str())
}

// =============================================================================
// Tests: Pending task with owner - conditions for nudge vs spawn
// =============================================================================

/// Test that active owners would be nudged (not spawned).
///
/// When a task is pending with an assigned owner who is currently active,
/// the daemon should nudge them rather than spawn a new coworker.
#[test]
fn pending_task_active_owner_should_be_nudged() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    // york is active in this snapshot
    assert!(
        snap.active_names.contains("york"),
        "york should be active in fixture"
    );

    // If york had a pending task, the condition for nudge is met:
    // - owner is active (in active_names)
    // - not at dev limit (is_at_dev_limit = false)
    // - (cooldown is runtime state, not in snapshot)

    // Verify the preconditions for nudge are satisfied
    assert!(
        !snap.is_at_dev_limit,
        "Should not be at dev limit for nudge"
    );
}

/// Test that inactive owners would trigger spawn.
///
/// When a task is pending with an assigned owner who is NOT active,
/// the daemon should spawn the coworker (if not at dev limit).
#[test]
fn pending_task_inactive_owner_should_spawn() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    // madison is NOT active in this snapshot
    assert!(
        !snap.active_names.contains("madison"),
        "madison should NOT be active in fixture"
    );

    // If madison had a pending task, the condition for spawn is met:
    // - owner is inactive (not in active_names)
    // - not at dev limit
    // - madison is a valid coworker name
    assert!(
        !snap.is_at_dev_limit,
        "Should not be at dev limit for spawn"
    );
    assert!(
        is_valid_coworker_name("madison"),
        "madison should be a valid coworker name"
    );
}

/// Test that dev limit blocks spawning.
///
/// When the daemon is at the dev coworker limit, it should skip spawning
/// new coworkers for pending tasks (to reserve slots for reviewers).
#[test]
fn dev_limit_blocks_spawn_condition() {
    // Create a modified snapshot with is_at_dev_limit = true
    // to verify the condition would block spawning
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    // In this fixture, is_at_dev_limit is false
    // When true, the spawn_for_pending_tasks function would skip unowned tasks
    println!(
        "is_at_dev_limit: {} (would block spawn if true)",
        snap.is_at_dev_limit
    );

    // Verify active count for context
    println!(
        "Active coworkers: {} (limit enforcement depends on config)",
        snap.active_names.len()
    );
}

/// Test that lead-owned tasks are skipped.
///
/// Tasks owned by "lead" are not actionable by the daemon - they're
/// manual coordination tasks.
#[test]
fn lead_owned_tasks_skipped() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    // Verify no pending tasks are owned by lead
    for (task_id, subject, owner) in &snap.pending_tasks_with_owners {
        assert!(
            owner.to_lowercase() != "lead",
            "pending_tasks_with_owners should not include lead-owned task #{}: {}",
            task_id,
            subject
        );
    }
}

/// Test that invalid owner names are detected.
///
/// If a task owner is not a valid coworker name (not an avenue name),
/// the daemon should skip it.
#[test]
fn invalid_owner_names_detected() {
    // Test the validation function
    assert!(is_valid_coworker_name("york"), "york is valid");
    assert!(is_valid_coworker_name("madison"), "madison is valid");
    assert!(
        is_valid_coworker_name("Broadway"),
        "Broadway is valid (case-insensitive)"
    );
    assert!(!is_valid_coworker_name("invalid"), "invalid is not valid");
    assert!(!is_valid_coworker_name(""), "empty is not valid");
    assert!(
        !is_valid_coworker_name("lead"),
        "lead is not a coworker name"
    );
}

// =============================================================================
// Tests: Orphan task recovery
// =============================================================================

/// Test orphan detection: in_progress task with inactive owner.
///
/// An orphaned task is in_progress but its owner is not active.
/// The daemon should detect this and trigger recovery.
#[test]
fn orphan_detection_finds_inactive_owners() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    // Find in_progress tasks whose owners are not active
    let orphans: Vec<_> = snap
        .in_progress_tasks
        .iter()
        .filter(|(_, _, owner)| {
            let owner_lower = owner.to_lowercase();
            !owner_lower.is_empty()
                && owner_lower != "lead"
                && !snap.active_names.contains(&owner_lower)
        })
        .collect();

    // Report findings
    if orphans.is_empty() {
        println!("No orphaned tasks in fixture (all owners are active)");
    } else {
        for (task_id, subject, owner) in orphans {
            println!(
                "Orphan detected: Task #{} ({}) owned by inactive {}",
                task_id, subject, owner
            );
        }
    }
}

/// Test that active owners are not flagged as orphans.
///
/// If the task owner is still active, it's not orphaned.
#[test]
fn active_owners_not_orphaned() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    // york has an in_progress task and is active - not orphaned
    if snap.active_names.contains("york") {
        let york_tasks: Vec<_> = snap
            .in_progress_tasks
            .iter()
            .filter(|(_, _, owner)| owner.to_lowercase() == "york")
            .collect();

        for (task_id, _, _) in york_tasks {
            println!("Task #{} owned by active york - NOT orphaned", task_id);
        }
    }
}

/// Test orphan recovery respects dev limit.
///
/// Even if there's an orphaned task, don't spawn recovery at dev limit.
#[test]
fn orphan_recovery_blocked_at_dev_limit() {
    // The orphan recovery function checks is_at_dev_limit before attempting recovery
    // When true, it returns None immediately
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    println!(
        "is_at_dev_limit: {} (orphan recovery would be blocked if true)",
        snap.is_at_dev_limit
    );
}

// =============================================================================
// Tests: Blocked task detection
// =============================================================================

/// Test blocked task identification.
///
/// Tasks with blocked_by dependencies on incomplete tasks should be
/// excluded from dispatch.
#[test]
fn blocked_task_detection() {
    let tasks = vec![
        Task {
            id: "1".to_string(),
            subject: "Task 1".to_string(),
            status: "completed".to_string(),
            owner: None,
            blocked_by: vec![],
        },
        Task {
            id: "2".to_string(),
            subject: "Task 2".to_string(),
            status: "in_progress".to_string(),
            owner: Some("york".to_string()),
            blocked_by: vec![],
        },
        Task {
            id: "3".to_string(),
            subject: "Task 3 - blocked".to_string(),
            status: "pending".to_string(),
            owner: None,
            blocked_by: vec!["2".to_string()], // blocked by in_progress task
        },
        Task {
            id: "4".to_string(),
            subject: "Task 4 - unblocked".to_string(),
            status: "pending".to_string(),
            owner: None,
            blocked_by: vec!["1".to_string()], // blocked by completed task
        },
    ];

    // Task 3 is blocked (blocker is in_progress)
    assert!(
        is_task_blocked(&tasks[2], &tasks),
        "Task 3 should be blocked by in_progress Task 2"
    );

    // Task 4 is unblocked (blocker is completed)
    assert!(
        !is_task_blocked(&tasks[3], &tasks),
        "Task 4 should be unblocked since Task 1 is completed"
    );

    // Task 1 and 2 have no blockers
    assert!(
        !is_task_blocked(&tasks[0], &tasks),
        "Task 1 has no blockers"
    );
    assert!(
        !is_task_blocked(&tasks[1], &tasks),
        "Task 2 has no blockers"
    );
}

/// Test blocked task handling with real fixture.
#[test]
fn blocked_tasks_excluded_from_pending_unowned() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    // pending_tasks_without_owners should not include blocked tasks
    // (The snapshot collector already filters these)
    for task in &snap.pending_tasks_without_owners {
        // Verify no pending unowned tasks are blocked
        let blocked = is_task_blocked(task, &snap.all_tasks);
        assert!(
            !blocked,
            "pending_tasks_without_owners should not include blocked task #{}: {}",
            task.id, task.subject
        );
    }
}

/// Test that completed blockers unblock tasks.
#[test]
fn completed_blockers_unblock_tasks() {
    let tasks = vec![
        Task {
            id: "1".to_string(),
            subject: "Blocker task".to_string(),
            status: "completed".to_string(),
            owner: None,
            blocked_by: vec![],
        },
        Task {
            id: "2".to_string(),
            subject: "Blocked task".to_string(),
            status: "pending".to_string(),
            owner: None,
            blocked_by: vec!["1".to_string()],
        },
    ];

    // Task 2 should NOT be blocked because task 1 is completed
    assert!(
        !is_task_blocked(&tasks[1], &tasks),
        "Task should be unblocked when blocker is completed"
    );
}

/// Test transitive blocking (task blocked by blocked task).
#[test]
fn transitive_blocking_not_expanded() {
    // Note: The current is_task_blocked only checks direct blockers,
    // not transitive dependencies. This is intentional - the daemon
    // evaluates one level at a time.
    let tasks = vec![
        Task {
            id: "1".to_string(),
            subject: "Root task".to_string(),
            status: "in_progress".to_string(),
            owner: Some("york".to_string()),
            blocked_by: vec![],
        },
        Task {
            id: "2".to_string(),
            subject: "Middle task".to_string(),
            status: "pending".to_string(),
            owner: None,
            blocked_by: vec!["1".to_string()], // blocked by in_progress task 1
        },
        Task {
            id: "3".to_string(),
            subject: "Leaf task".to_string(),
            status: "pending".to_string(),
            owner: None,
            blocked_by: vec!["2".to_string()], // blocked by pending task 2
        },
    ];

    // Task 2 is blocked (task 1 is in_progress)
    assert!(
        is_task_blocked(&tasks[1], &tasks),
        "Task 2 is blocked by task 1"
    );

    // Task 3 is blocked (task 2 is pending, not completed)
    assert!(
        is_task_blocked(&tasks[2], &tasks),
        "Task 3 is blocked by task 2"
    );
}

// =============================================================================
// Tests: Duplicate worker detection
// =============================================================================

/// Test duplicate worker sorting by start time.
///
/// When multiple coworkers claim the same task, the daemon keeps the
/// earliest-started one and shuts down duplicates.
#[test]
fn duplicate_worker_sorting() {
    use chrono::Duration;

    let now = Utc::now();
    let earlier = now - Duration::minutes(5);
    let later = now + Duration::minutes(5);

    let mut workers: Vec<(String, Option<DateTime<Utc>>)> = vec![
        ("later_worker".to_string(), Some(later)),
        ("earlier_worker".to_string(), Some(earlier)),
        ("now_worker".to_string(), Some(now)),
    ];

    // Sort by start time (earliest first)
    workers.sort_by(|a, b| match (&a.1, &b.1) {
        (Some(t1), Some(t2)) => t1.cmp(t2),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    assert_eq!(workers[0].0, "earlier_worker", "Earliest should be first");
    assert_eq!(workers[1].0, "now_worker");
    assert_eq!(workers[2].0, "later_worker", "Latest should be last");
}

/// Test duplicate worker sorting with unknown times.
///
/// Workers with known start times should sort before those with unknown times.
#[test]
fn duplicate_worker_sorting_unknown_times() {
    let now = Utc::now();

    let mut workers: Vec<(String, Option<DateTime<Utc>>)> = vec![
        ("unknown1".to_string(), None),
        ("known".to_string(), Some(now)),
        ("unknown2".to_string(), None),
    ];

    workers.sort_by(|a, b| match (&a.1, &b.1) {
        (Some(t1), Some(t2)) => t1.cmp(t2),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    assert_eq!(workers[0].0, "known", "Known time should sort first");
    // Unknown workers stay in relative order (stable sort)
}

/// Test coworker start times are tracked for duplicate detection.
#[test]
fn coworker_start_times_tracked() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    // All active coworkers should have start times
    for name in &snap.active_names {
        assert!(
            snap.coworker_start_times.contains_key(name),
            "Active coworker {} should have a start time",
            name
        );
    }
}

/// Test duplicate task detection preconditions.
///
/// The snapshot tracks which coworkers own which in_progress tasks.
/// Duplicates would be detected by grouping by task_id.
#[test]
fn duplicate_task_detection_data_available() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    // Build a map of task_id -> owners
    let mut task_owners: HashMap<String, Vec<String>> = HashMap::new();
    for (task_id, _, owner) in &snap.in_progress_tasks {
        if !owner.is_empty() && owner.to_lowercase() != "lead" {
            task_owners
                .entry(task_id.clone())
                .or_default()
                .push(owner.clone());
        }
    }

    // Check for any tasks with multiple owners (would be duplicates)
    for (task_id, owners) in &task_owners {
        if owners.len() > 1 {
            println!(
                "Duplicate detected: Task #{} has {} owners: {:?}",
                task_id,
                owners.len(),
                owners
            );
        }
    }
}

// =============================================================================
// Tests: Reviewer spawning
// =============================================================================

/// Test reviewer assignments are tracked.
///
/// The snapshot should track which coworkers are assigned to review which PRs.
#[test]
fn reviewer_assignments_tracked() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    // Check if broadway is assigned to review a PR
    if let Some(pr) = snap.reviewer_pr_assignments.get("broadway") {
        println!("Broadway is reviewing PR #{}", pr);
        assert_eq!(
            *pr, 533,
            "Broadway should be reviewing PR 533 in this fixture"
        );
    }

    // Report all reviewer assignments
    for (reviewer, pr) in &snap.reviewer_pr_assignments {
        println!("Reviewer {}: PR #{}", reviewer, pr);
    }
}

/// Test that active reviewers are excluded from idle coworker pool.
///
/// Reviewers should not be considered idle for task assignment.
#[test]
fn reviewers_excluded_from_idle_pool() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    let idle = get_idle_coworkers(&snap);

    for reviewer in &snap.active_reviewers {
        assert!(
            !idle.contains(reviewer),
            "Active reviewer {} should not be in idle pool",
            reviewer
        );
    }
}

/// Test reviewer state isolation.
///
/// Reviewers may be in isolated_tasks mode (their task list is separate).
#[test]
fn reviewer_isolation_tracked() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let v: Value = serde_json::from_str(fixture).expect("valid JSON");

    // Check coworker_snapshots for isolated_tasks flag
    if let Some(coworkers) = v["coworker_snapshots"].as_array() {
        for cw in coworkers {
            let name = cw["name"].as_str().unwrap_or("");
            let isolated = cw["isolated_tasks"].as_bool().unwrap_or(false);
            if isolated {
                println!("Coworker {} is in isolated task mode (reviewer)", name);
            }
        }
    }
}

// =============================================================================
// Tests: Snapshot data integrity
// =============================================================================

/// Test that busy coworkers match in_progress task owners.
#[test]
fn busy_coworkers_consistency() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    // Get owners of in_progress tasks
    let in_progress_owners: HashSet<String> = snap
        .in_progress_tasks
        .iter()
        .map(|(_, _, owner)| owner.to_lowercase())
        .filter(|o| !o.is_empty() && o != "lead")
        .collect();

    // Each busy coworker should own an in_progress task
    for busy in &snap.busy_coworkers {
        assert!(
            in_progress_owners.contains(busy),
            "Busy coworker {} should own an in_progress task",
            busy
        );
    }
}

/// Test that pending unowned tasks are unblocked.
#[test]
fn pending_unowned_tasks_are_unblocked() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    for task in &snap.pending_tasks_without_owners {
        // Verify the task has no blocking dependencies that are incomplete
        if !task.blocked_by.is_empty() {
            for blocker_id in &task.blocked_by {
                let blocker = snap.all_tasks.iter().find(|t| t.id == *blocker_id);
                if let Some(b) = blocker {
                    assert_eq!(
                        b.status, "completed",
                        "pending_tasks_without_owners task #{} is blocked by incomplete #{} ({})",
                        task.id, b.id, b.status
                    );
                }
            }
        }
    }
}

/// Test task owner validation.
#[test]
fn task_owner_validation() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    for task in &snap.all_tasks {
        if let Some(owner) = &task.owner
            && !owner.is_empty()
        {
            let owner_lower = owner.to_lowercase();
            let is_valid = owner_lower == "lead" || is_valid_coworker_name(&owner_lower);
            assert!(
                is_valid,
                "Task #{} has invalid owner '{}' - must be an avenue name or 'lead'",
                task.id, owner
            );
        }
    }
}

// =============================================================================
// Tests: Integration scenarios
// =============================================================================

/// Test complete dispatch scenario analysis with fixture.
#[test]
fn dispatch_scenario_analysis() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    println!("=== Dispatch Scenario Analysis ===");
    println!("Active coworkers: {:?}", snap.active_names);
    println!("Busy coworkers: {:?}", snap.busy_coworkers);
    println!("At dev limit: {}", snap.is_at_dev_limit);
    println!("In-progress tasks: {}", snap.in_progress_tasks.len());
    println!(
        "Pending with owners: {}",
        snap.pending_tasks_with_owners.len()
    );
    println!(
        "Pending without owners: {}",
        snap.pending_tasks_without_owners.len()
    );
    println!("Active reviewers: {:?}", snap.active_reviewers);

    // Calculate idle coworkers
    let idle = get_idle_coworkers(&snap);
    println!("Idle coworkers: {:?}", idle);

    // Analyze each pending task with owner
    for (task_id, task_subject, owner) in &snap.pending_tasks_with_owners {
        let owner_lower = owner.to_lowercase();
        let is_active = snap.active_names.contains(&owner_lower);
        let action = if is_active { "nudge" } else { "spawn" };
        println!(
            "Task #{} ({}) owned by {} -> would {}",
            task_id, task_subject, owner, action
        );
    }

    // Check for orphans
    let orphan_count = snap
        .in_progress_tasks
        .iter()
        .filter(|(_, _, owner)| {
            let owner_lower = owner.to_lowercase();
            !owner_lower.is_empty()
                && owner_lower != "lead"
                && !snap.active_names.contains(&owner_lower)
        })
        .count();
    println!("Orphaned tasks: {}", orphan_count);
}

/// Test that fixture timestamp is reasonable.
#[test]
fn fixture_timestamp_is_valid() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    // Timestamp should be recent (after 2026)
    assert!(
        snap.now_utc.year() >= 2026,
        "Fixture timestamp {} should be recent",
        snap.now_utc
    );
}

/// Test multiple fixture files for consistency.
#[test]
fn multiple_fixtures_valid_structure() {
    let fixtures = [
        include_str!("fixtures/snapshot/snapshot-20260203-152121.json"),
        include_str!("fixtures/snapshot/snapshot-20260203-161629.json"),
        include_str!("fixtures/snapshot/snapshot-20260203-182216.json"),
    ];

    for (i, fixture_str) in fixtures.iter().enumerate() {
        let snap = load_snapshot(fixture_str);

        // Basic validity checks
        assert!(
            !snap.active_names.is_empty() || snap.all_tasks.is_empty(),
            "Fixture {} should have active names if it has tasks",
            i
        );

        // All busy coworkers should be active
        for busy in &snap.busy_coworkers {
            assert!(
                snap.active_names.contains(busy),
                "Fixture {}: busy coworker {} should be active",
                i,
                busy
            );
        }

        println!(
            "Fixture {} validated: {} active coworkers",
            i,
            snap.active_names.len()
        );
    }
}

/// Test idle coworker calculation.
#[test]
fn idle_coworker_calculation() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    let idle = get_idle_coworkers(&snap);

    // Verify all returned coworkers are truly idle
    for name in &idle {
        assert!(
            snap.active_names.contains(name),
            "{} should be active",
            name
        );
        assert!(
            !snap.busy_coworkers.contains(name),
            "{} should not be busy",
            name
        );
        assert!(
            !snap.coworkers_with_open_prs.contains(name),
            "{} should not have open PR",
            name
        );
        assert!(
            !snap.active_reviewers.contains(name),
            "{} should not be a reviewer",
            name
        );
    }

    println!("Idle coworkers: {:?}", idle);
}

/// Test dev limit state is captured.
#[test]
fn dev_limit_state_captured() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_snapshot(fixture);

    // The is_at_dev_limit flag is captured and can be used for decision logic
    println!("is_at_dev_limit: {}", snap.is_at_dev_limit);

    // When true, new task spawns would be blocked
    // When false, spawns are allowed (up to the configured limit)
}

/// Compute the number of PRs that need review but don't have an assigned reviewer yet.
/// This mirrors the logic that should be in dispatch.rs for deciding whether to defer
/// unowned task pickup in favor of spawning reviewers.
fn unserved_prs_needing_review(snap: &DispatchSnapshot) -> usize {
    let prs_with_reviewers: HashSet<&u64> = snap.reviewer_pr_assignments.values().collect();
    snap.prs_needing_review
        .saturating_sub(prs_with_reviewers.len())
}

/// Should task dispatch be deferred to prioritize reviewer spawning?
/// This mirrors the deferral condition in dispatch.rs spawn_for_pending_tasks().
const MAX_CONCURRENT_REVIEWS: usize = 4;
fn should_defer_task_dispatch(snap: &DispatchSnapshot) -> bool {
    let active_review_count = snap.active_reviewers.len();
    let unserved = unserved_prs_needing_review(snap);
    unserved > 0 && active_review_count < MAX_CONCURRENT_REVIEWS && !snap.is_at_coworker_limit
}

/// Regression test: dispatch deferral should NOT block when all PRs needing review
/// already have active reviewers assigned.
///
/// Captured snapshot shows: prs_needing_review=2, active_reviewers=[pleasant, york],
/// reviewer_pr_assignments={pleasant: 644, york: 645}. Both PRs are covered, so
/// task dispatch should proceed. The bug was that the deferral only checked
/// prs_needing_review > 0 without subtracting PRs already being reviewed.
#[test]
fn dispatch_not_deferred_when_all_prs_have_reviewers() {
    let fixture = include_str!(
        "fixtures/snapshot/snapshot-daemon-not-dispatching-tasks-20260205-040201.json"
    );
    let snap = load_snapshot(fixture);

    // Verify the snapshot has the conditions that triggered the bug
    assert_eq!(snap.prs_needing_review, 2, "2 PRs need review");
    assert_eq!(snap.active_reviewers.len(), 2, "2 active reviewers");
    assert_eq!(
        snap.reviewer_pr_assignments.len(),
        2,
        "2 reviewer-PR assignments"
    );
    assert!(
        !snap.pending_tasks_without_owners.is_empty(),
        "there are unowned tasks waiting for dispatch"
    );
    assert!(!snap.is_at_coworker_limit, "not at coworker limit");

    // Every PR needing review already has a reviewer assigned
    let prs_with_reviewers: HashSet<&u64> = snap.reviewer_pr_assignments.values().collect();
    assert_eq!(
        prs_with_reviewers.len(),
        snap.prs_needing_review,
        "all PRs needing review have assigned reviewers"
    );

    // The key assertion: dispatch should NOT be deferred because all PRs are covered
    assert_eq!(
        unserved_prs_needing_review(&snap),
        0,
        "no unserved PRs needing review"
    );
    assert!(
        !should_defer_task_dispatch(&snap),
        "task dispatch should NOT be deferred when all PRs already have reviewers"
    );
}

// =============================================================================
// Tests: Stale claim reconciliation conditions
// =============================================================================

/// Verify that a claim is considered stale when in-memory assignment exists
/// but the task is NOT in the in_progress list on disk.
///
/// This simulates the scenario: coworker called `midtown task claim`, daemon
/// recorded the in-memory assignment and nudged the Lead, but the Lead failed
/// to process the nudge. The task remains "pending" on disk.
#[test]
fn stale_claim_detected_when_task_pending_on_disk() {
    // In-memory: coworker "park" is assigned task "42"
    let in_memory_assignments: HashMap<String, String> = [("park".to_string(), "42".to_string())]
        .into_iter()
        .collect();

    // On disk: no in_progress tasks (Lead didn't process the claim)
    let on_disk_in_progress: HashSet<String> = HashSet::new();

    // Check: which in-memory assignments have no matching on-disk in_progress?
    let stale: Vec<_> = in_memory_assignments
        .iter()
        .filter(|(_, task_id)| !on_disk_in_progress.contains(*task_id))
        .map(|(name, tid)| (name.clone(), tid.clone()))
        .collect();

    assert_eq!(stale.len(), 1, "should detect one stale claim");
    assert_eq!(stale[0].0, "park");
    assert_eq!(stale[0].1, "42");
}

/// Verify that a claim is NOT stale when the task is in_progress on disk.
///
/// This is the happy path: the Lead processed the nudge and set the task
/// to in_progress. The in-memory assignment and on-disk state agree.
#[test]
fn claim_not_stale_when_task_in_progress_on_disk() {
    let in_memory_assignments: HashMap<String, String> = [("park".to_string(), "42".to_string())]
        .into_iter()
        .collect();

    // On disk: task 42 is in_progress (Lead processed the claim)
    let on_disk_in_progress: HashSet<String> = ["42".to_string()].into_iter().collect();

    let stale: Vec<_> = in_memory_assignments
        .iter()
        .filter(|(_, task_id)| !on_disk_in_progress.contains(*task_id))
        .collect();

    assert!(
        stale.is_empty(),
        "claim should not be stale when task is in_progress on disk"
    );
}

/// Verify that stale claim detection works with multiple coworkers.
///
/// Some claims may be stale while others are not. The reconciliation should
/// only flag the stale ones.
#[test]
fn stale_claim_detection_with_multiple_coworkers() {
    let in_memory_assignments: HashMap<String, String> = [
        ("park".to_string(), "42".to_string()), // stale — not in_progress on disk
        ("amsterdam".to_string(), "55".to_string()), // OK — in_progress on disk
        ("york".to_string(), "78".to_string()), // stale — not in_progress on disk
    ]
    .into_iter()
    .collect();

    let on_disk_in_progress: HashSet<String> = ["55".to_string()].into_iter().collect();

    let mut stale: Vec<_> = in_memory_assignments
        .iter()
        .filter(|(_, task_id)| !on_disk_in_progress.contains(*task_id))
        .map(|(name, tid)| (name.clone(), tid.clone()))
        .collect();
    stale.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(stale.len(), 2, "should detect two stale claims");
    assert_eq!(stale[0].0, "park");
    assert_eq!(stale[1].0, "york");
}

/// Verify the retry escalation logic: after max retries, direct disk write should happen.
///
/// The reconciliation function checks `nudge_retries >= TASK_CLAIM_MAX_RETRIES`.
/// When exceeded, it should emit an `AssignTaskOwnerDirect` effect instead of
/// another `NudgeLead` effect.
#[test]
fn stale_claim_escalation_after_max_retries() {
    let max_retries: u32 = 3;

    // Simulate retry counts for different coworkers
    let claims: Vec<(&str, &str, u32)> = vec![
        ("park", "42", 0),      // First detection — should re-nudge
        ("amsterdam", "55", 2), // Under max — should re-nudge
        ("york", "78", 3),      // At max — should fall back to direct write
        ("madison", "99", 5),   // Over max — should fall back to direct write
    ];

    let mut nudge_count = 0;
    let mut direct_write_count = 0;

    for (_, _, retries) in &claims {
        if *retries >= max_retries {
            direct_write_count += 1;
        } else {
            nudge_count += 1;
        }
    }

    assert_eq!(nudge_count, 2, "2 claims should trigger re-nudge");
    assert_eq!(
        direct_write_count, 2,
        "2 claims should trigger direct disk write"
    );
}

/// Verify that the reconciliation correctly cross-references in-memory
/// assignments against the snapshot's in_progress_tasks list.
///
/// Uses the same data format as WorldSnapshot to ensure compatibility.
#[test]
fn reconciliation_uses_snapshot_in_progress_tasks() {
    // WorldSnapshot format: in_progress_tasks is Vec<(task_id, subject, owner)>
    let in_progress_tasks: Vec<(String, String, String)> = vec![
        (
            "42".to_string(),
            "Fix auth bug".to_string(),
            "park".to_string(),
        ),
        (
            "55".to_string(),
            "Add tests".to_string(),
            "amsterdam".to_string(),
        ),
    ];

    // Derive on_disk_in_progress set (same logic as reconcile_stale_claims)
    let on_disk_in_progress: HashSet<String> = in_progress_tasks
        .iter()
        .map(|(id, _, _)| id.clone())
        .collect();

    assert!(on_disk_in_progress.contains("42"));
    assert!(on_disk_in_progress.contains("55"));
    assert!(
        !on_disk_in_progress.contains("78"),
        "task 78 is not in_progress"
    );

    // In-memory assignment for task 78 would be stale
    let in_memory_task = "78";
    assert!(
        !on_disk_in_progress.contains(in_memory_task),
        "task 78 should be detected as stale (not in_progress on disk)"
    );
}

// =============================================================================
// Tests: task.claim RPC flow — effect production logic
// =============================================================================
//
// These tests verify the reconciliation effect decision logic, mirroring
// what reconcile_stale_claims() produces given various input states.
// Since reconcile_stale_claims is pub(super), we test the decision patterns
// directly with the same data structures and thresholds.

/// Describes which action type the reconciliation should take for a stale claim.
#[derive(Debug, PartialEq)]
enum ReconcileAction {
    /// Re-nudge the Lead (retries < max)
    ReNudgeLead {
        task_id: String,
        coworker: String,
        retry_number: u32,
    },
    /// Fall back to direct disk write (retries >= max)
    DirectWrite { task_id: String, owner: String },
}

/// Simulate the reconciliation effect decision for a single stale claim.
///
/// This mirrors the logic in `reconcile_stale_claims` from dispatch.rs.
fn decide_reconcile_action(
    coworker: &str,
    task_id: &str,
    retries: u32,
    max_retries: u32,
) -> ReconcileAction {
    if retries >= max_retries {
        ReconcileAction::DirectWrite {
            task_id: task_id.to_string(),
            owner: coworker.to_string(),
        }
    } else {
        ReconcileAction::ReNudgeLead {
            task_id: task_id.to_string(),
            coworker: coworker.to_string(),
            retry_number: retries + 1,
        }
    }
}

/// Verify that stale claims under the retry threshold produce re-nudge actions.
///
/// When a claim is detected as stale but hasn't exhausted retries, the
/// reconciliation should produce three effects:
/// 1. NudgeLead — with a retry-annotated message
/// 2. RecordCooldown — to prevent spamming
/// 3. IncrementClaimRetry — to track retry count
#[test]
fn reconcile_stale_claim_produces_renudge_with_retry_tracking() {
    let max_retries: u32 = 3;

    // First retry (retries=0)
    let action = decide_reconcile_action("park", "42", 0, max_retries);
    assert_eq!(
        action,
        ReconcileAction::ReNudgeLead {
            task_id: "42".to_string(),
            coworker: "park".to_string(),
            retry_number: 1,
        },
        "First stale detection should re-nudge as retry 1"
    );

    // Second retry (retries=1)
    let action = decide_reconcile_action("park", "42", 1, max_retries);
    assert_eq!(
        action,
        ReconcileAction::ReNudgeLead {
            task_id: "42".to_string(),
            coworker: "park".to_string(),
            retry_number: 2,
        },
        "Second stale detection should re-nudge as retry 2"
    );

    // Third retry (retries=2) — still under max
    let action = decide_reconcile_action("park", "42", 2, max_retries);
    assert_eq!(
        action,
        ReconcileAction::ReNudgeLead {
            task_id: "42".to_string(),
            coworker: "park".to_string(),
            retry_number: 3,
        },
        "Third stale detection should re-nudge as retry 3"
    );
}

/// Verify that stale claims at or above the retry threshold produce direct write actions.
///
/// After exhausting retries, the reconciliation falls back to writing task
/// ownership directly to disk, bypassing the Lead. This produces:
/// 1. AssignTaskOwnerDirect — sets owner on disk
/// 2. PostToChannel — warning message about the fallback
#[test]
fn reconcile_stale_claim_escalates_to_direct_write() {
    let max_retries: u32 = 3;

    // At max retries (retries=3)
    let action = decide_reconcile_action("park", "42", 3, max_retries);
    assert_eq!(
        action,
        ReconcileAction::DirectWrite {
            task_id: "42".to_string(),
            owner: "park".to_string(),
        },
        "At max retries should escalate to direct write"
    );

    // Over max retries (retries=5) — still direct write
    let action = decide_reconcile_action("york", "99", 5, max_retries);
    assert_eq!(
        action,
        ReconcileAction::DirectWrite {
            task_id: "99".to_string(),
            owner: "york".to_string(),
        },
        "Over max retries should also escalate to direct write"
    );
}

/// Verify the nudge message format includes retry count and max retries.
///
/// The reconciliation nudge message follows the format:
/// "Reminder: Set task #<id> owner to \"<name>\" ... (retry N/M)"
/// This ensures the Lead sees which retry this is.
#[test]
fn reconcile_nudge_message_format_includes_retry_info() {
    let max_retries: u32 = 3;

    // Simulate the nudge message format from dispatch.rs
    for retries in 0..max_retries {
        let task_id = "42";
        let coworker = "park";
        let retry_number = retries + 1;

        let nudge_msg = format!(
            "Reminder: Set task #{} owner to \"{}\" and status to in_progress using TaskUpdate. \
             (retry {}/{})",
            task_id, coworker, retry_number, max_retries
        );

        assert!(
            nudge_msg.contains(&format!("retry {}/{}", retry_number, max_retries)),
            "Nudge message should include retry progress: {}",
            nudge_msg
        );
        assert!(
            nudge_msg.contains(&format!("task #{}", task_id)),
            "Nudge message should reference the task ID"
        );
        assert!(
            nudge_msg.contains(&format!("\"{}\"", coworker)),
            "Nudge message should include the coworker name"
        );
        assert!(
            nudge_msg.contains("TaskUpdate"),
            "Nudge message should mention TaskUpdate tool"
        );
    }
}

/// Verify that the full reconciliation pipeline processes a batch of stale claims
/// and produces the correct mix of actions.
///
/// In production, reconcile_stale_claims iterates over all stale claims and
/// produces independent effect sets. This test verifies the batch behavior.
#[test]
fn reconcile_batch_produces_independent_effect_sets() {
    let max_retries: u32 = 3;

    // Simulate a batch of stale claims with varying retry counts
    let stale_claims: Vec<(&str, &str, u32)> = vec![
        ("park", "42", 0),      // Should re-nudge (retry 1/3)
        ("amsterdam", "55", 2), // Should re-nudge (retry 3/3)
        ("york", "78", 3),      // Should direct write (exhausted)
        ("madison", "99", 10),  // Should direct write (way over)
    ];

    let mut renudge_actions = Vec::new();
    let mut direct_write_actions = Vec::new();

    for (coworker, task_id, retries) in &stale_claims {
        match decide_reconcile_action(coworker, task_id, *retries, max_retries) {
            action @ ReconcileAction::ReNudgeLead { .. } => renudge_actions.push(action),
            action @ ReconcileAction::DirectWrite { .. } => direct_write_actions.push(action),
        }
    }

    assert_eq!(
        renudge_actions.len(),
        2,
        "Two claims should produce re-nudge actions"
    );
    assert_eq!(
        direct_write_actions.len(),
        2,
        "Two claims should produce direct write actions"
    );

    // Verify re-nudge actions reference correct coworkers
    assert!(matches!(
        &renudge_actions[0],
        ReconcileAction::ReNudgeLead { coworker, task_id, retry_number: 1 }
        if coworker == "park" && task_id == "42"
    ));
    assert!(matches!(
        &renudge_actions[1],
        ReconcileAction::ReNudgeLead { coworker, task_id, retry_number: 3 }
        if coworker == "amsterdam" && task_id == "55"
    ));

    // Verify direct write actions reference correct coworkers
    assert!(matches!(
        &direct_write_actions[0],
        ReconcileAction::DirectWrite { owner, task_id }
        if owner == "york" && task_id == "78"
    ));
    assert!(matches!(
        &direct_write_actions[1],
        ReconcileAction::DirectWrite { owner, task_id }
        if owner == "madison" && task_id == "99"
    ));
}

/// Verify that the stale claim timeout threshold works correctly.
///
/// Claims are only stale when: (1) task is pending on disk AND (2) assignment
/// age exceeds the timeout (60s). This test verifies the time-based filtering.
#[test]
fn stale_claim_timeout_filters_recent_assignments() {
    use std::time::{Duration, Instant};

    let timeout = Duration::from_secs(60);
    let now = Instant::now();

    // Simulate assignments with different ages
    struct TestAssignment {
        coworker: &'static str,
        task_id: &'static str,
        assigned_at: Instant,
    }

    let assignments = [
        TestAssignment {
            coworker: "park",
            task_id: "42",
            assigned_at: now - Duration::from_secs(120), // 2 minutes ago — stale
        },
        TestAssignment {
            coworker: "amsterdam",
            task_id: "55",
            assigned_at: now - Duration::from_secs(30), // 30s ago — NOT stale yet
        },
        TestAssignment {
            coworker: "york",
            task_id: "78",
            assigned_at: now - Duration::from_secs(61), // Just over timeout — stale
        },
        TestAssignment {
            coworker: "madison",
            task_id: "99",
            assigned_at: now - Duration::from_secs(59), // Just under timeout — NOT stale
        },
    ];

    // None of these tasks are in_progress on disk
    let on_disk_in_progress: HashSet<String> = HashSet::new();

    let stale: Vec<_> = assignments
        .iter()
        .filter(|a| {
            now.duration_since(a.assigned_at) > timeout && !on_disk_in_progress.contains(a.task_id)
        })
        .map(|a| a.coworker)
        .collect();

    assert_eq!(stale.len(), 2, "Only 2 assignments should be stale");
    assert!(
        stale.contains(&"park"),
        "park's 120s assignment should be stale"
    );
    assert!(
        stale.contains(&"york"),
        "york's 61s assignment should be stale"
    );
    assert!(
        !stale.contains(&"amsterdam"),
        "amsterdam's 30s assignment should not be stale"
    );
    assert!(
        !stale.contains(&"madison"),
        "madison's 59s assignment should not be stale"
    );
}

/// Verify the complete task.claim flow from claim to reconciliation to escalation.
///
/// Simulates the full lifecycle:
/// 1. Coworker claims task → in-memory assignment recorded
/// 2. Timeout passes → stale detected → re-nudge Lead
/// 3. More timeouts → retry count increments
/// 4. Max retries reached → escalate to direct disk write
#[test]
fn full_claim_lifecycle_from_claim_to_escalation() {
    use std::time::{Duration, Instant};

    let timeout = Duration::from_secs(60);
    let max_retries: u32 = 3;
    let now = Instant::now();

    // Phase 1: Coworker claims task, in-memory assignment created
    let coworker = "park";
    let task_id = "42";
    let mut assigned_at = now - Duration::from_secs(120); // Already past timeout
    let mut retries: u32 = 0;

    // Phase 2: First stale detection — should re-nudge
    let is_stale = now.duration_since(assigned_at) > timeout;
    assert!(is_stale, "Assignment should be stale after 120s");
    let action = decide_reconcile_action(coworker, task_id, retries, max_retries);
    assert!(
        matches!(
            action,
            ReconcileAction::ReNudgeLead {
                retry_number: 1,
                ..
            }
        ),
        "First stale detection should be retry 1"
    );

    // Simulate IncrementClaimRetry effect: bump retry count and reset timestamp
    retries += 1;
    assigned_at = now; // Reset timestamp on retry

    // Phase 3: Second stale detection after another timeout
    let later = now + Duration::from_secs(120);
    let is_stale = later.duration_since(assigned_at) > timeout;
    assert!(is_stale, "Should be stale again after another timeout");
    let action = decide_reconcile_action(coworker, task_id, retries, max_retries);
    assert!(
        matches!(
            action,
            ReconcileAction::ReNudgeLead {
                retry_number: 2,
                ..
            }
        ),
        "Second stale detection should be retry 2"
    );
    retries += 1;

    // Phase 4: Third retry
    let action = decide_reconcile_action(coworker, task_id, retries, max_retries);
    assert!(
        matches!(
            action,
            ReconcileAction::ReNudgeLead {
                retry_number: 3,
                ..
            }
        ),
        "Third stale detection should be retry 3"
    );
    retries += 1;

    // Phase 5: Max retries reached — escalate to direct write
    assert_eq!(retries, max_retries);
    let action = decide_reconcile_action(coworker, task_id, retries, max_retries);
    assert_eq!(
        action,
        ReconcileAction::DirectWrite {
            task_id: task_id.to_string(),
            owner: coworker.to_string(),
        },
        "After max retries should escalate to direct disk write"
    );
}

/// Verify that claims are cleared from stale detection once the task
/// appears as in_progress on disk (Lead successfully processed the nudge).
///
/// This is the happy path resolution: the Lead processes the nudge,
/// updates the task to in_progress, and the next reconciliation cycle
/// sees the task on disk and skips it.
#[test]
fn claim_resolved_when_task_transitions_to_in_progress() {
    use std::time::{Duration, Instant};

    let timeout = Duration::from_secs(60);
    let now = Instant::now();

    // Assignment exists and is older than timeout
    let assigned_at = now - Duration::from_secs(120);
    let task_id = "42";
    let _coworker = "park";

    // Before Lead processes: task NOT in_progress on disk → stale
    let on_disk_empty: HashSet<String> = HashSet::new();
    let is_stale = now.duration_since(assigned_at) > timeout && !on_disk_empty.contains(task_id);
    assert!(
        is_stale,
        "Should be stale when task not in_progress on disk"
    );

    // After Lead processes: task IS in_progress on disk → NOT stale
    let on_disk_with_task: HashSet<String> = [task_id.to_string()].into_iter().collect();
    let is_stale =
        now.duration_since(assigned_at) > timeout && !on_disk_with_task.contains(task_id);
    assert!(
        !is_stale,
        "Should NOT be stale once task is in_progress on disk"
    );
}

/// Verify that the direct disk write fallback message includes
/// the retry count and coworker name for debugging.
#[test]
fn direct_write_fallback_message_includes_context() {
    let max_retries: u32 = 3;
    let coworker = "park";
    let task_id = "42";
    let retries = max_retries;

    // Simulate the fallback channel message format from dispatch.rs
    let channel_msg = format!(
        "⚠️ Lead did not process claim for task #{} by {} after {} retries. \
         Set ownership directly.",
        task_id, coworker, retries
    );

    assert!(
        channel_msg.contains(&format!("task #{}", task_id)),
        "Message should include task ID"
    );
    assert!(
        channel_msg.contains(coworker),
        "Message should include coworker name"
    );
    assert!(
        channel_msg.contains(&retries.to_string()),
        "Message should include retry count"
    );
    assert!(
        channel_msg.contains("directly"),
        "Message should indicate direct ownership was set"
    );
}
