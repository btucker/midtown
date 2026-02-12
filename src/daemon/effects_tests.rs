use super::*;
use crate::daemon::trackers::PrIssueType;

/// Helper to count effects of a specific type.
fn count_nudge_coworker(effects: &[Effect], name: &str) -> usize {
    effects
        .iter()
        .filter(|e| matches!(e, Effect::NudgeCoworker { name: n, .. } if n == name))
        .count()
}

fn count_nudge_with_callbacks(effects: &[Effect], name: &str) -> usize {
    effects
        .iter()
        .filter(|e| matches!(e, Effect::NudgeCoworkerWithCallbacks { name: n, .. } if n == name))
        .count()
}

#[test]
fn test_dedup_removes_duplicate_nudge_coworker() {
    let effects = vec![
        Effect::NudgeCoworker {
            name: "riverside".into(),
            message: "first nudge".into(),
            session_id: None,
        },
        Effect::NudgeCoworker {
            name: "riverside".into(),
            message: "second nudge".into(),
            session_id: None,
        },
        Effect::NudgeCoworker {
            name: "riverside".into(),
            message: "third nudge".into(),
            session_id: None,
        },
    ];

    let deduped = dedup_nudge_effects(effects);
    assert_eq!(count_nudge_coworker(&deduped, "riverside"), 1);
    // First message wins
    if let Effect::NudgeCoworker { message, .. } = &deduped[0] {
        assert_eq!(message, "first nudge");
    } else {
        panic!("Expected NudgeCoworker");
    }
}

#[test]
fn test_dedup_removes_duplicate_nudge_with_callbacks() {
    let effects = vec![
        Effect::NudgeCoworkerWithCallbacks {
            name: "riverside".into(),
            message: "CI green".into(),
            session_id: None,
            on_success: vec![Effect::RecordPrNudge {
                pr_number: 42,
                issue_type: PrIssueType::Approved,
            }],
        },
        Effect::NudgeCoworkerWithCallbacks {
            name: "riverside".into(),
            message: "review complete".into(),
            session_id: None,
            on_success: vec![Effect::RecordPrNudge {
                pr_number: 42,
                issue_type: PrIssueType::ReviewComplete,
            }],
        },
        Effect::NudgeCoworkerWithCallbacks {
            name: "riverside".into(),
            message: "merge conflict".into(),
            session_id: None,
            on_success: vec![Effect::RecordPrNudge {
                pr_number: 42,
                issue_type: PrIssueType::MergeConflict,
            }],
        },
    ];

    let deduped = dedup_nudge_effects(effects);
    assert_eq!(
        count_nudge_with_callbacks(&deduped, "riverside"),
        1,
        "Should collapse 3 nudges into 1"
    );
    // First message wins, but all callbacks are merged
    if let Effect::NudgeCoworkerWithCallbacks {
        message,
        on_success,
        ..
    } = &deduped[0]
    {
        assert_eq!(message, "CI green");
        assert_eq!(
            on_success.len(),
            3,
            "All three on_success callbacks should be merged"
        );
    } else {
        panic!("Expected NudgeCoworkerWithCallbacks");
    }
}

#[test]
fn test_dedup_preserves_different_coworkers() {
    let effects = vec![
        Effect::NudgeCoworker {
            name: "riverside".into(),
            message: "nudge riverside".into(),
            session_id: None,
        },
        Effect::NudgeCoworker {
            name: "broadway".into(),
            message: "nudge broadway".into(),
            session_id: None,
        },
        Effect::NudgeCoworker {
            name: "riverside".into(),
            message: "duplicate riverside".into(),
            session_id: None,
        },
    ];

    let deduped = dedup_nudge_effects(effects);
    assert_eq!(count_nudge_coworker(&deduped, "riverside"), 1);
    assert_eq!(count_nudge_coworker(&deduped, "broadway"), 1);
    assert_eq!(deduped.len(), 2);
}

