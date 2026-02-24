use super::*;
use crate::daemon::trackers::PrIssueType;

fn mk_session_record(
    session_id: &str,
    task_id: Option<&str>,
    is_running: bool,
) -> crate::daemon::state::SessionRecord {
    crate::daemon::state::SessionRecord {
        session_id: session_id.to_string(),
        task_id: task_id.map(ToString::to_string),
        current_name: Some("lexington".to_string()),
        preferred_name: Some("lexington".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        is_running,
        ..Default::default()
    }
}

/// Helper to count NudgeSession effects for a given session_id.
fn count_nudge_session(effects: &[Effect], sid: &str) -> usize {
    effects
        .iter()
        .filter(|e| matches!(e, Effect::NudgeSession { session_id, .. } if session_id == sid))
        .count()
}

#[test]
fn record_session_recovery_cooldown_records_resume_spawns() {
    let tracker = std::sync::Mutex::new(crate::rules::CooldownTracker::new());
    super::record_session_recovery_cooldown(&tracker, "sess-resume-1", true);
    let guard = tracker.lock().unwrap();
    assert!(
        guard.has_entry("session_recovered", "sess-resume-1"),
        "resume spawns should record the session_recovered cooldown"
    );
}

#[test]
fn record_session_recovery_cooldown_skips_fresh_spawns() {
    let tracker = std::sync::Mutex::new(crate::rules::CooldownTracker::new());
    super::record_session_recovery_cooldown(&tracker, "fresh-spawn-1", false);
    let guard = tracker.lock().unwrap();
    assert!(
        !guard.has_entry("session_recovered", "fresh-spawn-1"),
        "fresh spawns should not record session_recovered cooldowns"
    );
}

#[test]
fn clear_task_binding_in_records_clears_only_stale_when_no_expected_session() {
    use std::collections::HashMap;

    let mut sessions: HashMap<String, crate::daemon::state::SessionRecord> = HashMap::new();
    sessions.insert(
        "sid-stale".to_string(),
        mk_session_record("sid-stale", Some("42"), false),
    );
    sessions.insert(
        "sid-running".to_string(),
        mk_session_record("sid-running", Some("42"), true),
    );
    sessions.insert(
        "sid-other".to_string(),
        mk_session_record("sid-other", Some("99"), false),
    );

    let cleared = clear_task_binding_in_records(&mut sessions, "42", None);
    assert_eq!(cleared, 1);
    assert!(sessions["sid-stale"].task_id.is_none());
    assert_eq!(sessions["sid-running"].task_id.as_deref(), Some("42"));
    assert_eq!(sessions["sid-other"].task_id.as_deref(), Some("99"));
}

#[test]
fn clear_task_binding_in_records_clears_expected_running_session() {
    use std::collections::HashMap;

    let mut sessions: HashMap<String, crate::daemon::state::SessionRecord> = HashMap::new();
    sessions.insert(
        "sid-running".to_string(),
        mk_session_record("sid-running", Some("42"), true),
    );

    let cleared = clear_task_binding_in_records(&mut sessions, "42", Some("sid-running"));
    assert_eq!(cleared, 1);
    assert!(sessions["sid-running"].task_id.is_none());
    assert!(!sessions["sid-running"].is_running);
    assert!(!sessions["sid-running"].resume_on_startup);
}

fn count_nudge_with_callbacks(effects: &[Effect], sid: &str) -> usize {
    effects
        .iter()
        .filter(|e| matches!(e, Effect::NudgeSessionWithCallbacks { session_id, .. } if session_id == sid))
        .count()
}

#[test]
fn test_dedup_removes_duplicate_nudge_session() {
    let effects = vec![
        Effect::nudge_session("sess-riverside-1", "first nudge"),
        Effect::nudge_session("sess-riverside-1", "second nudge"),
        Effect::nudge_session("sess-riverside-1", "third nudge"),
    ];

    let deduped = dedup_nudge_effects(effects);
    assert_eq!(count_nudge_session(&deduped, "sess-riverside-1"), 1);
    // First message wins
    if let Effect::NudgeSession { reason, .. } = &deduped[0] {
        assert_eq!(reason.to_nudge_message(), "first nudge");
    } else {
        panic!("Expected NudgeSession");
    }
}

#[test]
fn test_dedup_removes_duplicate_nudge_with_callbacks() {
    let effects = vec![
        Effect::nudge_session_with_callbacks(
            "sess-riverside-1",
            "CI green",
            vec![Effect::RecordPrNudge {
                pr_number: 42,
                issue_type: PrIssueType::Approved,
            }],
        ),
        Effect::nudge_session_with_callbacks(
            "sess-riverside-1",
            "review complete",
            vec![Effect::RecordPrNudge {
                pr_number: 42,
                issue_type: PrIssueType::ReviewComplete,
            }],
        ),
        Effect::nudge_session_with_callbacks(
            "sess-riverside-1",
            "merge conflict",
            vec![Effect::RecordPrNudge {
                pr_number: 42,
                issue_type: PrIssueType::MergeConflict,
            }],
        ),
    ];

    let deduped = dedup_nudge_effects(effects);
    assert_eq!(
        count_nudge_with_callbacks(&deduped, "sess-riverside-1"),
        1,
        "Should collapse 3 nudges into 1"
    );
    // First message wins, but all callbacks are merged
    if let Effect::NudgeSessionWithCallbacks {
        reason, on_success, ..
    } = &deduped[0]
    {
        assert_eq!(reason.to_nudge_message(), "CI green");
        assert_eq!(
            on_success.len(),
            3,
            "All three on_success callbacks should be merged"
        );
    } else {
        panic!("Expected NudgeSessionWithCallbacks");
    }
}

#[test]
fn test_dedup_preserves_different_sessions() {
    let effects = vec![
        Effect::nudge_session("sess-riverside-1", "nudge riverside"),
        Effect::nudge_session("sess-broadway-2", "nudge broadway"),
        Effect::nudge_session("sess-riverside-1", "duplicate riverside"),
    ];

    let deduped = dedup_nudge_effects(effects);
    assert_eq!(count_nudge_session(&deduped, "sess-riverside-1"), 1);
    assert_eq!(count_nudge_session(&deduped, "sess-broadway-2"), 1);
    assert_eq!(deduped.len(), 2);
}

#[test]
fn test_dedup_mixed_nudge_types_promotes_callbacks() {
    // Plain NudgeSession first, then NudgeSessionWithCallbacks — the nudge
    // is deduped but on_success callbacks are promoted to standalone effects
    // so state tracking (RecordPrNudge) still fires.
    let effects = vec![
        Effect::nudge_session("sess-riverside-1", "plain nudge"),
        Effect::nudge_session_with_callbacks(
            "sess-riverside-1",
            "callback nudge",
            vec![Effect::RecordPrNudge {
                pr_number: 42,
                issue_type: PrIssueType::Approved,
            }],
        ),
    ];

    let deduped = dedup_nudge_effects(effects);
    // 1 NudgeSession + 1 promoted RecordPrNudge callback
    assert_eq!(deduped.len(), 2);
    assert_eq!(count_nudge_session(&deduped, "sess-riverside-1"), 1);
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
        Effect::nudge_session("sess-riverside-1", "nudge 1"),
        Effect::RecordCooldown {
            category: "test".into(),
            key: "key".into(),
        },
        Effect::nudge_session("sess-riverside-1", "nudge 2"),
        Effect::PostToChannel {
            sender: "midtown".into(),
            message: "world".into(),
            channel: None,
        },
    ];

    let deduped = dedup_nudge_effects(effects);
    // 1 nudge + 2 PostToChannel + 1 RecordCooldown = 4
    assert_eq!(deduped.len(), 4);
    assert_eq!(count_nudge_session(&deduped, "sess-riverside-1"), 1);
}

