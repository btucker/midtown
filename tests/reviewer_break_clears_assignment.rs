//! Test that breaking a reviewer clears their assignment.
//!
//! Regression test for: When a reviewer coworker is manually broken
//! (`midtown agent stop`), the daemon didn't clear the review assignment.
//! It then detected the PR still needs review, sees no active reviewer,
//! and respawned one — creating a loop.

use midtown::daemon::{DaemonPersistentState, SessionRecord};
use std::collections::HashMap;

fn insert_reviewer_session(
    ps: &mut DaemonPersistentState,
    name: &str,
    session_id: &str,
    task_id: &str,
    pr: u64,
) {
    ps.sessions.insert(
        session_id.to_string(),
        SessionRecord {
            session_id: session_id.to_string(),
            name: name.to_string(),
            agent_type: "midtown-code-reviewer".to_string(),
            task_id: Some(task_id.to_string()),
            pr_number: Some(pr),
            is_running: true,
            ..Default::default()
        },
    );
    // Set PR number on the session record
    if let Some(s) = ps
        .sessions
        .values_mut()
        .find(|s| s.task_id.as_deref() == Some(task_id))
    {
        s.pr_number = Some(pr);
    }
}

#[test]
fn test_breaking_reviewer_should_clear_span() {
    let mut ps = DaemonPersistentState::default();
    insert_reviewer_session(&mut ps, "amsterdam", "sess-rev-42", "review-42", 42);
    let pr_to_task = HashMap::from([(42u64, "review-42".to_string())]);
    assert!(ps.pr_has_active_reviewer(42, &pr_to_task));

    let cleared = ps.clear_reviewer_assignment("amsterdam", "test-repo");
    assert!(cleared, "should have cleared an assignment");
    assert!(!ps.pr_has_active_reviewer(42, &pr_to_task));
}

#[test]
fn test_breaking_untracked_reviewer_still_clears_span() {
    let mut ps = DaemonPersistentState::default();
    insert_reviewer_session(&mut ps, "amsterdam", "sess-rev-42", "review-42", 42);
    let pr_to_task = HashMap::from([(42u64, "review-42".to_string())]);
    assert!(ps.pr_has_active_reviewer(42, &pr_to_task));

    let cleared = ps.clear_reviewer_assignment("amsterdam", "test-repo");
    assert!(cleared);
    assert!(!ps.pr_has_active_reviewer(42, &pr_to_task));
}

#[test]
fn test_breaking_non_reviewer_is_safe() {
    let mut ps = DaemonPersistentState::default();
    insert_reviewer_session(&mut ps, "lexington", "sess-rev-42", "review-42", 42);
    let pr_to_task = HashMap::from([(42u64, "review-42".to_string())]);

    let cleared = ps.clear_reviewer_assignment("park", "test-repo");
    assert!(!cleared, "should not have cleared anything");
    assert!(ps.pr_has_active_reviewer(42, &pr_to_task));
}

#[test]
fn test_breaking_reviewer_with_multiple_spans() {
    let mut ps = DaemonPersistentState::default();
    insert_reviewer_session(&mut ps, "amsterdam", "sess-rev-42", "review-42", 42);
    insert_reviewer_session(&mut ps, "lexington", "sess-rev-43", "review-43", 43);
    let pr_to_task = HashMap::from([
        (42u64, "review-42".to_string()),
        (43u64, "review-43".to_string()),
    ]);

    let cleared = ps.clear_reviewer_assignment("amsterdam", "test-repo");
    assert!(cleared);
    assert!(!ps.pr_has_active_reviewer(42, &pr_to_task));
    assert!(ps.pr_has_active_reviewer(43, &pr_to_task));
}

#[test]
fn test_coworker_break_prevents_respawn_loop() {
    let mut ps = DaemonPersistentState::default();
    insert_reviewer_session(&mut ps, "amsterdam", "sess-rev-42", "review-42", 42);
    let pr_to_task = HashMap::from([(42u64, "review-42".to_string())]);
    assert!(ps.pr_has_active_reviewer(42, &pr_to_task));

    let cleared = ps.clear_reviewer_assignment("amsterdam", "test-repo");
    assert!(cleared);
    assert!(!ps.pr_has_active_reviewer(42, &pr_to_task));
    assert!(ps.running_reviewer_sessions().is_empty());
}
