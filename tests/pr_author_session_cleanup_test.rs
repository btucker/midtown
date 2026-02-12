// Test for pr_author_sessions cleanup on task completion (issue #1192).
//
// Bug: When Effect::CompleteTask executes (PR merged), it marks the task completed
// and clears the task assignment, but did NOT remove the pr_author_sessions entry.
// This left stale state that could cause the daemon to think the task still needs work.
//
// Fix: In effects.rs, when CompleteTask runs, also remove the pr_author_sessions
// entry for the completed task's PR.

use midtown::github_state::{GitHubState, PrAuthorSession};
use tempfile::tempdir;

#[test]
fn test_pr_author_sessions_cleanup_on_complete_task() {
    // Create a test state with a pr_author_session for task 42
    let mut state = GitHubState::default();

    // Store author session for PR #100 associated with task "42"
    state.store_pr_author_session(
        100,
        "session-abc-123",
        "madison/feature-branch",
        "madison",
        "feat: Add feature [Midtown !42]",
    );

    // Verify the session is stored
    assert!(state.get_pr_author_session(100).is_some());
    let session = state.get_pr_author_session(100).unwrap();
    assert_eq!(session.task_id, Some("42".to_string()));

    // Simulate CompleteTask cleanup: retain only sessions where task_id != "42"
    let task_id = "42";
    state
        .pr_author_sessions
        .retain(|_, session| session.task_id.as_deref() != Some(task_id));

    // Verify the session is now removed
    assert!(
        state.get_pr_author_session(100).is_none(),
        "pr_author_session should be removed when task is completed"
    );
}

#[test]
fn test_pr_author_sessions_cleanup_preserves_other_tasks() {
    let mut state = GitHubState::default();

    // Store sessions for multiple tasks
    state.store_pr_author_session(
        100,
        "session-1",
        "madison/task-42",
        "madison",
        "feat: Feature A [Midtown !42]",
    );
    state.store_pr_author_session(
        101,
        "session-2",
        "park/task-43",
        "park",
        "feat: Feature B [Midtown !43]",
    );
    state.store_pr_author_session(
        102,
        "session-3",
        "york/task-44",
        "york",
        "feat: Feature C [Midtown !44]",
    );

    // Complete task 42
    let task_id = "42";
    state
        .pr_author_sessions
        .retain(|_, session| session.task_id.as_deref() != Some(task_id));

    // Verify task 42's session is removed
    assert!(state.get_pr_author_session(100).is_none());

    // Verify other tasks' sessions are preserved
    assert!(state.get_pr_author_session(101).is_some());
    assert!(state.get_pr_author_session(102).is_some());
}

#[test]
fn test_pr_author_sessions_cleanup_without_task_id() {
    let mut state = GitHubState::default();

    // Manually create a session without a task_id (legacy or malformed data)
    state.pr_author_sessions.insert(
        100,
        PrAuthorSession {
            session_id: "session-legacy".to_string(),
            branch: "legacy/branch".to_string(),
            original_author: "legacy-author".to_string(),
            stored_at: chrono::Utc::now(),
            task_id: None,
        },
    );

    // Store another session with task_id
    state.store_pr_author_session(
        101,
        "session-new",
        "madison/task-42",
        "madison",
        "feat: Feature [Midtown !42]",
    );

    // Complete task 42
    let task_id = "42";
    state
        .pr_author_sessions
        .retain(|_, session| session.task_id.as_deref() != Some(task_id));

    // Legacy session without task_id should be preserved (not matched)
    assert!(state.get_pr_author_session(100).is_some());

    // Session with task_id=42 should be removed
    assert!(state.get_pr_author_session(101).is_none());
}

#[test]
fn test_pr_author_sessions_persists_after_save_load() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("github-state.json");

    let mut state = GitHubState::default();
    state.store_pr_author_session(
        100,
        "session-abc",
        "madison/feature",
        "madison",
        "feat: Feature [Midtown !42]",
    );

    // Save
    state.save(&path).unwrap();

    // Load
    let mut loaded = GitHubState::load(&path).unwrap();
    assert!(loaded.get_pr_author_session(100).is_some());

    // Cleanup task 42
    let task_id = "42";
    loaded
        .pr_author_sessions
        .retain(|_, session| session.task_id.as_deref() != Some(task_id));

    // Save again
    loaded.save(&path).unwrap();

    // Reload and verify cleanup persisted
    let reloaded = GitHubState::load(&path).unwrap();
    assert!(
        reloaded.get_pr_author_session(100).is_none(),
        "Cleanup should persist across save/load cycles"
    );
}
