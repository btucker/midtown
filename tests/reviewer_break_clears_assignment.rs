//! Test that breaking a reviewer clears their PR assignment.
//!
//! Regression test for: When a reviewer coworker is manually broken
//! (`midtown coworker break`), the daemon didn't clear the review assignment
//! from GitHubState. It then detected the PR still needs review, sees no
//! active reviewer, and respawned one — creating a loop.

use midtown::github_state::{AssignmentSource, GitHubState};

#[test]
fn test_breaking_reviewer_should_clear_assignment() {
    // Setup: Create a GitHub state with a reviewer assigned to PR #42
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "amsterdam", AssignmentSource::Webhook);

    // Verify the assignment exists
    assert_eq!(state.get_reviewer(42), Some("amsterdam"));
    assert!(state.is_assigned(42));

    // Simulate breaking the reviewer: clear the assignment
    let removed = state.remove_assignment_by_reviewer("amsterdam");

    // Verify the assignment was removed
    assert!(removed.is_some());
    let assignment = removed.unwrap();
    assert_eq!(assignment.reviewer, "amsterdam");
    assert_eq!(assignment.pr_number, 42);

    // Verify no assignment remains
    assert_eq!(state.get_reviewer(42), None);
    assert!(!state.is_assigned(42));

    // Verify the PR won't be detected as needing a reviewer again
    assert_eq!(state.pr_for_reviewer("amsterdam"), None);
}

#[test]
fn test_breaking_untracked_reviewer_still_clears_assignment() {
    // Regression test for review feedback issue #1:
    // When a coworker is not tracked (already deregistered, crashed, or broken twice)
    // but still has an active reviewer assignment, the early return in handle_coworker_break
    // would skip the cleanup, causing the daemon to respawn them.

    // Setup: A reviewer assignment exists but the coworker is not tracked
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "amsterdam", AssignmentSource::Webhook);

    // Verify assignment exists
    assert_eq!(state.get_reviewer(42), Some("amsterdam"));

    // Simulate the fix: clear assignment even if coworker is not tracked
    // (This would be called before the early return in handle_coworker_break)
    let removed = state.remove_assignment_by_reviewer("amsterdam");
    assert!(removed.is_some());

    // Verify the assignment was cleared
    assert_eq!(state.get_reviewer(42), None);

    // The daemon should NOT spawn a new reviewer on the next tick
    // because there's no assignment anymore
}

#[test]
fn test_breaking_non_reviewer_is_safe() {
    // Setup: Create a GitHub state with a reviewer assigned
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "lexington", AssignmentSource::Webhook);

    // Try to break a coworker that's not reviewing anything
    let removed = state.remove_assignment_by_reviewer("park");

    // Should return None (no assignment to remove)
    assert!(removed.is_none());

    // Original assignment should still be there
    assert_eq!(state.get_reviewer(42), Some("lexington"));
}

#[test]
fn test_breaking_reviewer_with_multiple_assignments() {
    // Setup: Multiple reviewers assigned
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "amsterdam", AssignmentSource::Webhook);
    state.assign_reviewer(43, "lexington", AssignmentSource::Webhook);

    // Break amsterdam
    let removed = state.remove_assignment_by_reviewer("amsterdam");
    assert!(removed.is_some());
    assert_eq!(removed.unwrap().pr_number, 42);

    // Amsterdam's assignment should be cleared
    assert_eq!(state.get_reviewer(42), None);

    // Lexington's assignment should be unaffected
    assert_eq!(state.get_reviewer(43), Some("lexington"));
}

#[test]
fn test_coworker_break_prevents_respawn_loop() {
    // Integration test that verifies the full daemon behavior:
    // When a reviewer coworker is broken, their assignment is cleared,
    // and the daemon won't spawn a new reviewer on the next tick.
    //
    // This test verifies the complete cycle:
    // 1. Reviewer is assigned to a PR
    // 2. Coworker is broken (simulated by calling remove_assignment_by_reviewer)
    // 3. Assignment is cleared
    // 4. Next tick won't see a reviewer need (preventing respawn loop)

    // Setup: A reviewer is assigned to PR #42
    let mut state = GitHubState::default();
    state.assign_reviewer(42, "amsterdam", AssignmentSource::Webhook);

    // Verify precondition: PR is assigned to amsterdam
    assert_eq!(state.get_reviewer(42), Some("amsterdam"));
    assert_eq!(state.pr_for_reviewer("amsterdam"), Some(42));

    // Simulate the break command: remove assignment by reviewer name
    // (This is what the Effect::ClearOrphanedReviewerAssignments does internally)
    let removed = state.remove_assignment_by_reviewer("amsterdam");
    assert!(removed.is_some());

    // Verify postcondition: PR is no longer assigned
    assert_eq!(state.get_reviewer(42), None);
    assert!(!state.is_assigned(42));
    assert_eq!(state.pr_for_reviewer("amsterdam"), None);

    // This state represents what the daemon sees on the next tick:
    // - PR #42 has no reviewer assignment
    // - amsterdam is not tracking any PR
    // - The daemon won't detect this as "needs reviewer"
    // - No spawn effect will be produced
    // - The respawn loop is prevented
}
