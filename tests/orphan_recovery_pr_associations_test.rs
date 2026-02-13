// Integration test for orphan recovery checking pr_task_associations
//
// Bug: should_recover_task() only checks task.pr (explicit PR field) but doesn't
// check pr_task_associations (the task-to-PR mapping from PrAuthorSession).
//
// Scenario: A coworker opens PR #1000 for task !42. The PrAuthorSession tracks this
// as pr_task_associations[1000] = "42". The task.pr field may or may not be set yet.
// The coworker crashes. Orphan recovery should NOT spawn a new coworker because
// the task already has an open PR tracked via pr_task_associations.
//
// Expected: should_recover_task() returns false (skip recovery)
// Actual (before fix): returns true, spawning duplicate coworkers

use midtown::tasks::{Task, TaskStatus};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Helper to create a minimal task for testing
fn create_test_task(id: &str, subject: &str, pr: Option<u64>) -> Task {
    Task {
        id: id.to_string(),
        subject: subject.to_string(),
        description: None,
        status: TaskStatus::InProgress,
        owner: Some("lexington".to_string()),
        blocked_by: vec![],
        channel: None,
        pr,
        created_at: None,
    }
}

#[test]
fn test_should_not_recover_task_with_open_pr_in_pr_task_associations() {
    // Scenario: Task !42 has an open PR #1000 tracked via pr_task_associations
    // The task.pr field is None (not set yet, or never set)
    let task = create_test_task("42", "Add auth endpoint", None);

    let merged_pr_numbers = HashSet::new(); // PR #1000 is NOT merged
    let mut tasks_with_open_prs = HashMap::new();
    tasks_with_open_prs.insert("42".to_string(), 1000u64); // Task 42 → PR 1000

    let repo_path = PathBuf::from("/fake/repo");

    // Expected: Should NOT recover because task has an open PR via pr_task_associations
    let result = midtown::daemon::should_recover_task_test_helper(
        &task,
        &merged_pr_numbers,
        &repo_path,
        &tasks_with_open_prs,
        &HashMap::new(),
    );

    assert!(
        !result,
        "should_recover_task should return false when task has open PR via pr_task_associations"
    );
}

#[test]
fn test_should_not_recover_task_with_explicit_pr_field_and_pr_task_associations() {
    // Scenario: Task !42 has PR #1000 via BOTH explicit pr field AND pr_task_associations
    let task = create_test_task("42", "Add auth endpoint", Some(1000));

    let merged_pr_numbers = HashSet::new(); // PR #1000 is NOT merged
    let mut tasks_with_open_prs = HashMap::new();
    tasks_with_open_prs.insert("42".to_string(), 1000u64);

    let repo_path = PathBuf::from("/fake/repo");

    // Expected: Should NOT recover (either check would prevent it)
    let result = midtown::daemon::should_recover_task_test_helper(
        &task,
        &merged_pr_numbers,
        &repo_path,
        &tasks_with_open_prs,
        &HashMap::new(),
    );

    assert!(
        !result,
        "should_recover_task should return false when task has open PR via both explicit pr field and pr_task_associations"
    );
}

#[test]
fn test_should_recover_task_when_pr_is_merged() {
    // Scenario: Task !42 had PR #1000, but it's now merged
    // The task should NOT be recovered (explicit PR merged → skip recovery)
    let task = create_test_task("42", "Add auth endpoint", Some(1000));

    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(1000); // PR #1000 is MERGED

    let tasks_with_open_prs = HashMap::new(); // No open PRs (1000 is merged, cleaned up)

    let repo_path = PathBuf::from("/fake/repo");

    // Expected: Should NOT recover because explicit PR is merged
    let result = midtown::daemon::should_recover_task_test_helper(
        &task,
        &merged_pr_numbers,
        &repo_path,
        &tasks_with_open_prs,
        &HashMap::new(),
    );

    assert!(
        !result,
        "should_recover_task should return false when task's explicit PR is merged"
    );
}

#[test]
fn test_should_recover_task_with_no_pr_anywhere() {
    // Scenario: Task !42 has no PR (investigation task, or hasn't opened PR yet)
    let task = create_test_task("42", "Investigate auth bug", None);

    let merged_pr_numbers = HashSet::new();
    let tasks_with_open_prs = HashMap::new(); // No PR for this task

    let repo_path = PathBuf::from("/fake/repo");

    // Expected: Should recover (no PR to prevent it)
    let result = midtown::daemon::should_recover_task_test_helper(
        &task,
        &merged_pr_numbers,
        &repo_path,
        &tasks_with_open_prs,
        &HashMap::new(),
    );

    assert!(
        result,
        "should_recover_task should return true for tasks with no PR association"
    );
}

#[test]
fn test_should_recover_task_when_pr_task_associations_stale_but_pr_merged() {
    // Scenario: Task !42 is in pr_task_associations (pointing to PR #1000),
    // BUT PR #1000 is actually merged. This can happen if pr_author_sessions
    // cleanup is async and hasn't run yet, leaving a stale entry.
    // Expected: Recovery SHOULD proceed (the PR is merged, just not cleaned up yet)
    let task = create_test_task("42", "Add auth endpoint", None);

    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(1000); // PR #1000 is MERGED

    let mut tasks_with_open_prs = HashMap::new();
    tasks_with_open_prs.insert("42".to_string(), 1000u64); // Stale entry

    let repo_path = PathBuf::from("/fake/repo");

    // Expected: Should recover (PR is merged, stale association doesn't block)
    let result = midtown::daemon::should_recover_task_test_helper(
        &task,
        &merged_pr_numbers,
        &repo_path,
        &tasks_with_open_prs,
        &HashMap::new(),
    );

    assert!(
        result,
        "should_recover_task should return true when pr_task_associations is stale (PR merged)"
    );
}
