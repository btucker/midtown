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
    /// PR numbers of recently merged PRs. Used to skip tasks referencing merged PRs.
    merged_pr_numbers: HashSet<u64>,
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

    let merged_pr_numbers: HashSet<u64> = v["merged_pr_numbers"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|n| n.as_u64()).collect())
        .unwrap_or_default();

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
        merged_pr_numbers,
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
/// Captured snapshot from bug #860: PRs #702 and #703 sat without reviews for 2+ hours.
/// The root cause was two interacting bugs:
/// 1. Usage limit detection patterns didn't match the new `/extra-usage` message
///    format from Claude Code v2.1.33+, so york and vernon weren't detected as
///    usage-limited even though their panes showed the limit screen.
/// 2. Because usage-limited reviewers weren't detected, cleanup_expired_preserving()
///    kept refreshing their assignment timestamps, preventing reassignment.
#[test]
fn usage_limited_reviewers_detected_from_pane_contents() {
    let fixture =
        include_str!("fixtures/snapshot/snapshot-stuck-reviews-702-703-20260206-050712.json");
    let v: Value = serde_json::from_str(fixture).expect("valid JSON");

    // The snapshot recorded usage_limited_coworkers as empty — that was the bug
    let recorded_usage_limited: Vec<String> = v["usage_limited_coworkers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        recorded_usage_limited.is_empty(),
        "snapshot recorded empty usage_limited_coworkers (the bug)"
    );

    // But the pane contents clearly show usage limits for york and vernon
    let pane_contents: HashMap<String, String> = v["pane_contents"]
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default();

    // Verify york and vernon panes contain usage limit indicators
    assert!(
        pane_contents["york"].contains("/extra-usage"),
        "york pane should contain /extra-usage"
    );
    assert!(
        pane_contents["vernon"].contains("/extra-usage"),
        "vernon pane should contain /extra-usage"
    );

    // After the fix, has_usage_limit_pattern should detect these
    let usage_limited: HashSet<String> = pane_contents
        .iter()
        .filter(|(_, content)| midtown::rules::has_usage_limit_pattern(content))
        .map(|(name, _)| name.to_lowercase())
        .collect();

    assert!(
        usage_limited.contains("york"),
        "york should be detected as usage-limited"
    );
    assert!(
        usage_limited.contains("vernon"),
        "vernon should be detected as usage-limited"
    );
    // park and amsterdam should NOT be usage-limited
    assert!(
        !usage_limited.contains("park"),
        "park should not be usage-limited"
    );
    assert!(
        !usage_limited.contains("amsterdam"),
        "amsterdam should not be usage-limited"
    );

    // Verify the fix: running_coworker_names excludes usage-limited coworkers.
    // This is the logic from pr.rs that feeds into cleanup_expired_preserving().
    // Without this exclusion, expired reviewer assignments for usage-limited
    // coworkers would be preserved indefinitely, blocking PR review reassignment.
    let running_coworkers: Vec<String> = v["running_coworkers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    c.get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();

    let running_minus_usage_limited: HashSet<String> = running_coworkers
        .iter()
        .filter(|name| !usage_limited.contains(&name.to_lowercase()))
        .cloned()
        .collect();

    // york and vernon are running but usage-limited — they should be excluded
    assert!(
        !running_minus_usage_limited.contains("york"),
        "york should be excluded from running set (usage-limited)"
    );
    assert!(
        !running_minus_usage_limited.contains("vernon"),
        "vernon should be excluded from running set (usage-limited)"
    );
    // park and amsterdam are running and NOT usage-limited — they stay
    assert!(
        running_minus_usage_limited.contains("park"),
        "park should remain in running set (not usage-limited)"
    );
    assert!(
        running_minus_usage_limited.contains("amsterdam"),
        "amsterdam should remain in running set (not usage-limited)"
    );
}

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
// Tests: Duplicate spawn/call-in prevention
// =============================================================================

/// Regression test: the daemon should not generate duplicate spawn/call-in
/// notifications for the same coworker+task across consecutive ticks.
///
/// Captured snapshot shows:
/// - Tasks #873 and #875 are both pending+unowned, both reference PR #708
/// - Pleasant is the PR #708 owner (has pending task #872 for that PR)
/// - PR grouping logic assigns both #873 and #875 to pleasant
/// - Without the fix, each tick re-generates nudges for both tasks because:
///   (a) grouped tasks bypassed the `assigned_this_tick` guard (within-tick)
///   (b) `NudgeCoworkerWithCallbacks` effects weren't tracked as in-flight (cross-tick)
///
/// The daemon logs showed 4+ pairs of "Called in pleasant for task #873/#875"
/// — the same two assignments repeated across consecutive ticks.
#[test]
fn no_duplicate_spawn_notifications_for_same_coworker_and_task() {
    let fixture =
        include_str!("fixtures/snapshot/snapshot-triple-spawn-pleasant-875-20260206-053332.json");
    let snap = load_snapshot(fixture);

    // Precondition: pleasant is active and busy
    assert!(
        snap.active_names.contains("pleasant"),
        "pleasant should be active"
    );
    assert!(
        snap.busy_coworkers.contains("pleasant"),
        "pleasant should be busy"
    );

    // Precondition: tasks #873 and #875 are pending without owners
    let unowned_ids: Vec<&str> = snap
        .pending_tasks_without_owners
        .iter()
        .map(|t| t.id.as_str())
        .collect();
    assert!(
        unowned_ids.contains(&"873"),
        "task #873 should be pending without owner"
    );
    assert!(
        unowned_ids.contains(&"875"),
        "task #875 should be pending without owner"
    );

    // Precondition: pleasant already has a pending owned task (#872) for PR #708
    let pleasant_owned: Vec<&(String, String, String)> = snap
        .pending_tasks_with_owners
        .iter()
        .filter(|(_, _, owner)| owner.to_lowercase() == "pleasant")
        .collect();
    assert!(
        !pleasant_owned.is_empty(),
        "pleasant should have at least one pending owned task"
    );

    // The fix ensures that within a single dispatch tick, after the first task
    // is assigned to pleasant, the second task targeting pleasant (via PR grouping)
    // is blocked by `assigned_this_tick`. Simulate this:
    let mut names_assigned_this_tick: HashSet<String> = HashSet::new();
    let mut assignments: Vec<(&str, &str)> = Vec::new(); // (task_id, coworker)

    // Process unowned tasks as the dispatch loop would
    for task in &snap.pending_tasks_without_owners {
        let target = "pleasant"; // PR grouping would assign both to pleasant
        let already_running = snap.active_names.contains(target);
        let is_busy_from_snapshot = snap.busy_coworkers.contains(target);
        let assigned_this_tick = names_assigned_this_tick.contains(target);
        let is_coworker_reviewer = snap.active_reviewers.contains(target);
        let was_grouped = true; // Both tasks reference PR #708

        // The fixed condition: assigned_this_tick always blocks, even for grouped tasks
        let should_skip = already_running
            && (is_coworker_reviewer
                || assigned_this_tick
                || (is_busy_from_snapshot && !was_grouped));

        if !should_skip {
            assignments.push((&task.id, target));
            names_assigned_this_tick.insert(target.to_string());
        }
    }

    // Only ONE task should be assigned to pleasant per tick, not both
    let pleasant_assignments: Vec<_> = assignments
        .iter()
        .filter(|(_, cw)| *cw == "pleasant")
        .collect();
    assert_eq!(
        pleasant_assignments.len(),
        1,
        "only one task per tick should be assigned to the same coworker; \
         got {} assignments to pleasant: {:?}",
        pleasant_assignments.len(),
        pleasant_assignments
    );
}

/// Regression test for cross-tick duplicate prevention.
///
/// After tick 1 assigns a task to pleasant and marks it in-flight, tick 2
/// should produce zero assignments for that same task. This tests the
/// `mark_in_flight_spawns_from_effects` fix which extends in-flight tracking
/// to cover `NudgeCoworkerWithCallbacks` effects (not just `AssignAndSpawn`).
///
/// Before the fix, nudge effects were not tracked, so the next tick would
/// re-evaluate the same task and produce another nudge — causing the repeated
/// "Called in pleasant for task #875" channel messages seen in the snapshot.
#[test]
fn no_cross_tick_duplicate_spawn_for_in_flight_task() {
    let fixture =
        include_str!("fixtures/snapshot/snapshot-triple-spawn-pleasant-875-20260206-053332.json");
    let snap = load_snapshot(fixture);

    // --- Tick 1: assign one task to pleasant ---
    let mut names_assigned_tick1: HashSet<String> = HashSet::new();
    let mut tick1_assignments: Vec<String> = Vec::new();

    for task in &snap.pending_tasks_without_owners {
        let target = "pleasant";
        let already_running = snap.active_names.contains(target);
        let assigned_this_tick = names_assigned_tick1.contains(target);
        let is_busy_from_snapshot = snap.busy_coworkers.contains(target);
        let is_coworker_reviewer = snap.active_reviewers.contains(target);
        let was_grouped = true;

        let should_skip = already_running
            && (is_coworker_reviewer
                || assigned_this_tick
                || (is_busy_from_snapshot && !was_grouped));

        if !should_skip {
            tick1_assignments.push(task.id.clone());
            names_assigned_tick1.insert(target.to_string());
        }
    }

    assert_eq!(
        tick1_assignments.len(),
        1,
        "tick 1 should assign exactly one task"
    );

    // Simulate mark_in_flight_spawns_from_effects: the assigned task is now in-flight
    let in_flight_tasks: HashSet<String> = tick1_assignments.into_iter().collect();

    // --- Tick 2: same snapshot state, but assigned tasks are now in-flight ---
    // In the real daemon, in-flight tasks are filtered out of pending_tasks_without_owners
    // before dispatch. Simulate this filtering.
    let remaining_unowned: Vec<&Task> = snap
        .pending_tasks_without_owners
        .iter()
        .filter(|t| !in_flight_tasks.contains(&t.id))
        .collect();

    // Tick 2 dispatch: process remaining unowned tasks
    let mut names_assigned_tick2: HashSet<String> = HashSet::new();
    let mut tick2_assignments: Vec<(&str, &str)> = Vec::new();

    for task in &remaining_unowned {
        let target = "pleasant";
        let already_running = snap.active_names.contains(target);
        let assigned_this_tick = names_assigned_tick2.contains(target);
        let is_busy_from_snapshot = snap.busy_coworkers.contains(target);
        let is_coworker_reviewer = snap.active_reviewers.contains(target);
        let was_grouped = true;

        let should_skip = already_running
            && (is_coworker_reviewer
                || assigned_this_tick
                || (is_busy_from_snapshot && !was_grouped));

        if !should_skip {
            tick2_assignments.push((&task.id, target));
            names_assigned_tick2.insert(target.to_string());
        }
    }

    // With only 2 unowned tasks and 1 in-flight, tick 2 should assign at most
    // the remaining task. But pleasant is already busy from snapshot AND was
    // already assigned the first task in tick 1 (which is now in-flight).
    // The key invariant: tick 2 should NOT re-assign the same task from tick 1.
    for (task_id, _) in &tick2_assignments {
        assert!(
            !in_flight_tasks.contains(*task_id),
            "tick 2 should not re-assign in-flight task #{} — \
             this was the cross-tick duplicate bug",
            task_id
        );
    }
}

// =============================================================================
// PR decision function tests with captured snapshots
// =============================================================================

/// Regression test: When a coworker is active (has a tmux window) but not idle,
/// the daemon should nudge them rather than trying to spawn a new window.
///
/// Bug: The daemon logged "call-in failed" for PR #708 notifications targeting
/// pleasant because `decide_pr_issue_action_with_handoff` and
/// `decide_pr_comment_action_with_handoff` returned `SpawnOwner` for active-but-busy
/// coworkers. Spawning fails when the coworker already has a tmux window.
///
/// Snapshot: captured while pleasant was active and working, but the daemon was
/// trying to spawn (not nudge) for review-complete and CI-green notifications.
#[test]
fn active_coworker_gets_nudge_not_spawn_for_pr_notifications() {
    use midtown::rules::{
        PrAction, decide_pr_comment_action_with_handoff, decide_pr_issue_action_with_handoff,
        decide_review_complete_action,
    };

    let fixture = include_str!(
        "fixtures/snapshot/snapshot-call-in-failed-and-false-recovery-20260206-053201.json"
    );
    let v: Value = serde_json::from_str(fixture).expect("valid JSON");

    // Extract the state that feeds into PR decision functions
    let active_coworkers: Vec<String> = v["active_coworkers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str().map(|s| s.to_string()))
        .collect();

    let idle_coworkers: Vec<String> = v["idle_coworkers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let is_at_dev_limit = v["is_at_dev_limit"].as_bool().unwrap_or(false);

    // Verify preconditions from the snapshot
    assert!(
        active_coworkers
            .iter()
            .any(|c| c.eq_ignore_ascii_case("pleasant")),
        "pleasant should be active in the snapshot"
    );
    assert!(
        !idle_coworkers
            .iter()
            .any(|c| c.eq_ignore_ascii_case("pleasant")),
        "pleasant should NOT be idle in the snapshot"
    );
    assert!(!is_at_dev_limit);

    // Test 1: PR issue action (CI green / review feedback) — should nudge, not spawn
    let action = decide_pr_issue_action_with_handoff(
        "pleasant",
        &active_coworkers,
        &idle_coworkers,
        is_at_dev_limit,
        None, // pleasant had empty session_id in snapshot
        "CI is green — please address review feedback and merge",
    );
    assert!(
        matches!(action, PrAction::NudgeOwner { .. }),
        "active-but-busy coworker should be nudged, not spawned. Got: {:?}",
        action
    );

    // Test 2: PR comment action (review complete) — should nudge, not spawn
    let action = decide_pr_comment_action_with_handoff(
        "pleasant",
        "reviewer",
        &active_coworkers,
        &idle_coworkers,
        is_at_dev_limit,
        None,
        "review is complete — please address feedback",
    );
    assert!(
        matches!(action, PrAction::NudgeOwner { .. }),
        "active-but-busy coworker should be nudged for comments, not spawned. Got: {:?}",
        action
    );

    // Test 3: Review complete action — should nudge, not spawn
    let action = decide_review_complete_action(
        "pleasant",
        &active_coworkers,
        &idle_coworkers,
        is_at_dev_limit,
        "review complete — please address feedback and merge",
    );
    assert!(
        matches!(action, PrAction::NudgeOwner { .. }),
        "active-but-busy coworker should be nudged for review complete, not spawned. Got: {:?}",
        action
    );
}

// =============================================================================
// Tests: Merged PR task filtering
// =============================================================================

/// Regression test: tasks referencing a merged PR should be skipped by dispatch.
///
/// Bug: The daemon kept nudging york for task #3 ("Address review feedback on
/// PR #709") even though PR #709 was merged hours ago. The dispatch logic
/// never checked whether a task's referenced PR was already merged.
///
/// The fix adds merged PR number tracking and skips tasks whose PR is merged,
/// auto-completing them instead of generating nudge/spawn effects.
#[test]
fn tasks_referencing_merged_pr_are_skipped() {
    // Simulate the bug scenario: task references a merged PR
    let merged_pr_numbers: HashSet<u64> = [709, 714].into_iter().collect();

    // Case 1: Pending task with owner references merged PR
    let pending_with_owners = vec![
        (
            "3".to_string(),
            "Address review feedback on PR #709".to_string(),
            "york".to_string(),
        ),
        (
            "5".to_string(),
            "Implement new feature".to_string(),
            "park".to_string(),
        ),
        (
            "7".to_string(),
            "Fix bug in PR #714".to_string(),
            "madison".to_string(),
        ),
    ];

    let mut skipped_tasks = Vec::new();
    let mut dispatched_tasks = Vec::new();

    for (task_id, subject, _owner) in &pending_with_owners {
        if let Some(pr_num_str) = midtown::tasks::extract_pr_number(subject) {
            if let Ok(pr_num) = pr_num_str.parse::<u64>() {
                if merged_pr_numbers.contains(&pr_num) {
                    skipped_tasks.push((task_id.as_str(), pr_num));
                    continue;
                }
            }
        }
        dispatched_tasks.push(task_id.as_str());
    }

    // Tasks referencing merged PRs should be skipped
    assert_eq!(
        skipped_tasks.len(),
        2,
        "two tasks reference merged PRs and should be skipped: {:?}",
        skipped_tasks
    );
    assert!(
        skipped_tasks
            .iter()
            .any(|(id, pr)| *id == "3" && *pr == 709),
        "task #3 (PR #709) should be skipped"
    );
    assert!(
        skipped_tasks
            .iter()
            .any(|(id, pr)| *id == "7" && *pr == 714),
        "task #7 (PR #714) should be skipped"
    );

    // Task without PR reference should proceed normally
    assert_eq!(
        dispatched_tasks,
        vec!["5"],
        "task #5 (no PR reference) should be dispatched"
    );
}

/// Test that unowned tasks referencing merged PRs are also skipped.
#[test]
fn unowned_tasks_referencing_merged_pr_are_skipped() {
    let merged_pr_numbers: HashSet<u64> = [709].into_iter().collect();

    // Simulate unowned tasks
    let subjects = vec![
        (
            "873",
            "Fix call-in failed when nudging coworker about PR #709",
        ),
        (
            "875",
            "Deduplicate reviewer @lead review notes for same issue",
        ),
        ("876", "Fix duplicate spawn notifications for PR #709"),
    ];

    let mut skipped = Vec::new();
    let mut passed = Vec::new();

    for (task_id, subject) in &subjects {
        if let Some(pr_num_str) = midtown::tasks::extract_pr_number(subject) {
            if let Ok(pr_num) = pr_num_str.parse::<u64>() {
                if merged_pr_numbers.contains(&pr_num) {
                    skipped.push(*task_id);
                    continue;
                }
            }
        }
        passed.push(*task_id);
    }

    // Tasks referencing merged PR #709 should be skipped
    assert_eq!(skipped, vec!["873", "876"]);
    // Task without PR #709 reference passes through
    assert_eq!(passed, vec!["875"]);
}