#[test]
fn test_should_resume_channel_lead_session() {
    assert!(
        !should_resume_channel_lead_session(""),
        "Empty stored session ID should trigger fresh spawn"
    );
    assert!(
        should_resume_channel_lead_session("session-123"),
        "Non-empty stored session ID should resume"
    );
}

#[test]
fn test_dedup_session_id_based() {
    // Session IDs are exact match, not case-insensitive
    let effects = vec![
        Effect::nudge_session("sess-abc-123", "nudge 1"),
        Effect::nudge_session("sess-abc-123", "nudge 2"),
    ];

    let deduped = dedup_nudge_effects(effects);
    assert_eq!(deduped.len(), 1);
}

#[test]
fn test_dedup_quadruple_nudge_scenario() {
    // Reproduces the exact bug: 4 nudges to same session in 1 second
    // from different PR issue sources.
    let effects = vec![
        Effect::nudge_session_with_callbacks(
            "sess-riverside-1",
            "PR #181 - CI checks passed",
            vec![Effect::RecordPrNudge {
                pr_number: 181,
                issue_type: PrIssueType::Approved,
            }],
        ),
        Effect::nudge_session_with_callbacks(
            "sess-riverside-1",
            "PR #181 - Review complete",
            vec![Effect::RecordPrNudge {
                pr_number: 181,
                issue_type: PrIssueType::ReviewComplete,
            }],
        ),
        Effect::nudge_session_with_callbacks(
            "sess-riverside-1",
            "PR #181 - Merge conflict",
            vec![Effect::RecordPrNudge {
                pr_number: 181,
                issue_type: PrIssueType::MergeConflict,
            }],
        ),
        Effect::nudge_session_with_callbacks(
            "sess-riverside-1",
            "PR #181 - Green with feedback",
            vec![Effect::RecordPrNudge {
                pr_number: 181,
                issue_type: PrIssueType::GreenWithFeedback,
            }],
        ),
    ];

    let deduped = dedup_nudge_effects(effects);

    // Should have: 1 nudge (with merged callbacks)
    assert_eq!(
        count_nudge_with_callbacks(&deduped, "sess-riverside-1"),
        1,
        "4 nudges should collapse into 1"
    );

    // The merged nudge should have all 4 on_success callbacks
    if let Effect::NudgeSessionWithCallbacks {
        on_success, reason, ..
    } = &deduped[0]
    {
        assert_eq!(
            reason.to_nudge_message(),
            "PR #181 - CI checks passed",
            "First message wins"
        );
        assert_eq!(on_success.len(), 4, "All 4 callbacks should be merged");
    } else {
        panic!("Expected NudgeSessionWithCallbacks");
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
            task_id: Some("42".to_string()),
        },
    );
    pr_sessions.insert(
        1004,
        PrAuthorSession {
            session_id: "session-jkl".to_string(),
            branch: "broadway/no-task".to_string(),
            original_author: "broadway".to_string(),
            stored_at: Utc::now(),
            task_id: None,
        },
    );
    persistent_state.github.pr_author_sessions = pr_sessions;

    let completed_task_id = "42";
    persistent_state
        .github
        .pr_author_sessions
        .retain(|_, session| session.task_id.as_deref() != Some(completed_task_id));

    assert!(
        !persistent_state
            .github
            .pr_author_sessions
            .contains_key(&1001)
    );
    assert!(
        persistent_state
            .github
            .pr_author_sessions
            .contains_key(&1002)
    );
    assert!(
        !persistent_state
            .github
            .pr_author_sessions
            .contains_key(&1003)
    );
    assert!(
        persistent_state
            .github
            .pr_author_sessions
            .contains_key(&1004)
    );
    assert_eq!(persistent_state.github.pr_author_sessions.len(), 2);
}

