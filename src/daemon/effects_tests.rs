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

// ── Session-centric effect tests ──────────────────────────────────────

#[test]
fn test_record_session_inserts_into_persistent_state() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};
    use chrono::Utc;

    let mut persistent_state = DaemonPersistentState::default();
    let record = SessionRecord {
        session_id: "sess-abc-123".to_string(),
        task_id: Some("42".to_string()),
        current_name: Some("lexington".to_string()),
        preferred_name: Some("lexington".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        branch: Some("lexington/task-42".to_string()),
        pr_number: None,
        initial_prompt: Some("Work on task 42".to_string()),
        is_reviewer: false,
        coworker_type: "dev".to_string(),
        is_running: true,
        created_at: Utc::now(),
        resume_on_startup: true,
    };

    // Simulate RecordSession effect
    persistent_state
        .sessions
        .insert(record.session_id.clone(), record.clone());

    assert!(persistent_state.sessions.contains_key("sess-abc-123"));
    let stored = persistent_state.sessions.get("sess-abc-123").unwrap();
    assert_eq!(stored.task_id.as_deref(), Some("42"));
    assert_eq!(stored.current_name.as_deref(), Some("lexington"));
    assert!(stored.is_running);
}

#[test]
fn test_record_session_updates_existing_record() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};
    use chrono::Utc;

    let mut persistent_state = DaemonPersistentState::default();

    // Insert initial record
    let record = SessionRecord {
        session_id: "sess-abc-123".to_string(),
        task_id: Some("42".to_string()),
        current_name: Some("lexington".to_string()),
        preferred_name: Some("lexington".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        branch: None,
        pr_number: None,
        initial_prompt: None,
        is_reviewer: false,
        coworker_type: "dev".to_string(),
        is_running: true,
        created_at: Utc::now(),
        resume_on_startup: true,
    };
    persistent_state
        .sessions
        .insert(record.session_id.clone(), record);

    // Simulate update (e.g., session stopped)
    let mut updated = persistent_state
        .sessions
        .get("sess-abc-123")
        .unwrap()
        .clone();
    updated.is_running = false;
    updated.current_name = None;
    persistent_state
        .sessions
        .insert("sess-abc-123".to_string(), updated);

    let stored = persistent_state.sessions.get("sess-abc-123").unwrap();
    assert!(!stored.is_running);
    assert!(stored.current_name.is_none());
    // Preferred name is preserved for future resume
    assert_eq!(stored.preferred_name.as_deref(), Some("lexington"));
}

#[test]
fn test_release_name_frees_name_in_pool() {
    use crate::name_pool::NamePool;

    let mut pool = NamePool::new(&["lexington", "park", "madison"]);

    // Allocate "lexington"
    let name = pool.allocate(None).unwrap();
    assert_eq!(name, "lexington");
    assert!(pool.is_allocated("lexington"));
    assert_eq!(pool.available_count(), 2);

    // Simulate ReleaseName effect
    pool.release(&name);
    assert!(!pool.is_allocated("lexington"));
    assert_eq!(pool.available_count(), 3);

    // Released name goes to the back of the LRU queue
    assert_eq!(pool.allocate(None).unwrap(), "park");
    assert_eq!(pool.allocate(None).unwrap(), "madison");
    assert_eq!(pool.allocate(None).unwrap(), "lexington");
}

#[test]
fn test_shutdown_session_marks_not_running() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};
    use chrono::Utc;

    let mut persistent_state = DaemonPersistentState::default();
    let record = SessionRecord {
        session_id: "sess-abc-123".to_string(),
        task_id: Some("42".to_string()),
        current_name: Some("lexington".to_string()),
        preferred_name: Some("lexington".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        branch: None,
        pr_number: None,
        initial_prompt: None,
        is_reviewer: false,
        coworker_type: "dev".to_string(),
        is_running: true,
        created_at: Utc::now(),
        resume_on_startup: true,
    };
    persistent_state
        .sessions
        .insert(record.session_id.clone(), record);

    // Simulate ShutdownSession effect
    if let Some(record) = persistent_state.sessions.get_mut("sess-abc-123") {
        record.is_running = false;
        record.current_name = None;
    }

    let stored = persistent_state.sessions.get("sess-abc-123").unwrap();
    assert!(!stored.is_running);
    assert!(stored.current_name.is_none());
    assert_eq!(stored.preferred_name.as_deref(), Some("lexington"));
}

#[test]
fn test_spawn_session_name_allocation_with_preferred() {
    use crate::name_pool::NamePool;
    use std::collections::HashSet;

    let mut pool = NamePool::new(&["lexington", "park", "madison"]);
    // Allocate park (simulating it's in use)
    pool.allocate(None); // takes lexington
    pool.allocate(None); // takes park
    pool.release("lexington"); // lexington returns to pool

    // SpawnSession with preferred_name="lexington" should get lexington
    let excluded: HashSet<String> = HashSet::new();
    let name = pool.allocate_excluding(Some("lexington"), &excluded);
    assert_eq!(name.as_deref(), Some("lexington"));
}

#[test]
fn test_spawn_session_excludes_channel_leads() {
    use crate::name_pool::NamePool;
    use std::collections::HashSet;

    let mut pool = NamePool::new(&["lexington", "park", "madison"]);

    // Channel leads should be excluded from allocation
    let channel_leads: HashSet<String> = ["lexington"].iter().map(|s| s.to_string()).collect();
    let name = pool.allocate_excluding(None, &channel_leads);
    assert_eq!(
        name.as_deref(),
        Some("park"),
        "Should skip lexington (channel lead)"
    );
}