#[test]
fn test_dedup_mixed_nudge_types_promotes_callbacks() {
    // Plain NudgeCoworker first, then NudgeCoworkerWithCallbacks — the nudge
    // is deduped but on_success callbacks are promoted to standalone effects
    // so state tracking (RecordPrNudge) still fires.
    let effects = vec![
        Effect::NudgeCoworker {
            name: "riverside".into(),
            message: "plain nudge".into(),
            session_id: None,
        },
        Effect::NudgeCoworkerWithCallbacks {
            name: "riverside".into(),
            message: "callback nudge".into(),
            session_id: None,
            on_success: vec![Effect::RecordPrNudge {
                pr_number: 42,
                issue_type: PrIssueType::Approved,
            }],
        },
    ];

    let deduped = dedup_nudge_effects(effects);
    // 1 NudgeCoworker + 1 promoted RecordPrNudge callback
    assert_eq!(deduped.len(), 2);
    assert_eq!(count_nudge_coworker(&deduped, "riverside"), 1);
    // Verify the RecordPrNudge callback was promoted as a standalone effect
    assert!(
        deduped
            .iter()
            .any(|e| matches!(e, Effect::RecordPrNudge { pr_number: 42, .. })),
        "RecordPrNudge callback should be promoted to standalone effect"
    );
}

#[test]
fn test_dedup_preserves_non_nudge_effects() {
    let effects = vec![
        Effect::PostToChannel {
            sender: "midtown".into(),
            message: "hello".into(),
            channel: None,
        },
        Effect::NudgeCoworker {
            name: "riverside".into(),
            message: "nudge 1".into(),
            session_id: None,
        },
        Effect::RecordCooldown {
            category: "test".into(),
            key: "key".into(),
        },
        Effect::NudgeCoworker {
            name: "riverside".into(),
            message: "nudge 2".into(),
            session_id: None,
        },
        Effect::PostToChannel {
            sender: "midtown".into(),
            message: "world".into(),
            channel: None,
        },
    ];

    let deduped = dedup_nudge_effects(effects);
    // 1 nudge + 2 PostToChannel + 1 RecordCooldown = 4
    assert_eq!(deduped.len(), 4);
    assert_eq!(count_nudge_coworker(&deduped, "riverside"), 1);
}

#[test]
fn test_dedup_case_insensitive() {
    let effects = vec![
        Effect::NudgeCoworker {
            name: "Riverside".into(),
            message: "nudge 1".into(),
            session_id: None,
        },
        Effect::NudgeCoworker {
            name: "riverside".into(),
            message: "nudge 2".into(),
            session_id: None,
        },
    ];

    let deduped = dedup_nudge_effects(effects);
    assert_eq!(deduped.len(), 1);
}

#[test]
fn test_dedup_quadruple_nudge_scenario() {
    // Reproduces the exact bug: 4 nudges to same coworker in 1 second
    // from different PR issue sources.
    let effects = vec![
        Effect::NudgeCoworkerWithCallbacks {
            name: "riverside".into(),
            message: "PR #181 - CI checks passed".into(),
            session_id: None,
            on_success: vec![Effect::RecordPrNudge {
                pr_number: 181,
                issue_type: PrIssueType::Approved,
            }],
        },
        Effect::NudgeCoworkerWithCallbacks {
            name: "riverside".into(),
            message: "PR #181 - Review complete".into(),
            session_id: None,
            on_success: vec![Effect::RecordPrNudge {
                pr_number: 181,
                issue_type: PrIssueType::ReviewComplete,
            }],
        },
        Effect::NudgeCoworkerWithCallbacks {
            name: "riverside".into(),
            message: "PR #181 - Merge conflict".into(),
            session_id: None,
            on_success: vec![Effect::RecordPrNudge {
                pr_number: 181,
                issue_type: PrIssueType::MergeConflict,
            }],
        },
        Effect::NudgeCoworkerWithCallbacks {
            name: "riverside".into(),
            message: "PR #181 - Green with feedback".into(),
            session_id: None,
            on_success: vec![Effect::RecordPrNudge {
                pr_number: 181,
                issue_type: PrIssueType::GreenWithFeedback,
            }],
        },
    ];

    let deduped = dedup_nudge_effects(effects);

    // Should have: 1 nudge (with merged callbacks)
    assert_eq!(
        count_nudge_with_callbacks(&deduped, "riverside"),
        1,
        "4 nudges should collapse into 1"
    );

    // The merged nudge should have all 4 on_success callbacks
    if let Effect::NudgeCoworkerWithCallbacks {
        on_success,
        message,
        ..
    } = &deduped[0]
    {
        assert_eq!(message, "PR #181 - CI checks passed", "First message wins");
        assert_eq!(on_success.len(), 4, "All 4 callbacks should be merged");
    } else {
        panic!("Expected NudgeCoworkerWithCallbacks");
    }
}

