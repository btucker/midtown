// Test for orphan recovery bug where tasks mentioning merged PRs in context
// are incorrectly skipped for recovery (issue #1147).
//
// Bug: should_recover_task() extracts ALL PR numbers from task text and skips
// recovery if those PRs are merged. This treats contextual mentions (e.g.,
// "PR #940 fix insufficient") as if the task was completed by that PR.
//
// Example: Task !1142 subject is "Fix remaining orphan worktree false positives — PR #940 fix insufficient"
// PR #940 is merged, so should_recover_task() returns false, preventing orphan recovery.
//
// Fix: Only skip recovery when the task has an explicit `pr` field pointing to a merged PR.
// Contextual PR mentions in text are ignored.

use midtown::tasks::{Task, TaskStatus};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Helper to create a minimal task for testing
fn create_test_task(id: &str, subject: &str, description: Option<String>) -> Task {
    Task {
        id: id.to_string(),
        subject: subject.to_string(),
        description,
        status: TaskStatus::InProgress,
        owner: Some("amsterdam".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    }
}

#[test]
fn test_should_recover_task_with_contextual_pr_mention() {
    // Task !1142 mentions PR #940 in context ("PR #940 fix insufficient")
    // but is NOT completed by PR #940. PR #940 is merged.
    // The task has no explicit pr field (it will create a different PR).
    let task = create_test_task(
        "1142",
        "Fix remaining orphan worktree false positives — PR #940 fix insufficient",
        Some("Task !1131 was completed (PR #940 merged) but orphan worktree false positives are STILL firing...".to_string()),
    );

    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(940); // PR #940 is merged

    let repo_path = PathBuf::from("/fake/repo");

    // Should return true (allow recovery) because task.pr is None —
    // PR #940 is just mentioned as context, not the task's actual PR.
    let tasks_with_open_prs = HashMap::new(); // No open PRs
    let result = midtown::daemon::should_recover_task_test_helper(
        &task,
        &merged_pr_numbers,
        &repo_path,
        &tasks_with_open_prs,
    );

    assert!(
        result,
        "should_recover_task should return true for task with contextual PR mention (pr field is None)"
    );
}

#[test]
fn test_should_skip_recovery_when_task_pr_is_merged() {
    // Task !1131 has explicit PR association to #940
    let mut task = create_test_task("1131", "Fix orphan worktree detector false positives", None);
    task.pr = Some(940); // Explicit PR association

    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(940);

    let repo_path = PathBuf::from("/fake/repo");

    // Should skip recovery because task.pr = Some(940) and PR #940 is merged
    let tasks_with_open_prs = HashMap::new(); // No open PRs
    let result = midtown::daemon::should_recover_task_test_helper(
        &task,
        &merged_pr_numbers,
        &repo_path,
        &tasks_with_open_prs,
    );

    assert!(
        !result,
        "should_recover_task should return false when task's explicit PR is merged"
    );
}

#[test]
fn test_should_recover_when_task_pr_is_open() {
    // Task !1142 has explicit PR association to #958, but it's still open
    let mut task = create_test_task(
        "1142",
        "Fix remaining orphan worktree false positives",
        None,
    );
    task.pr = Some(958); // Explicit PR association to open PR

    let merged_pr_numbers = HashSet::new(); // PR #958 NOT merged

    let repo_path = PathBuf::from("/fake/repo");

    // Should allow recovery because PR #958 is not in the merged set
    // (and the GitHub API check in tests will fail/return None, so recovery is allowed)
    let tasks_with_open_prs = HashMap::new(); // No open PRs
    let result = midtown::daemon::should_recover_task_test_helper(
        &task,
        &merged_pr_numbers,
        &repo_path,
        &tasks_with_open_prs,
    );

    assert!(
        result,
        "should_recover_task should return true when task's PR is open (coworker may have crashed)"
    );
}

#[test]
fn test_should_recover_task_with_no_pr() {
    // Task with no PR association at all (investigation, review, etc.)
    let task = create_test_task(
        "1100",
        "Investigate CI slowdown",
        Some("Looking into why tests take 5min+ in CI".to_string()),
    );

    let merged_pr_numbers = HashSet::new();

    let repo_path = PathBuf::from("/fake/repo");

    // Should allow recovery for non-PR tasks (pr field is None)
    let tasks_with_open_prs = HashMap::new(); // No open PRs
    let result = midtown::daemon::should_recover_task_test_helper(
        &task,
        &merged_pr_numbers,
        &repo_path,
        &tasks_with_open_prs,
    );

    assert!(
        result,
        "should_recover_task should return true for tasks with no PR"
    );
}
