//! Test that breaking a reviewer clears their task session span.
//!
//! Regression test for: When a reviewer coworker is manually broken
//! (`midtown agent stop`), the daemon didn't clear the review span.
//! It then detected the PR still needs review, sees no active reviewer,
//! and respawned one — creating a loop.

use midtown::daemon::DaemonPersistentState;

#[test]
fn test_breaking_reviewer_should_clear_span() {
    let mut ps = DaemonPersistentState::default();

    // Setup: Create a reviewer span for PR #42
    ps.create_span("review-42", "amsterdam", "reviewer", "sess-rev-42");
    ps.task_pr_number.insert("review-42".to_string(), 42);

    // Verify the span exists
    assert!(ps.active_reviewer_for_pr(42).is_some());

    // Simulate breaking the reviewer via clear_reviewer_assignment
    let cleared = ps.clear_reviewer_assignment("amsterdam", "test-repo");
    assert!(cleared, "should have cleared an assignment");

    // Verify no active reviewer remains
    assert!(ps.active_reviewer_for_pr(42).is_none());
}

#[test]
fn test_breaking_untracked_reviewer_still_clears_span() {
    let mut ps = DaemonPersistentState::default();

    // A reviewer span exists but the coworker session is not tracked
    ps.create_span("review-42", "amsterdam", "reviewer", "sess-rev-42");
    ps.task_pr_number.insert("review-42".to_string(), 42);
    assert!(ps.active_reviewer_for_pr(42).is_some());

    // Clear even if coworker is not tracked
    let cleared = ps.clear_reviewer_assignment("amsterdam", "test-repo");
    assert!(cleared);

    assert!(ps.active_reviewer_for_pr(42).is_none());
}

#[test]
fn test_breaking_non_reviewer_is_safe() {
    let mut ps = DaemonPersistentState::default();

    // Setup: Create a reviewer span for lexington
    ps.create_span("review-42", "lexington", "reviewer", "sess-rev-42");
    ps.task_pr_number.insert("review-42".to_string(), 42);

    // Try to break a coworker that's not reviewing anything
    let cleared = ps.clear_reviewer_assignment("park", "test-repo");
    assert!(!cleared, "should not have cleared anything");

    // Original span should still be there
    assert!(ps.active_reviewer_for_pr(42).is_some());
}

#[test]
fn test_breaking_reviewer_with_multiple_spans() {
    let mut ps = DaemonPersistentState::default();

    // Multiple reviewers assigned
    ps.create_span("review-42", "amsterdam", "reviewer", "sess-rev-42");
    ps.task_pr_number.insert("review-42".to_string(), 42);
    ps.create_span("review-43", "lexington", "reviewer", "sess-rev-43");
    ps.task_pr_number.insert("review-43".to_string(), 43);

    // Break amsterdam
    let cleared = ps.clear_reviewer_assignment("amsterdam", "test-repo");
    assert!(cleared);

    // Amsterdam's span should be closed
    assert!(ps.active_reviewer_for_pr(42).is_none());

    // Lexington's span should be unaffected
    assert!(ps.active_reviewer_for_pr(43).is_some());
}

#[test]
fn test_coworker_break_prevents_respawn_loop() {
    let mut ps = DaemonPersistentState::default();

    // A reviewer is assigned to PR #42
    ps.create_span("review-42", "amsterdam", "reviewer", "sess-rev-42");
    ps.task_pr_number.insert("review-42".to_string(), 42);
    assert!(ps.active_reviewer_for_pr(42).is_some());

    // Simulate the break command
    let cleared = ps.clear_reviewer_assignment("amsterdam", "test-repo");
    assert!(cleared);

    // No active reviewer remains - daemon won't try to respawn
    assert!(ps.active_reviewer_for_pr(42).is_none());
    assert!(ps.active_reviewer_spans().is_empty());
}