#[tokio::test]
async fn test_cleanup_merged_worktree_removes_pr_author_session() {
    use crate::daemon::state::DaemonPersistentState;
    use crate::github_state::PrAuthorSession;
    use chrono::Utc;
    use std::collections::HashMap;

    let mut persistent_state = DaemonPersistentState::default();
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

    let merged_pr_number = 1001;
    persistent_state
        .github
        .pr_author_sessions
        .remove(&merged_pr_number);

    assert!(
        !persistent_state
            .github
            .pr_author_sessions
            .contains_key(&1001)
    );
    assert!(
        persistent_state
            .github
            .pr_author_sessions
            .contains_key(&1002)
    );
    assert_eq!(persistent_state.github.pr_author_sessions.len(), 1);
}

// ── Session-centric effect tests ──────────────────────────────────────

#[test]
fn test_record_session_inserts_into_persistent_state() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};

    let mut persistent_state = DaemonPersistentState::default();
    let record = SessionRecord {
        session_id: "sess-abc-123".to_string(),
        task_id: Some("42".to_string()),
        current_name: Some("lexington".to_string()),
        preferred_name: Some("lexington".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        branch: Some("lexington/task-42".to_string()),
        initial_prompt: Some("Work on task 42".to_string()),
        is_running: true,
        ..Default::default()
    };

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

    let mut persistent_state = DaemonPersistentState::default();
    let record = SessionRecord {
        session_id: "sess-abc-123".to_string(),
        task_id: Some("42".to_string()),
        current_name: Some("lexington".to_string()),
        preferred_name: Some("lexington".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        is_running: true,
        ..Default::default()
    };
    persistent_state
        .sessions
        .insert(record.session_id.clone(), record);

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
    assert_eq!(stored.preferred_name.as_deref(), Some("lexington"));
}

#[test]
fn test_release_name_frees_name_in_pool() {
    use crate::name_pool::NamePool;

    let mut pool = NamePool::new(&["lexington", "park", "madison"]);
    let name = pool.allocate(None).unwrap();
    assert_eq!(name, "lexington");
    assert!(pool.is_allocated("lexington"));
    assert_eq!(pool.available_count(), 2);

    pool.release(&name);
    assert!(!pool.is_allocated("lexington"));
    assert_eq!(pool.available_count(), 3);

    assert_eq!(pool.allocate(None).unwrap(), "park");
    assert_eq!(pool.allocate(None).unwrap(), "madison");
    assert_eq!(pool.allocate(None).unwrap(), "lexington");
}

#[test]
fn test_shutdown_session_marks_not_running() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};

    let mut persistent_state = DaemonPersistentState::default();
    let record = SessionRecord {
        session_id: "sess-abc-123".to_string(),
        task_id: Some("42".to_string()),
        current_name: Some("lexington".to_string()),
        preferred_name: Some("lexington".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        is_running: true,
        ..Default::default()
    };
    persistent_state
        .sessions
        .insert(record.session_id.clone(), record);

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
    pool.allocate(None);
    pool.allocate(None);
    pool.release("lexington");

    let excluded: HashSet<String> = HashSet::new();
    let name = pool.allocate_excluding(Some("lexington"), &excluded);
    assert_eq!(name.as_deref(), Some("lexington"));
}

