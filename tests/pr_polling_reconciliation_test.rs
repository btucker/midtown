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
#[ignore] // TODO: Complete test once snapshot is available
async fn polling_spawns_reviewers_after_restart() {
    // Load snapshot captured after daemon restart
    // let fixture = include_str!("fixtures/snapshot/snapshot-review-spawn-lost-after-restart-20260217-XXXXXX.json");
    // let snap: WorldSnapshot = serde_json::from_str(fixture)
    //     .expect("Failed to deserialize WorldSnapshot from fixture");

    // Verify preconditions from snapshot:
    // - Multiple PRs in open_prs_data with headRefName populated
    // - Worktree registry has active assignments for these PR branches
    // - prs_needing_review > 0
    // - Some PR authors not in active_names (simulates restart scenario)

    // Create mock DaemonState for testing
    // (This is complex - may need helper function or simplified mock)

    // Call collect_reviewer_effects_with_source with:
    // - branch_owners_map from snap.worktree_branch_owners
    // - worktree_registry from snap.worktree_registry
    // - mock state
    // - open_prs_data as PR array

    // Verify:
    // - effects.len() > 0 (reviewers are spawned)
    // - effects contain SpawnCoworkerWithCallbacks for each unreviewed PR
    // - No PR is incorrectly skipped as "orphaned" when it has an active worktree

    todo!("Waiting for corrected snapshot from lead");
}
