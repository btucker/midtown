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

/// Test that polling generates reviewer spawn effects for unreviewed PRs
/// even when the PR author coworkers are not currently active.
///
/// This test uses a snapshot captured after daemon restart showing:
/// - Multiple open PRs needing review
/// - Worktree registry has assignments for these PRs
/// - Some PR authors are not in the active coworkers list
///
/// Before the fix, collect_reviewer_effects would skip these PRs as "orphaned".
/// After the fix, it should generate spawn effects for all reviewable PRs.
#[tokio::test]
async fn polling_spawns_reviewers_after_restart() {
    // TODO: Load snapshot after lead recaptures with correct headRefName data
    // let fixture = include_str!("fixtures/snapshot/snapshot-review-spawn-lost-after-restart-20260217-XXXXXX.json");
    // let snap: WorldSnapshot = serde_json::from_str(fixture)
    //     .expect("Failed to deserialize WorldSnapshot from fixture");

    // TODO: Call collect_reviewer_effects and verify spawn effects are generated
    // Expected: spawn effects for all 4 PRs that need review
    // Actual before fix: 0 effects (all marked orphaned)

    // Placeholder - will implement after snapshot is ready
    todo!("Waiting for corrected snapshot from lead");
}