#[test]
fn test_spawn_session_excludes_channel_leads() {
    use crate::name_pool::NamePool;
    use std::collections::HashSet;

    let mut pool = NamePool::new(&["lexington", "park", "madison"]);
    let channel_leads: HashSet<String> = ["lexington"].iter().map(|s| s.to_string()).collect();
    let name = pool.allocate_excluding(None, &channel_leads);
    assert_eq!(name.as_deref(), Some("park"));
}

#[test]
fn test_reverse_maps_consistency() {
    use std::collections::HashMap;

    let mut name_to_session: HashMap<String, String> = HashMap::new();
    let mut session_to_name: HashMap<String, String> = HashMap::new();
    let mut task_to_session: HashMap<String, String> = HashMap::new();

    let session_id = "sess-abc-123".to_string();
    let name = "lexington".to_string();
    let task_id = "42".to_string();
    name_to_session.insert(name.clone(), session_id.clone());
    session_to_name.insert(session_id.clone(), name.clone());
    task_to_session.insert(task_id.clone(), session_id.clone());

    assert_eq!(name_to_session.get("lexington"), Some(&session_id));
    assert_eq!(session_to_name.get("sess-abc-123"), Some(&name));
    assert_eq!(task_to_session.get("42"), Some(&session_id));

    let removed_session = name_to_session.remove("lexington");
    if let Some(ref sid) = removed_session {
        session_to_name.remove(sid);
    }

    assert!(name_to_session.is_empty());
    assert!(session_to_name.is_empty());
    assert_eq!(task_to_session.get("42"), Some(&session_id));
}

#[test]
fn test_coworker_break_updates_session_record() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};
    use std::collections::HashMap;

    let mut persistent_state = DaemonPersistentState::default();
    let record = SessionRecord {
        session_id: "sess-abc-123".to_string(),
        task_id: Some("42".to_string()),
        current_name: Some("lexington".to_string()),
        preferred_name: Some("lexington".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        is_running: true,
        ..Default::default()
    };
    persistent_state
        .sessions
        .insert(record.session_id.clone(), record);

    let mut name_to_session: HashMap<String, String> = HashMap::new();
    name_to_session.insert("lexington".to_string(), "sess-abc-123".to_string());
    let session_id = name_to_session.get("lexington").cloned();

    if let Some(session_id) = session_id
        && let Some(record) = persistent_state.sessions.get_mut(&session_id)
    {
        record.is_running = false;
        record.current_name = None;
    }

    let stored = persistent_state.sessions.get("sess-abc-123").unwrap();
    assert!(!stored.is_running);
    assert!(stored.current_name.is_none());
}

