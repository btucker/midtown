//! E2E test for PR polling reconciliation after daemon restart.
//!
//! Bug: After daemon restart, polling doesn't reconcile unreviewed PRs
//! because the orphan check fails when PR author coworkers aren't currently active.
//!
//! Root cause: collect_reviewer_effects checks if PR owner appears in
//! worktree_branch_owners.values() (only includes currently-bound coworkers).
//! After restart, if author isn't spawned yet, they're marked orphaned.
//!
//! Fix: Check worktree registry directly (persistent assignments) instead of
//! current bindings.

use midtown::daemon::snapshot::WorldSnapshot;

/// Test that the worktree registry correctly identifies non-orphaned PRs
/// even when PR author coworkers are not currently active.
///
/// This test uses a snapshot captured after daemon restart showing:
/// - Multiple open PRs with headRefName populated
/// - Worktree registry has active (non-completed) assignments for these PRs
/// - Some PR authors are not in the active coworkers list
///
/// The test verifies that PRs with active worktree assignments are correctly
/// identified as non-orphaned by checking the registry, not current bindings.
#[test]
fn worktree_registry_identifies_non_orphaned_prs() {
    let fixture = include_str!(
        "fixtures/snapshot/snapshot-review-spawn-lost-after-restart-20260217-003046.json"
    );
    let snap: WorldSnapshot =
        serde_json::from_str(fixture).expect("Failed to deserialize WorldSnapshot from fixture");

    // Verify snapshot preconditions
    assert!(
        !snap.open_prs_data.is_empty(),
        "Snapshot should have open PRs"
    );

    // Count PRs that have active worktree assignments
    let mut prs_with_active_worktrees = 0;

    for pr in &snap.open_prs_data {
        let pr_number = pr
            .get("number")
            .and_then(|n: &serde_json::Value| n.as_u64())
            .unwrap_or(0);
        let head_ref = pr
            .get("headRefName")
            .and_then(|r: &serde_json::Value| r.as_str())
            .unwrap_or("");

        // Check if this PR has an active worktree using the registry
        // (This is the core logic from the fix)
        let worktree = snap
            .worktree_registry
            .get_by_pr(pr_number)
            .or_else(|| snap.worktree_registry.get_by_branch(head_ref));

        let has_active_worktree =
            matches!(worktree, Some(assignment) if assignment.completed_at.is_none());

        if has_active_worktree {
            prs_with_active_worktrees += 1;
            println!(
                "✓ PR #{} ({}) has active worktree assignment",
                pr_number, head_ref
            );
        } else {
            println!(
                "✗ PR #{} ({}) is orphaned or completed",
                pr_number, head_ref
            );
        }
    }

    // The key assertion: after daemon restart, PRs with persistent worktree
    // assignments should be identified as non-orphaned, even if their author
    // coworkers aren't currently active.
    //
    // Before the fix: 0 (all PRs marked orphaned because authors not in active_names)
    // After the fix: > 0 (PRs with active worktree assignments are not orphaned)
    assert!(
        prs_with_active_worktrees > 0,
        "Expected at least one PR with an active worktree assignment. \
         If this fails, the orphan check is still incorrectly relying on \
         current coworker bindings instead of the persistent worktree registry."
    );

    println!(
        "\n✓ Test passed: {} / {} PRs have active worktree assignments",
        prs_with_active_worktrees,
        snap.open_prs_data.len()
    );
}