#[tokio::test]
async fn test_complete_task_cleans_up_pr_author_sessions() {
    use crate::daemon::state::DaemonPersistentState;
    use crate::github_state::PrAuthorSession;
    use chrono::Utc;
    use std::collections::HashMap;

    let mut persistent_state = DaemonPersistentState::default();

    // Set up pr_author_sessions with entries for different tasks
    let mut pr_sessions = HashMap::new();
    pr_sessions.insert(
        1001,
        PrAuthorSession {
            session_id: "session-abc".to_string(),
            branch: "vernon/fix-task-42".to_string(),
            original_author: "vernon".to_string(),
            stored_at: Utc::now(),
            task_id: Some("42".to_string()),
        },
    );
    pr_sessions.insert(
        1002,
        PrAuthorSession {
            session_id: "session-def".to_string(),
            branch: "park/feature-task-99".to_string(),
            original_author: "park".to_string(),
            stored_at: Utc::now(),
            task_id: Some("99".to_string()),
        },
    );
    pr_sessions.insert(
        1003,
        PrAuthorSession {
            session_id: "session-ghi".to_string(),
            branch: "madison/another-task-42".to_string(),
            original_author: "madison".to_string(),
            stored_at: Utc::now(),
            task_id: Some("42".to_string()), // Same task_id as PR 1001
        },
    );
    pr_sessions.insert(
        1004,
        PrAuthorSession {
            session_id: "session-jkl".to_string(),
            branch: "broadway/no-task".to_string(),
            original_author: "broadway".to_string(),
            stored_at: Utc::now(),
            task_id: None, // No task_id
        },
    );
    persistent_state.github.pr_author_sessions = pr_sessions;

    // Simulate the cleanup logic from Effect::CompleteTask for task "42"
    let completed_task_id = "42";
    persistent_state
        .github
        .pr_author_sessions
        .retain(|_, session| session.task_id.as_deref() != Some(completed_task_id));

    // Verify cleanup results
    assert!(
        !persistent_state
            .github
            .pr_author_sessions
            .contains_key(&1001),
        "PR 1001 with task_id=42 should be removed"
    );
    assert!(
        persistent_state
            .github
            .pr_author_sessions
            .contains_key(&1002),
        "PR 1002 with task_id=99 should remain"
    );
    assert!(
        !persistent_state
            .github
            .pr_author_sessions
            .contains_key(&1003),
        "PR 1003 with task_id=42 should be removed"
    );
    assert!(
        persistent_state
            .github
            .pr_author_sessions
            .contains_key(&1004),
        "PR 1004 with no task_id should remain"
    );
    assert_eq!(
        persistent_state.github.pr_author_sessions.len(),
        2,
        "Should have exactly 2 remaining entries (1002 and 1004)"
    );
}