#[test]
fn test_shutdown_coworker_impl_updates_session_via_name_lookup() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};
    use crate::name_pool::NamePool;
    use std::collections::HashMap;

    let mut persistent_state = DaemonPersistentState::default();
    let mut name_to_session: HashMap<String, String> = HashMap::new();
    let mut session_to_name: HashMap<String, String> = HashMap::new();
    let mut pool = NamePool::new(&["lexington", "park", "madison"]);

    let record = SessionRecord {
        session_id: "sess-123".to_string(),
        task_id: Some("42".to_string()),
        current_name: Some("lexington".to_string()),
        preferred_name: Some("lexington".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        is_running: true,
        ..Default::default()
    };
    persistent_state
        .sessions
        .insert(record.session_id.clone(), record);
    name_to_session.insert("lexington".to_string(), "sess-123".to_string());
    session_to_name.insert("sess-123".to_string(), "lexington".to_string());
    pool.allocate(Some("lexington"));

    let session_id = name_to_session.get("lexington").cloned();
    if let Some(session_id) = &session_id
        && let Some(sr) = persistent_state.sessions.get_mut(session_id)
    {
        sr.is_running = false;
        sr.current_name = None;
    }
    pool.release("lexington");
    name_to_session.remove("lexington");
    if let Some(sid) = session_id {
        session_to_name.remove(&sid);
    }

    let stored = persistent_state.sessions.get("sess-123").unwrap();
    assert!(!stored.is_running);
    assert!(stored.current_name.is_none());
    assert_eq!(stored.preferred_name.as_deref(), Some("lexington"));
    assert!(!pool.is_allocated("lexington"));
    assert_eq!(pool.available_count(), 3);
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

    let midtown_dir = tempdir().unwrap();
    let _midtown_guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());
    let repo_name = "test-repo";

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
    persistent_state.save_for_repo(repo_name).unwrap();

    let loaded_before = DaemonPersistentState::load_for_repo(repo_name).unwrap();
    assert!(loaded_before.github.pr_author_sessions.contains_key(&2001));

    let pr_session_removed = persistent_state.github.pr_author_sessions.remove(&2001);
    assert!(pr_session_removed.is_some());
    persistent_state.save_for_repo(repo_name).unwrap();

    let loaded_after = DaemonPersistentState::load_for_repo(repo_name).unwrap();
    assert!(!loaded_after.github.pr_author_sessions.contains_key(&2001));
}