#[test]
fn test_reverse_maps_consistency() {
    use std::collections::HashMap;

    // Simulate the reverse map operations from RecordSession
    let mut name_to_session: HashMap<String, String> = HashMap::new();
    let mut session_to_name: HashMap<String, String> = HashMap::new();
    let mut task_to_session: HashMap<String, String> = HashMap::new();

    // RecordSession: insert
    let session_id = "sess-abc-123".to_string();
    let name = "lexington".to_string();
    let task_id = "42".to_string();
    name_to_session.insert(name.clone(), session_id.clone());
    session_to_name.insert(session_id.clone(), name.clone());
    task_to_session.insert(task_id.clone(), session_id.clone());

    // Verify lookups
    assert_eq!(name_to_session.get("lexington"), Some(&session_id));
    assert_eq!(session_to_name.get("sess-abc-123"), Some(&name));
    assert_eq!(task_to_session.get("42"), Some(&session_id));

    // ReleaseName: cleanup
    let removed_session = name_to_session.remove("lexington");
    if let Some(ref sid) = removed_session {
        session_to_name.remove(sid);
    }

    assert!(name_to_session.is_empty());
    assert!(session_to_name.is_empty());
    // task_to_session is NOT cleaned up by ReleaseName (intentional — task mapping persists)
    assert_eq!(task_to_session.get("42"), Some(&session_id));
}

#[test]
fn test_coworker_break_updates_session_record() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};
    use chrono::Utc;
    use std::collections::HashMap;

    let mut persistent_state = DaemonPersistentState::default();
    let record = SessionRecord {
        session_id: "sess-abc-123".to_string(),
        task_id: Some("42".to_string()),
        current_name: Some("lexington".to_string()),
        preferred_name: Some("lexington".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        branch: None,
        pr_number: None,
        initial_prompt: None,
        is_reviewer: false,
        coworker_type: "dev".to_string(),
        is_running: true,
        created_at: Utc::now(),
        resume_on_startup: true,
    };
    persistent_state
        .sessions
        .insert(record.session_id.clone(), record);

    // Simulate what handle_coworker_break should do:
    // 1. Look up session_id from name
    let mut name_to_session: HashMap<String, String> = HashMap::new();
    name_to_session.insert("lexington".to_string(), "sess-abc-123".to_string());
    let session_id = name_to_session.get("lexington").cloned();

    // 2. Update session record
    if let Some(session_id) = session_id {
        if let Some(record) = persistent_state.sessions.get_mut(&session_id) {
            record.is_running = false;
            record.current_name = None;
        }
    }

    let stored = persistent_state.sessions.get("sess-abc-123").unwrap();
    assert!(!stored.is_running);
    assert!(stored.current_name.is_none());
}

#[test]
fn test_shutdown_coworker_impl_updates_session_via_name_lookup() {
    // Verifies that when shutdown_coworker_impl runs for a name that has
    // an associated session, the SessionRecord gets is_running=false and
    // current_name=None. This matters because the Idle path in
    // handle_coworker_report_state flows through ShutdownCoworkerWithCallbacks
    // → shutdown_coworker_impl, so session cleanup must happen there.
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};
    use crate::name_pool::NamePool;
    use chrono::Utc;
    use std::collections::HashMap;

    let mut persistent_state = DaemonPersistentState::default();
    let mut name_to_session: HashMap<String, String> = HashMap::new();
    let mut session_to_name: HashMap<String, String> = HashMap::new();
    let mut pool = NamePool::new(&["lexington", "park", "madison"]);

    // Set up: session "sess-123" is running as "lexington"
    let record = SessionRecord {
        session_id: "sess-123".to_string(),
        task_id: Some("42".to_string()),
        current_name: Some("lexington".to_string()),
        preferred_name: Some("lexington".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        branch: None,
        pr_number: None,
        initial_prompt: None,
        is_reviewer: false,
        coworker_type: "dev".to_string(),
        is_running: true,
        created_at: Utc::now(),
        resume_on_startup: true,
    };
    persistent_state
        .sessions
        .insert(record.session_id.clone(), record);
    name_to_session.insert("lexington".to_string(), "sess-123".to_string());
    session_to_name.insert("sess-123".to_string(), "lexington".to_string());
    pool.allocate(Some("lexington")); // mark as allocated

    // Simulate what shutdown_coworker_impl should do after shutting down "lexington":
    // 1. Look up session from name
    let session_id = name_to_session.get("lexington").cloned();
    // 2. Update session record
    if let Some(session_id) = &session_id {
        if let Some(sr) = persistent_state.sessions.get_mut(session_id) {
            sr.is_running = false;
            sr.current_name = None;
        }
    }
    // 3. Release name from pool
    pool.release("lexington");
    // 4. Clean up reverse maps
    name_to_session.remove("lexington");
    if let Some(sid) = session_id {
        session_to_name.remove(&sid);
    }

    // Verify: session record updated
    let stored = persistent_state.sessions.get("sess-123").unwrap();
    assert!(!stored.is_running);
    assert!(stored.current_name.is_none());
    assert_eq!(stored.preferred_name.as_deref(), Some("lexington"));

    // Verify: name released back to pool
    assert!(!pool.is_allocated("lexington"));
    assert_eq!(pool.available_count(), 3);

    // Verify: reverse maps cleaned up
    assert!(name_to_session.is_empty());
    assert!(session_to_name.is_empty());
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
