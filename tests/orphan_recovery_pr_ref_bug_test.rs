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
// Expected: Only skip recovery when the task has an ASSOCIATED PR (one with
// [Midtown !{task_id}] in its title) that is merged.

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
        created_at: None,
    }
}

#[test]
fn test_should_recover_task_with_contextual_pr_mention() {
    // Task !1142 mentions PR #940 in context ("PR #940 fix insufficient")
    // but is NOT completed by PR #940. PR #940 is merged.
    let task = create_test_task(
        "1142",
        "Fix remaining orphan worktree false positives — PR #940 fix insufficient",
        Some("Task !1131 was completed (PR #940 merged) but orphan worktree false positives are STILL firing...".to_string()),
    );

    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(940); // PR #940 is merged

    let repo_path = PathBuf::from("/fake/repo");

    // BUG: Current implementation returns false (skip recovery) because it extracts
    // PR #940 from the subject and sees it's merged.
    // EXPECTED: Should return true (allow recovery) because PR #940 doesn't have
    // [Midtown !1142] in its title — it's just mentioned as context.

    // This test will FAIL with the current buggy implementation
    let result = midtown::daemon::should_recover_task_test_helper(
        &task,
        &merged_pr_numbers,
        &HashSet::new(), // no open PR associations for task 1142
        &HashMap::new(), // no merged PR associations for task 1142
        &repo_path,
    );

    assert!(
        result,
        "should_recover_task should return true for task with contextual PR mention, not false"
    );
}

#[test]
fn test_should_skip_recovery_when_task_pr_is_merged() {
    // Task !1131 has PR #940 with [Midtown !1131] in title
    let task = create_test_task("1131", "Fix orphan worktree detector false positives", None);

    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(940);

    // PR #940 has [Midtown !1131] in its title
    let mut merged_pr_task_associations = HashMap::new();
    merged_pr_task_associations.insert(940, "1131".to_string());

    let repo_path = PathBuf::from("/fake/repo");

    // Should skip recovery because PR #940 (associated with task !1131) is merged
    let result = midtown::daemon::should_recover_task_test_helper(
        &task,
        &merged_pr_numbers,
        &HashSet::new(),
        &merged_pr_task_associations,
        &repo_path,
    );

    assert!(
        !result,
        "should_recover_task should return false when task's associated PR is merged"
    );
}

#[test]
fn test_should_recover_when_task_pr_is_open() {
    // Task !1142 has PR #958 with [Midtown !1142] in title, but it's still open
    let task = create_test_task(
        "1142",
        "Fix remaining orphan worktree false positives",
        None,
    );

    let merged_pr_numbers = HashSet::new(); // PR #958 NOT merged

    // PR #958 is open and associated with task !1142
    let mut open_pr_task_associations = HashSet::new();
    open_pr_task_associations.insert("1142".to_string());

    let repo_path = PathBuf::from("/fake/repo");

    // Should allow recovery because PR #958 is open (not merged yet)
    // The coworker might have crashed/disconnected mid-work
    let result = midtown::daemon::should_recover_task_test_helper(
        &task,
        &merged_pr_numbers,
        &open_pr_task_associations,
        &HashMap::new(),
        &repo_path,
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

    // Should allow recovery for non-PR tasks
    let result = midtown::daemon::should_recover_task_test_helper(
        &task,
        &merged_pr_numbers,
        &HashSet::new(),
        &HashMap::new(),
        &repo_path,
    );

    assert!(
        result,
        "should_recover_task should return true for tasks with no PR"
    );
}