#[test]
fn test_spawn_session_marks_old_records_with_same_name_as_not_running() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};
    use chrono::Utc;

    let mut persistent_state = DaemonPersistentState::default();

    let old_record = SessionRecord {
        session_id: "sess-old-111".to_string(),
        task_id: Some("42".to_string()),
        current_name: Some("riverside".to_string()),
        preferred_name: Some("riverside".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        is_running: true,
        created_at: Utc::now() - chrono::Duration::hours(1),
        ..Default::default()
    };
    persistent_state
        .sessions
        .insert(old_record.session_id.clone(), old_record);

    let old_reviewer = SessionRecord {
        session_id: "sess-old-222".to_string(),
        current_name: Some("riverside".to_string()),
        preferred_name: Some("riverside".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        pr_number: Some(100),
        is_reviewer: true,
        coworker_type: "reviewer".to_string(),
        is_running: true,
        created_at: Utc::now() - chrono::Duration::minutes(30),
        resume_on_startup: false,
        ..Default::default()
    };
    persistent_state
        .sessions
        .insert(old_reviewer.session_id.clone(), old_reviewer);

    let unrelated = SessionRecord {
        session_id: "sess-amsterdam".to_string(),
        task_id: Some("99".to_string()),
        current_name: Some("amsterdam".to_string()),
        preferred_name: Some("amsterdam".to_string()),
        working_dir: "/tmp/worktree".to_string(),
        is_running: true,
        ..Default::default()
    };
    persistent_state
        .sessions
        .insert(unrelated.session_id.clone(), unrelated);

    let new_session_id = "sess-new-333";
    let effective_name = "riverside";

    for record in persistent_state.sessions.values_mut() {
        if record.session_id != new_session_id
            && record.is_running
            && (record.preferred_name.as_deref() == Some(effective_name)
                || record.current_name.as_deref() == Some(effective_name))
        {
            record.is_running = false;
        }
    }

    let new_record = SessionRecord {
        session_id: new_session_id.to_string(),
        task_id: Some("50".to_string()),
        current_name: Some(effective_name.to_string()),
        preferred_name: Some(effective_name.to_string()),
        working_dir: "/tmp/worktree".to_string(),
        is_running: true,
        ..Default::default()
    };
    persistent_state
        .sessions
        .insert(new_record.session_id.clone(), new_record);

    assert!(!persistent_state.sessions["sess-old-111"].is_running);
    assert!(!persistent_state.sessions["sess-old-222"].is_running);
    assert!(persistent_state.sessions[new_session_id].is_running);
    assert!(persistent_state.sessions["sess-amsterdam"].is_running);
}

/// Test that the ClearSessionWorkingDir handler clears a stale working_dir
/// from a session record. Mirrors the inline effect handler logic (lock state,
/// clear field) without requiring a full DaemonState.
#[test]
fn clear_session_working_dir_clears_stale_path() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    ps.sessions.insert(
        "sess-stale".to_string(),
        crate::daemon::state::SessionRecord {
            session_id: "sess-stale".to_string(),
            working_dir: "/tmp/deleted-worktree".to_string(),
            ..Default::default()
        },
    );
    ps.sessions.insert(
        "sess-valid".to_string(),
        crate::daemon::state::SessionRecord {
            session_id: "sess-valid".to_string(),
            working_dir: "/tmp/existing-worktree".to_string(),
            ..Default::default()
        },
    );

    // Simulate ClearSessionWorkingDir handler: clear the stale session's working_dir
    let session_id = "sess-stale";
    if let Some(record) = ps.sessions.get_mut(session_id) {
        record.working_dir = String::new();
    }

    assert!(
        ps.sessions["sess-stale"].working_dir.is_empty(),
        "stale session's working_dir should be cleared"
    );
    assert_eq!(
        ps.sessions["sess-valid"].working_dir, "/tmp/existing-worktree",
        "other sessions' working_dir should be untouched"
    );
}

/// Test that ClearSessionWorkingDir is a no-op when the session doesn't exist.
#[test]
fn clear_session_working_dir_noop_for_missing_session() {
    use crate::daemon::state::DaemonPersistentState;

    let mut ps = DaemonPersistentState::default();
    ps.sessions.insert(
        "sess-existing".to_string(),
        crate::daemon::state::SessionRecord {
            session_id: "sess-existing".to_string(),
            working_dir: "/tmp/worktree".to_string(),
            ..Default::default()
        },
    );

    // Simulate ClearSessionWorkingDir for a nonexistent session — should not panic
    let session_id = "sess-nonexistent";
    if let Some(record) = ps.sessions.get_mut(session_id) {
        record.working_dir = String::new();
    }

    assert_eq!(
        ps.sessions["sess-existing"].working_dir, "/tmp/worktree",
        "existing session should be untouched"
    );
    assert!(
        !ps.sessions.contains_key("sess-nonexistent"),
        "no phantom session record should be created"
    );
}

// ── invoke_workflow_script ────────────────────────────────────────────────────

/// Mutex to serialize tests that modify the PATH environment variable.
static WORKFLOW_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Build a minimal DaemonState for workflow-script tests.
///
/// Returns the state, the project root temp dir (which becomes `all_repo_paths[0]`),
/// and the midtown base dir guard (must stay alive for the test's duration).
fn make_workflow_test_state(
    repo_name: &str,
) -> (
    DaemonState,
    tempfile::TempDir,
    crate::paths::TestMidtownBaseDirGuard,
) {
    use std::process::Command;

    // Redirect ~/.midtown/ to a temp dir so paths resolve under test.
    let midtown_dir = tempfile::tempdir().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    // Create a minimal git repo so DaemonState::new is happy.
    let project_dir = tempfile::tempdir().expect("project temp dir");
    Command::new("git")
        .args(["init"])
        .current_dir(project_dir.path())
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(project_dir.path())
        .output()
        .expect("git config email");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(project_dir.path())
        .output()
        .expect("git config name");
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(project_dir.path())
        .output()
        .expect("git commit");

    let wm = crate::worktree::WorktreeManager::new(project_dir.path().to_path_buf()).expect("wm");
    let cm = crate::coworker::CoworkerManager::new(wm);
    let channel_router = crate::ChannelRouter::new(project_dir.path(), "midtown");
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let state = DaemonState::new(
        "/tmp/workflow-test.sock".into(),
        cm,
        repo_name.to_string(),
        vec![project_dir.path().to_path_buf()],
        channel_router,
        None,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state");

    (state, project_dir, _guard)
}

#[tokio::test]
async fn emit_workflow_event_noop_when_no_script_configured() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo");

    let event = crate::workflow::WorkflowEvent::TimerTick {
        channel: "test-channel".into(),
    };

    // No workflow.py anywhere → function should return without posting anything.
    invoke_workflow_script(&state, event).await;

    // The channel JSONL should not exist (no messages were written).
    let channel_file = crate::paths::projects_dir_for_repo("myrepo")
        .join("channels")
        .join("test-channel")
        .join("history")
        .join("current.jsonl");
    assert!(
        !channel_file.exists(),
        "no channel message should be written when no workflow script is configured"
    );
}

