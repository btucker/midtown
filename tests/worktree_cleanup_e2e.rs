//! E2E tests for worktree auto-cleanup after task completion.
//!
//! Run with: `cargo test --test worktree_cleanup_e2e -- --ignored --test-threads=1`

use chrono::{Duration, Utc};
use midtown::worktree_registry::{WorktreeAssignment, WorktreeRegistry};
use std::collections::HashSet;

/// Helper function to check for stale worktrees and return their IDs.
///
/// This is a test implementation that mimics what the daemon will do.
/// It checks each worktree assignment for:
/// - Has a completed_at timestamp
/// - Completed more than retention_period ago
/// - Not currently bound to an active coworker
fn check_for_stale_worktrees_test(
    registry: &WorktreeRegistry,
    active_coworkers: &HashSet<String>,
    retention_period: Duration,
) -> Vec<String> {
    let now = Utc::now();
    let mut stale_worktrees = Vec::new();

    for (_, assignment) in registry.all_assignments().iter() {
        // Skip if not completed
        let Some(completed_at) = assignment.completed_at else {
            continue;
        };

        // Skip if within retention period
        let age = now.signed_duration_since(completed_at);
        if age < retention_period {
            continue;
        };

        // Skip if actively in use
        if let Some(ref coworker) = assignment.current_coworker
            && active_coworkers.contains(coworker)
        {
            continue;
        }

        // This worktree should be cleaned up
        stale_worktrees.push(assignment.worktree_id.clone());
    }

    stale_worktrees
}

#[test]
#[ignore]
fn test_cleanup_stale_worktrees_after_24_hours() {
    // Create a registry with a worktree that completed 25 hours ago
    let mut registry = WorktreeRegistry::new();

    let old_completed = Utc::now() - Duration::hours(25);
    let assignment = WorktreeAssignment {
        worktree_id: "task-42-old-work".to_string(),
        branch_name: "task-42-old-work".to_string(),
        task_id: Some("42".to_string()),
        current_coworker: None, // Not actively in use
        pr_number: None,
        created_at: Utc::now() - Duration::hours(30),
        completed_at: Some(old_completed),
    };
    registry.assign_worktree(assignment).unwrap();

    let active_coworkers = HashSet::new();
    let stale = check_for_stale_worktrees_test(&registry, &active_coworkers, Duration::hours(24));

    // Should identify the stale worktree
    assert!(!stale.is_empty(), "Should identify stale worktrees");
    assert!(
        stale.contains(&"task-42-old-work".to_string()),
        "Should identify task-42-old-work as stale"
    );
}

#[test]
#[ignore]
fn test_no_cleanup_for_recent_completions() {
    // Create a registry with a worktree that completed 12 hours ago (within retention)
    let mut registry = WorktreeRegistry::new();

    let recent_completed = Utc::now() - Duration::hours(12);
    let assignment = WorktreeAssignment {
        worktree_id: "task-99-recent-work".to_string(),
        branch_name: "task-99-recent-work".to_string(),
        task_id: Some("99".to_string()),
        current_coworker: None,
        pr_number: None,
        created_at: Utc::now() - Duration::hours(15),
        completed_at: Some(recent_completed),
    };
    registry.assign_worktree(assignment).unwrap();

    let active_coworkers = HashSet::new();
    let stale = check_for_stale_worktrees_test(&registry, &active_coworkers, Duration::hours(24));

    // Should NOT identify stale worktrees
    assert!(
        stale.is_empty(),
        "Should not cleanup worktrees within retention period"
    );
}

#[test]
#[ignore]
fn test_no_cleanup_for_active_coworkers() {
    // Create a registry with a worktree that's old but actively in use
    let mut registry = WorktreeRegistry::new();

    let old_completed = Utc::now() - Duration::hours(30);
    let assignment = WorktreeAssignment {
        worktree_id: "task-77-active".to_string(),
        branch_name: "task-77-active".to_string(),
        task_id: Some("77".to_string()),
        current_coworker: Some("park".to_string()), // Actively in use!
        pr_number: None,
        created_at: Utc::now() - Duration::hours(35),
        completed_at: Some(old_completed),
    };
    registry.assign_worktree(assignment).unwrap();

    // Create a set with this coworker active
    let mut active_coworkers = HashSet::new();
    active_coworkers.insert("park".to_string());

    let stale = check_for_stale_worktrees_test(&registry, &active_coworkers, Duration::hours(24));

    // Should NOT cleanup - coworker is active
    assert!(
        stale.is_empty(),
        "Should not cleanup worktrees with active coworkers"
    );
}

#[test]
#[ignore]
fn test_no_cleanup_for_uncompleted_tasks() {
    // Create a registry with an old worktree but no completion timestamp
    let mut registry = WorktreeRegistry::new();

    let assignment = WorktreeAssignment {
        worktree_id: "task-55-in-progress".to_string(),
        branch_name: "task-55-in-progress".to_string(),
        task_id: Some("55".to_string()),
        current_coworker: None,
        pr_number: None,
        created_at: Utc::now() - Duration::hours(48), // Old creation
        completed_at: None,                           // But not completed
    };
    registry.assign_worktree(assignment).unwrap();

    let active_coworkers = HashSet::new();
    let stale = check_for_stale_worktrees_test(&registry, &active_coworkers, Duration::hours(24));

    // Should NOT cleanup - task hasn't completed
    assert!(
        stale.is_empty(),
        "Should not cleanup worktrees without completion timestamp"
    );
}

#[test]
#[ignore]
fn test_cleanup_multiple_stale_worktrees() {
    // Create multiple old worktrees
    let mut registry = WorktreeRegistry::new();

    let old_time = Utc::now() - Duration::hours(30);

    for i in 1..=3 {
        let assignment = WorktreeAssignment {
            worktree_id: format!("task-{}-old", i),
            branch_name: format!("task-{}-old", i),
            task_id: Some(i.to_string()),
            current_coworker: None,
            pr_number: None,
            created_at: Utc::now() - Duration::hours(35),
            completed_at: Some(old_time),
        };
        registry.assign_worktree(assignment).unwrap();
    }

    let active_coworkers = HashSet::new();
    let stale = check_for_stale_worktrees_test(&registry, &active_coworkers, Duration::hours(24));

    // Should cleanup all 3
    assert_eq!(stale.len(), 3, "Should cleanup all stale worktrees");
}

#[test]
#[ignore]
fn test_review_worktrees_cleanup() {
    // Review worktrees (review-pr-*) should also be cleaned up if the PR is merged and old
    let mut registry = WorktreeRegistry::new();

    let old_time = Utc::now() - Duration::hours(30);
    let assignment = WorktreeAssignment {
        worktree_id: "review-pr-123".to_string(),
        branch_name: "review-pr-123".to_string(),
        task_id: None,
        current_coworker: None,
        pr_number: Some(123),
        created_at: Utc::now() - Duration::hours(35),
        completed_at: Some(old_time), // PR was reviewed and merged long ago
    };
    registry.assign_worktree(assignment).unwrap();

    let active_coworkers = HashSet::new();
    let stale = check_for_stale_worktrees_test(&registry, &active_coworkers, Duration::hours(24));

    // Should cleanup review worktree
    assert!(!stale.is_empty(), "Should cleanup old review worktrees");
    assert!(
        stale.contains(&"review-pr-123".to_string()),
        "Should identify review-pr-123 as stale"
    );
}