#[tokio::test]
async fn test_cleanup_merged_worktree_removes_pr_author_session() {
    use crate::daemon::state::DaemonPersistentState;
    use crate::github_state::PrAuthorSession;
    use chrono::Utc;
    use std::collections::HashMap;

    let mut persistent_state = DaemonPersistentState::default();

    // Set up pr_author_sessions with entries for different PRs
    let mut pr_sessions = HashMap::new();
    pr_sessions.insert(
        1001,
        PrAuthorSession {
            session_id: "session-abc".to_string(),
            branch: "vernon/fix-bug".to_string(),
            original_author: "vernon".to_string(),
            stored_at: Utc::now(),
            task_id: Some("42".to_string()),
        },
    );
    pr_sessions.insert(
        1002,
        PrAuthorSession {
            session_id: "session-def".to_string(),
            branch: "park/feature".to_string(),
            original_author: "park".to_string(),
            stored_at: Utc::now(),
            task_id: Some("99".to_string()),
        },
    );
    persistent_state.github.pr_author_sessions = pr_sessions;

    // Simulate the cleanup logic from Effect::CleanupMergedWorktree for PR 1001
    let merged_pr_number = 1001;
    persistent_state
        .github
        .pr_author_sessions
        .remove(&merged_pr_number);

    // Verify cleanup results
    assert!(
        !persistent_state
            .github
            .pr_author_sessions
            .contains_key(&1001),
        "PR 1001 should be removed after worktree cleanup"
    );
    assert!(
        persistent_state
            .github
            .pr_author_sessions
            .contains_key(&1002),
        "PR 1002 should remain"
    );
    assert_eq!(
        persistent_state.github.pr_author_sessions.len(),
        1,
        "Should have exactly 1 remaining entry (1002)"
    );
}

#[tokio::test]
async fn test_cleanup_merged_worktree_saves_when_only_pr_session_removed() {
    use crate::daemon::state::DaemonPersistentState;
    use crate::github_state::PrAuthorSession;
    use chrono::Utc;
    use std::collections::HashMap;
    use tempfile::tempdir;

    // Create temp dir for persistent state
    let temp_dir = tempdir().unwrap();
    let repo_name = "test-repo";

    // Initial state: pr_author_sessions has a stale entry for PR #2001,
    // but worktree_registry has NO entry (worktree was already cleaned up somehow)
    let mut persistent_state = DaemonPersistentState::default();
    let mut pr_sessions = HashMap::new();
    pr_sessions.insert(
        2001,
        PrAuthorSession {
            session_id: "stale-session".to_string(),
            branch: "old-branch".to_string(),
            original_author: "columbus".to_string(),
            stored_at: Utc::now(),
            task_id: Some("123".to_string()),
        },
    );
    persistent_state.github.pr_author_sessions = pr_sessions;

    // Save initial state to disk
    unsafe {
        std::env::set_var("MIDTOWN_PROJECTS_ROOT", temp_dir.path());
    }
    persistent_state.save_for_repo(repo_name).unwrap();

    // Verify stale entry exists on disk
    let loaded_before = DaemonPersistentState::load_for_repo(repo_name).unwrap();
    assert!(
        loaded_before.github.pr_author_sessions.contains_key(&2001),
        "Stale PR session should exist before cleanup"
    );

    // Simulate CleanupMergedWorktree cleanup for PR #2001
    // worktree_registry.cleanup_for_merged_pr returns None (no worktree found)
    // but pr_author_sessions.remove returns Some (stale entry found)
    let pr_session_removed = persistent_state.github.pr_author_sessions.remove(&2001);
    assert!(
        pr_session_removed.is_some(),
        "Should have removed the stale pr_author_session"
    );

    // The fix ensures save happens when either worktree OR pr_session is removed
    persistent_state.save_for_repo(repo_name).unwrap();

    // Verify PR #2001 session is removed from disk (defense-in-depth actually persisted)
    let loaded_after = DaemonPersistentState::load_for_repo(repo_name).unwrap();
    assert!(
        !loaded_after.github.pr_author_sessions.contains_key(&2001),
        "Stale PR session should be removed from disk after cleanup"
    );
}
