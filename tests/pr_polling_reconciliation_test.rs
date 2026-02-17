//! Integration test for PR polling reconciliation after daemon restart.
//!
//! Bug: After daemon restart, polling doesn't reconcile unreviewed PRs
//! when their task worktrees are marked completed but the PRs are still open.
//!
//! Root cause: collect_reviewer_effects marks PRs as "orphaned" if their
//! worktree has `completed_at` set, even when the PR is still open and
//! can receive review feedback.
//!
//! Fix: PRs with open status should get reviewers spawned regardless of
//! worktree completion status. The author can still address feedback by
//! pushing to the branch.

use midtown::daemon::snapshot::WorldSnapshot;

/// Test that the captured snapshot has the right preconditions for the bug:
/// - PR #1166 is open, not draft, needs review
/// - Task worktree is completed
/// - No reviewer assigned
///
/// The actual logic test (calling collect_reviewer_effects_with_source) is in
/// src/daemon/pr_tests.rs::test_completed_worktree_with_open_pr_gets_reviewer
#[test]
fn snapshot_preconditions_for_completed_worktree_reviewer_bug() {
    let fixture = include_str!(
        "fixtures/snapshot/snapshot-review-spawn-lost-after-restart-20260217-003046.json"
    );
    let snap: WorldSnapshot =
        serde_json::from_str(fixture).expect("Failed to deserialize WorldSnapshot from fixture");

    // Find PR #1166
    let pr_1166 = snap
        .open_prs_data
        .iter()
        .find(|pr| pr.get("number").and_then(|n| n.as_u64()) == Some(1166))
        .expect("Snapshot should contain PR #1166");

    // Verify preconditions
    let is_draft = pr_1166
        .get("isDraft")
        .and_then(|d| d.as_bool())
        .unwrap_or(false);
    assert!(!is_draft, "PR #1166 should not be a draft");

    assert!(
        !snap.reviewed_prs.contains(&1166),
        "PR #1166 should not be reviewed yet"
    );

    // Find the task worktree for task 1323
    let task_worktree = snap
        .worktree_registry
        .all_assignments()
        .values()
        .find(|a| a.task_id.as_deref() == Some("1323"))
        .expect("Snapshot should contain worktree for task 1323");

    // Verify the worktree is marked completed
    assert!(
        task_worktree.completed_at.is_some(),
        "Task 1323 worktree should be marked completed in the snapshot"
    );

    // Verify no reviewer is assigned to this PR
    let has_reviewer = snap.reviewer_pr_assignments.values().any(|&pr| pr == 1166);
    assert!(
        !has_reviewer,
        "PR #1166 should not have a reviewer assigned yet"
    );
}
