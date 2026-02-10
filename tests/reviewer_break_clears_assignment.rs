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