#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await.
#[tokio::test]
async fn emit_workflow_event_posts_error_on_nonzero_exit() {
    use std::os::unix::fs::PermissionsExt;

    let _path_guard = WORKFLOW_PATH_LOCK.lock().unwrap();
    let (state, project_dir, _guard) = make_workflow_test_state("myrepo-err");

    // Write a workflow script that exits non-zero with a stderr message.
    let script_dir = project_dir
        .path()
        .join(".midtown")
        .join("channels")
        .join("err-channel");
    std::fs::create_dir_all(&script_dir).unwrap();
    let script = script_dir.join("workflow.py");
    std::fs::write(
        &script,
        "#!/bin/sh\necho 'something went wrong' >&2\nexit 1",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Write a fake `uv` that strips "run" and exec's the script directly.
    let bin_dir = project_dir.path().join("fake-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_uv = bin_dir.join("uv");
    std::fs::write(&fake_uv, "#!/bin/sh\nshift\nexec \"$@\"").unwrap();
    std::fs::set_permissions(&fake_uv, std::fs::Permissions::from_mode(0o755)).unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), original_path));
    }

    let event = crate::workflow::WorkflowEvent::TimerTick {
        channel: "err-channel".into(),
    };
    invoke_workflow_script(&state, event).await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    // A system message should have been written to the err-channel JSONL.
    // The channel router uses project_dir as its base_dir, so messages land in
    // project_dir/channels/<channel>/history/current.jsonl (not ~/.midtown/...).
    let channel_file = project_dir
        .path()
        .join("channels")
        .join("err-channel")
        .join("history")
        .join("current.jsonl");
    assert!(
        channel_file.exists(),
        "error message should be written to the channel when the script fails"
    );
    let content = std::fs::read_to_string(&channel_file).unwrap();
    assert!(
        content.contains("workflow.py"),
        "error message should identify the script; got: {content}"
    );
}

#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await.
#[tokio::test]
async fn emit_workflow_event_no_error_message_on_success() {
    use std::os::unix::fs::PermissionsExt;

    let _path_guard = WORKFLOW_PATH_LOCK.lock().unwrap();
    let (state, project_dir, _guard) = make_workflow_test_state("myrepo-ok");

    // Write a workflow script that exits 0 (success).
    let script_dir = project_dir
        .path()
        .join(".midtown")
        .join("channels")
        .join("ok-channel");
    std::fs::create_dir_all(&script_dir).unwrap();
    let script = script_dir.join("workflow.py");
    std::fs::write(&script, "#!/bin/sh\nexit 0").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Write a fake `uv` that strips "run" and exec's the script directly.
    let bin_dir = project_dir.path().join("fake-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_uv = bin_dir.join("uv");
    std::fs::write(&fake_uv, "#!/bin/sh\nshift\nexec \"$@\"").unwrap();
    std::fs::set_permissions(&fake_uv, std::fs::Permissions::from_mode(0o755)).unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), original_path));
    }

    let event = crate::workflow::WorkflowEvent::TimerTick {
        channel: "ok-channel".into(),
    };
    invoke_workflow_script(&state, event).await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    // No error message should have been written (success = silent).
    // Channel router base_dir = project_dir.path(), so messages would be here.
    let channel_file = project_dir
        .path()
        .join("channels")
        .join("ok-channel")
        .join("history")
        .join("current.jsonl");
    assert!(
        !channel_file.exists(),
        "no channel message should be written on successful script exit"
    );
}
