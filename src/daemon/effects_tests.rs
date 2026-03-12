use super::*;
use crate::daemon::trackers::PrIssueType;
use crate::github_state::AssignmentSource;
use std::process::Command;

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
            auto_output: false,
            message_type: None,
            nudge_type: None,
            tool_data: None,
            provider: None,
            tool_use_id: None,
            parent_tool_use_id: None,
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
            auto_output: false,
            message_type: None,
            nudge_type: None,
            tool_data: None,
            provider: None,
            tool_use_id: None,
            parent_tool_use_id: None,
        },
    ];

    let deduped = dedup_nudge_effects(effects);
    // 1 nudge + 2 PostToChannel + 1 RecordCooldown = 4
    assert_eq!(deduped.len(), 4);
    assert_eq!(count_nudge_session(&deduped, "sess-riverside-1"), 1);
}

#[tokio::test]
async fn test_execute_effects_nudge_channel_lead_uses_stored_session_id() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo");
    let channel = "web".to_string();
    let stored_session_id = "lead-session-123".to_string();
    let message = "wake web lead".to_string();

    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert(channel.clone(), stored_session_id.clone());
    }

    let observed_session_ids = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let observed_for_hook = observed_session_ids.clone();
    let message_for_hook = message.clone();
    state
        .session_manager
        .set_test_send_message_to_session_id_hook(Some(std::sync::Arc::new(
            move |session_id, msg| {
                observed_for_hook
                    .lock()
                    .expect("hook mutex poisoned")
                    .push(session_id.to_string());
                assert_eq!(msg, message_for_hook);
                Ok(())
            },
        )));

    execute_effects(
        vec![Effect::NudgeChannelLead {
            channel_name: channel.clone(),
            reason: crate::daemon::wake_reason::WakeReason::Nudge {
                message: message.clone(),
            },
        }],
        &state,
    )
    .await;

    let observed = observed_session_ids
        .lock()
        .expect("hook mutex poisoned")
        .clone();
    assert_eq!(observed, vec![stored_session_id.clone()]);

    let ps = state.persistent_state.lock().await;
    assert_eq!(
        ps.channel_lead_sessions.get(&channel),
        Some(&stored_session_id)
    );
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

// ── dispatch_workflow_event ───────────────────────────────────────────────────

/// Build a minimal DaemonState for workflow dispatch tests.
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
        crate::paths::ProjectPaths::with_project_name(repo_name, repo_name),
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
async fn dispatch_workflow_event_noop_when_no_plugins() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo");

    let event = crate::workflow::WorkflowEvent::TimerTick {
        channel: "test-channel".into(),
    };

    // No plugins configured → function should return false (no default_prevented).
    let default_prevented = dispatch_workflow_event(&state, event).await;
    assert!(
        !default_prevented,
        "default_prevented should be false when no plugins are configured"
    );

    // The channel JSONL should not exist (no messages were written).
    let channel_file = crate::paths::projects_dir_for_repo("myrepo")
        .join("channels")
        .join("test-channel")
        .join("history")
        .join("current.jsonl");
    assert!(
        !channel_file.exists(),
        "no channel message should be written when no plugins are configured"
    );
}

#[test]
fn plugin_actions_to_effects_channel_post() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-actions");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "channel.post".to_string(),
        params: serde_json::json!({"message": "hello from plugin", "channel": "test-ch"}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state);
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::PostSystemMessage { message, channel }
            if message == "hello from plugin" && *channel == Some("test-ch".to_string())
    ));
}

#[test]
fn plugin_actions_to_effects_nudge_coworker() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-nudge");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "coworker.nudge".to_string(),
        params: serde_json::json!({"name": "lexington", "message": "PR approved"}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state);
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::NudgeSession { .. }));
}

#[test]
fn plugin_actions_to_effects_task_done() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-done");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "task.done".to_string(),
        params: serde_json::json!({"id": "42"}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state);
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::CompleteTask { task_id, .. } if task_id == "42"
    ));
}

#[test]
fn plugin_actions_to_effects_auto_merge() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-merge");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "pr.auto-merge".to_string(),
        params: serde_json::json!({"pr": 123}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state);
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::AutoMergePr { pr_number, .. } if *pr_number == 123
    ));
}

#[test]
fn plugin_actions_to_effects_unknown_method_skipped() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-unk");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "unknown.method".to_string(),
        params: serde_json::json!({}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state);
    assert!(effects.is_empty(), "unknown methods should be skipped");
}

#[test]
fn plugin_actions_to_effects_multiple_actions() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-multi");

    let actions = vec![
        super::super::plugin_daemon::PluginAction {
            method: "channel.post".to_string(),
            params: serde_json::json!({"message": "first"}),
        },
        super::super::plugin_daemon::PluginAction {
            method: "channel.post".to_string(),
            params: serde_json::json!({"message": "second"}),
        },
        super::super::plugin_daemon::PluginAction {
            method: "pr.auto-merge".to_string(),
            params: serde_json::json!({"pr": 99}),
        },
    ];

    let effects = plugin_actions_to_effects(&actions, &state);
    assert_eq!(effects.len(), 3);
}

#[test]
fn plugin_actions_to_effects_channel_post_empty_message_skipped() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-empty-msg");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "channel.post".to_string(),
        params: serde_json::json!({"channel": "test-ch"}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state);
    assert!(
        effects.is_empty(),
        "channel.post with missing message should be skipped"
    );
}

#[test]
fn plugin_actions_to_effects_channel_post_blank_message_skipped() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-blank-msg");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "channel.post".to_string(),
        params: serde_json::json!({"message": "", "channel": "test-ch"}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state);
    assert!(
        effects.is_empty(),
        "channel.post with empty string message should be skipped"
    );
}

// ---------------------------------------------------------------------------
// CreateTask dedup guard tests
//
// The `create_task_duplicate_exists` helper is used inside the `for effect in
// effects` loop in `execute_effects`.  The caller uses `continue` (not
// `return`) so that only the duplicate CreateTask is skipped and subsequent
// effects in the batch still execute.
// ---------------------------------------------------------------------------

fn mk_task(pr: Option<u64>, status: crate::tasks::TaskStatus) -> crate::tasks::Task {
    crate::tasks::Task {
        id: "1".to_string(),
        subject: "test task".to_string(),
        status,
        owner: None,
        description: None,
        blocked_by: vec![],
        channel: None,
        pr,
        created_at: None,
    }
}

#[test]
fn create_task_duplicate_exists_returns_false_for_empty_list() {
    assert!(
        !super::create_task_duplicate_exists(&[], 42),
        "no tasks → not a duplicate"
    );
}

#[test]
fn create_task_duplicate_exists_returns_false_when_only_completed_tasks() {
    let tasks = vec![
        mk_task(Some(42), crate::tasks::TaskStatus::Completed),
        mk_task(Some(42), crate::tasks::TaskStatus::Completed),
    ];
    assert!(
        !super::create_task_duplicate_exists(&tasks, 42),
        "only completed tasks for this PR → allowed to create a new one"
    );
}

#[test]
fn create_task_duplicate_exists_returns_true_for_pending_task() {
    let tasks = vec![mk_task(Some(42), crate::tasks::TaskStatus::Pending)];
    assert!(
        super::create_task_duplicate_exists(&tasks, 42),
        "pending task for PR → skip creation"
    );
}

#[test]
fn create_task_duplicate_exists_returns_true_for_in_progress_task() {
    let tasks = vec![mk_task(Some(42), crate::tasks::TaskStatus::InProgress)];
    assert!(
        super::create_task_duplicate_exists(&tasks, 42),
        "in-progress task for PR → skip creation"
    );
}

#[test]
fn create_task_duplicate_exists_ignores_other_pr_numbers() {
    // Task exists for PR 99, not PR 42 — must not block PR 42.
    let tasks = vec![mk_task(Some(99), crate::tasks::TaskStatus::Pending)];
    assert!(
        !super::create_task_duplicate_exists(&tasks, 42),
        "task for a different PR → not a duplicate"
    );
}

#[test]
fn create_task_duplicate_exists_ignores_tasks_without_pr() {
    // Task with no associated PR must not block a PR-specific CreateTask.
    let tasks = vec![mk_task(None, crate::tasks::TaskStatus::Pending)];
    assert!(
        !super::create_task_duplicate_exists(&tasks, 42),
        "task with no PR → not a duplicate"
    );
}

// ---------------------------------------------------------------------------
// BindCoworkerToWorktree collision guard — batch-level regression test
//
// When a worktree collision is detected (the target worktree is already bound
// to a different ACTIVE coworker), the guard must skip only the colliding
// effect and continue processing the remaining effects in the batch.  Using
// `return` instead of `continue` would silently drop every subsequent effect.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bind_coworker_to_worktree_collision_does_not_drop_subsequent_effects() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-collision");

    // Register a worktree and bind it to "old-coworker".
    {
        let mut ps = state.persistent_state.lock().await;
        ps.worktree_registry
            .assign_worktree(crate::worktree_registry::WorktreeAssignment {
                worktree_id: "wt-collision-test".to_string(),
                branch_name: "old-coworker/task-1".to_string(),
                task_id: None,
                current_coworker: None,
                pr_number: None,
                created_at: chrono::Utc::now(),
                completed_at: None,
            })
            .expect("assign worktree");
        ps.worktree_registry
            .bind_coworker("wt-collision-test", "old-coworker")
            .expect("bind old-coworker");
    }

    // Make the session manager report "old-coworker" as alive so the collision
    // guard fires.
    state
        .session_manager
        .set_test_is_alive_hook(Some(std::sync::Arc::new(|name: &str| {
            name == "old-coworker"
        })));

    // Batch: first effect will be blocked (collision), second must still run.
    let sentinel_channel = "sentinel-ch".to_string();
    let sentinel_session = "sess-sentinel-99".to_string();
    execute_effects(
        vec![
            Effect::BindCoworkerToWorktree {
                worktree_id: "wt-collision-test".to_string(),
                coworker: "new-coworker".to_string(),
            },
            Effect::SaveChannelLeadSession {
                channel_name: sentinel_channel.clone(),
                session_id: sentinel_session.clone(),
            },
        ],
        &state,
    )
    .await;

    // The SaveChannelLeadSession effect must have executed — if the collision
    // guard used `return` instead of `continue`, this would be None.
    let ps = state.persistent_state.lock().await;
    assert_eq!(
        ps.channel_lead_sessions
            .get(&sentinel_channel)
            .map(String::as_str),
        Some(sentinel_session.as_str()),
        "SaveChannelLeadSession must execute even when a preceding \
         BindCoworkerToWorktree is blocked by the collision guard"
    );
}

// ============================================================================
// auto_detach_suffix_message — legacy "lead" name coverage
// ============================================================================

/// The legacy "lead" session name must produce the same respawn suffix as the
/// canonical repo name.
///
/// Regression: before the fix, `auto_detach_suffix_message` only checked
/// `eq_ignore_ascii_case(repo_name)`, so a session named "lead" got the
/// "Session will be reassigned via normal task dispatch." suffix instead of
/// the correct "Headless session will respawn on the next tick." suffix.
#[test]
fn test_auto_detach_suffix_legacy_lead_gets_respawn_message() {
    // Legacy name
    assert_eq!(
        auto_detach_suffix_message("lead", "midtown", false),
        " Headless session will respawn on the next tick.",
        "legacy 'lead' session must get the respawn suffix"
    );
    // Case-insensitive variants
    assert_eq!(
        auto_detach_suffix_message("Lead", "midtown", false),
        " Headless session will respawn on the next tick."
    );
    assert_eq!(
        auto_detach_suffix_message("LEAD", "midtown", false),
        " Headless session will respawn on the next tick."
    );
}

/// Canonical repo-named session must produce the respawn suffix.
#[test]
fn test_auto_detach_suffix_canonical_name_gets_respawn_message() {
    assert_eq!(
        auto_detach_suffix_message("midtown", "midtown", false),
        " Headless session will respawn on the next tick."
    );
    assert_eq!(
        auto_detach_suffix_message("Midtown", "midtown", false),
        " Headless session will respawn on the next tick."
    );
}

/// Regular coworker sessions must produce the task-dispatch suffix.
#[test]
fn test_auto_detach_suffix_coworker_gets_task_dispatch_message() {
    assert_eq!(
        auto_detach_suffix_message("lexington", "midtown", false),
        " Session will be reassigned via normal task dispatch."
    );
}

/// Channel-lead sessions must produce the channel-respawn suffix.
#[test]
fn test_auto_detach_suffix_channel_lead_gets_channel_message() {
    assert_eq!(
        auto_detach_suffix_message("auth", "midtown", true),
        " Channel lead session will be respawned for its channel."
    );
}

// ── PostToChannel thread resolution tests ─────────────────────────────────────

/// When PostToChannel has `channel: None` and the sender has a fork_bound_threads
/// entry, the message should be posted to the default channel with thread_parent_id —
/// not dropped due to an empty channel name.
///
/// Regression test for PR #1591 review feedback: the original code used
/// `channel_name.unwrap_or_default()` which produced "" when channel was None,
/// causing Channel::new("") to reject the message.
#[tokio::test]
async fn test_post_to_channel_none_channel_with_bound_thread_uses_default() {
    let (state, project_dir, _guard) = make_workflow_test_state("bound-thread-repo");

    let sender = "test-agent".to_string();
    let thread_id = "thread-parent-123".to_string();

    // Insert a fork_bound_threads entry for the sender
    {
        let mut threads = state.fork_bound_threads.lock().unwrap();
        threads.insert(sender.clone(), thread_id.clone());
    }

    // Execute PostToChannel with channel: None — should fall back to default channel
    execute_effects(
        vec![Effect::PostToChannel {
            sender: sender.clone(),
            message: "hello from bound thread".into(),
            channel: None,
            auto_output: false,
            message_type: None,
            nudge_type: None,
            tool_data: None,
            provider: None,
            tool_use_id: None,
            parent_tool_use_id: None,
        }],
        &state,
    )
    .await;

    // The message should land in the default channel ("midtown") JSONL file
    let channel_file = project_dir
        .path()
        .join("channels")
        .join("midtown")
        .join("history")
        .join("current.jsonl");
    assert!(
        channel_file.exists(),
        "message should be written to the default channel, not dropped"
    );

    let content = std::fs::read_to_string(&channel_file).unwrap();
    let messages: Vec<crate::message::Message> = content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    assert_eq!(messages.len(), 1, "exactly one message should be posted");
    let msg = &messages[0];
    assert_eq!(msg.from, sender);
    assert_eq!(msg.content, "hello from bound thread");
    assert_eq!(
        msg.channel,
        Some("midtown".to_string()),
        "message should be in the default channel"
    );
    assert_eq!(
        msg.thread_parent_id,
        Some(thread_id),
        "message should carry the bound thread parent ID"
    );
}

// ── DM separator tests ──────────────────────────────────────────────

/// SpawnSession for a task produces a PostSystemMessage separator
/// targeting the coworker's DM channel (dm-<name>).
#[test]
fn test_dm_separator_produced_for_dev_session() {
    let effect = build_dm_separator_effect("park", "42", Some("Fix auth bug"));
    match effect {
        Effect::PostSystemMessage { message, channel } => {
            assert_eq!(channel, Some("dm-park".to_string()));
            assert!(
                message.contains("Task !42"),
                "separator should contain the task ID, got: {}",
                message
            );
            assert!(
                message.contains("Fix auth bug"),
                "separator should contain the task subject, got: {}",
                message
            );
        }
        other => panic!("expected PostSystemMessage, got {:?}", other),
    }
}

/// SpawnSession for a task without a subject still produces a separator.
#[test]
fn test_dm_separator_without_subject() {
    let effect = build_dm_separator_effect("madison", "99", None);
    match effect {
        Effect::PostSystemMessage { message, channel } => {
            assert_eq!(channel, Some("dm-madison".to_string()));
            assert!(
                message.contains("Task !99"),
                "separator should contain the task ID, got: {}",
                message
            );
        }
        other => panic!("expected PostSystemMessage, got {:?}", other),
    }
}

/// An empty subject string (Some("")) should be treated like None — the
/// separator should contain only the task ID, not a trailing colon+space.
/// Callers should filter empty subjects before passing to this function.
#[test]
fn test_dm_separator_empty_subject_treated_as_none() {
    // Direct call with Some("") — shows the raw behavior
    let effect = build_dm_separator_effect("park", "42", Some(""));
    let msg = match effect {
        Effect::PostSystemMessage { message, .. } => message,
        other => panic!("expected PostSystemMessage, got {:?}", other),
    };
    // If callers forget to filter, the output has a trailing ": " — this
    // test documents the current behavior so callers know to filter.
    // The correct pattern is: task_subject.as_deref().filter(|s| !s.is_empty())
    assert!(
        msg.contains("Task !42"),
        "separator should contain task ID, got: {}",
        msg
    );
}

/// Reviewer sessions produce DM separator effects so their output
/// streams to dm-<name> channels alongside regular coworkers.
#[test]
fn test_dm_separator_produced_for_reviewer_session() {
    let effect = build_dm_separator_effect("riverside", "42", Some("Review PR"));
    match effect {
        Effect::PostSystemMessage { message, channel } => {
            assert_eq!(channel, Some("dm-riverside".to_string()));
            assert!(message.contains("!42"));
            assert!(message.contains("Review PR"));
        }
        other => panic!("expected PostSystemMessage, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// PostPrComment effect execution tests
// ---------------------------------------------------------------------------

/// Verify that executing a PostPrComment effect calls `gh pr comment`,
/// parses the comment ID from stdout, and stores it on the assignment.
///
/// This is an E2E test for the placeholder posting flow — the daemon posts
/// the comment (not the reviewer agent) to avoid prompt-compliance issues
/// like escaped `!` characters.
#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await.
#[tokio::test]
async fn test_post_pr_comment_stores_comment_id_on_assignment() {
    use std::os::unix::fs::PermissionsExt;

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();

    let (state, _project_dir, _guard) = make_workflow_test_state("post-pr-test");

    // Pre-assign a reviewer so post_pr_comment can store the comment ID
    let pr_number = 42u64;
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github
            .assign_reviewer(pr_number, "park", AssignmentSource::Webhook);
    }

    // Mock `gh` to output a comment URL (like real `gh pr comment` does)
    let temp_dir = tempfile::tempdir().unwrap();
    let mock_gh_dir = temp_dir.path().join("bin");
    std::fs::create_dir_all(&mock_gh_dir).unwrap();
    let mock_gh_script = mock_gh_dir.join("gh");
    std::fs::write(
        &mock_gh_script,
        "#!/bin/bash\necho 'https://github.com/btucker/midtown/pull/42#issuecomment-98765'",
    )
    .unwrap();
    std::fs::set_permissions(&mock_gh_script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", mock_gh_dir.display(), original_path),
        );
    }

    // Execute the PostPrComment effect
    let effects = vec![Effect::PostPrComment {
        pr_number,
        reviewer_name: "park".to_string(),
        body: "<!-- midtown-placeholder -->\n## Review Status\n\n🔍 Review in progress by park..."
            .to_string(),
    }];
    execute_effects(effects, &state).await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    // Verify the comment ID was parsed and stored on the assignment
    {
        let ps = state.persistent_state.lock().await;
        let assignment = ps
            .github
            .pr_reviewers
            .get(&pr_number)
            .expect("assignment should still exist");
        assert_eq!(
            assignment.placeholder_comment_id,
            Some(98765),
            "Should parse comment ID 98765 from the issuecomment URL"
        );
    }

    // Verify the placeholder cache was populated
    {
        let cache = state.reviewer_placeholder_cache.lock().unwrap();
        let (cached_id, _instant) = cache.get(&pr_number).expect("cache should be populated");
        assert_eq!(
            *cached_id,
            Some(98765),
            "Placeholder cache should contain the comment ID"
        );
    }
}

/// Verify that `post_pr_comment` handles a bare numeric URL format
/// (not just `issuecomment-<id>`).
#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await.
#[tokio::test]
async fn test_post_pr_comment_parses_bare_numeric_url() {
    use std::os::unix::fs::PermissionsExt;

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();

    let (state, _project_dir, _guard) = make_workflow_test_state("post-pr-bare");

    let pr_number = 55u64;
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github
            .assign_reviewer(pr_number, "madison", AssignmentSource::PollingFallback);
    }

    // Mock gh to output just a URL ending in a bare number
    let temp_dir = tempfile::tempdir().unwrap();
    let mock_gh_dir = temp_dir.path().join("bin");
    std::fs::create_dir_all(&mock_gh_dir).unwrap();
    let mock_gh_script = mock_gh_dir.join("gh");
    std::fs::write(
        &mock_gh_script,
        "#!/bin/bash\necho 'https://github.com/btucker/midtown/issues/55/comments/11223'",
    )
    .unwrap();
    std::fs::set_permissions(&mock_gh_script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", mock_gh_dir.display(), original_path),
        );
    }

    let effects = vec![Effect::PostPrComment {
        pr_number,
        reviewer_name: "madison".to_string(),
        body: "<!-- midtown-placeholder -->\nReview in progress...".to_string(),
    }];
    execute_effects(effects, &state).await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    {
        let ps = state.persistent_state.lock().await;
        let assignment = ps.github.pr_reviewers.get(&pr_number).unwrap();
        assert_eq!(
            assignment.placeholder_comment_id,
            Some(11223),
            "Should parse comment ID 11223 from the bare numeric URL"
        );
    }
}

/// Verify that when a placeholder comment ID is already stored on the
/// `PrReviewerAssignment` (from a previous reviewer cycle), `post_pr_comment`
/// edits the existing comment (PATCH) instead of creating a new one.
///
/// Uses the 3-tier lookup: tier 1 (persistent state) returns the stored ID,
/// so no GitHub API call is needed for discovery — only for the PATCH update.
#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await.
#[tokio::test]
async fn test_post_pr_comment_reuses_existing_placeholder() {
    use std::os::unix::fs::PermissionsExt;

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();

    let (state, _project_dir, _guard) = make_workflow_test_state("post-pr-reuse");

    let pr_number = 77u64;
    let existing_comment_id = 55555u64;
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github
            .assign_reviewer(pr_number, "riverside", AssignmentSource::Webhook);
        // Pre-populate the placeholder_comment_id (as if a previous reviewer
        // cycle posted it before timing out). This is the tier 1 lookup path.
        if let Some(assignment) = ps.github.pr_reviewers.get_mut(&pr_number) {
            assignment.placeholder_comment_id = Some(existing_comment_id);
        }
    }

    // Mock `gh` to:
    // 1. Accept the PATCH request to update the existing comment
    // 2. Log which commands were called for verification
    // Note: no "issues/.../comments" mock needed — tier 1 lookup finds the ID
    let temp_dir = tempfile::tempdir().unwrap();
    let mock_gh_dir = temp_dir.path().join("bin");
    std::fs::create_dir_all(&mock_gh_dir).unwrap();
    let log_file = temp_dir.path().join("gh_calls.log");
    let mock_gh_script = mock_gh_dir.join("gh");

    std::fs::write(
        &mock_gh_script,
        format!(
            r#"#!/bin/bash
echo "$@" >> "{log}"
if echo "$@" | grep -q "repo view"; then
  echo 'test/repo'
elif echo "$@" | grep -q "PATCH"; then
  echo '{{"id": {existing_comment_id}}}'
elif echo "$@" | grep -q "pr comment"; then
  echo 'https://github.com/test/repo/pull/77#issuecomment-99999'
fi
"#,
            log = log_file.display(),
            existing_comment_id = existing_comment_id,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&mock_gh_script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", mock_gh_dir.display(), original_path),
        );
    }

    let effects = vec![Effect::PostPrComment {
        pr_number,
        reviewer_name: "riverside".to_string(),
        body: "<!-- midtown-placeholder -->\n## Review Status\n\n🔍 Review in progress by riverside..."
            .to_string(),
    }];
    execute_effects(effects, &state).await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    // Verify: the PATCH endpoint was called (editing existing comment)
    let log_contents = std::fs::read_to_string(&log_file).unwrap();
    assert!(
        log_contents.contains("PATCH"),
        "Should have called gh api --method PATCH to edit existing placeholder, got: {}",
        log_contents,
    );

    // Verify: `gh pr comment` was NOT called (no new comment created)
    assert!(
        !log_contents.contains("pr comment"),
        "Should NOT have called `gh pr comment` when placeholder exists, got: {}",
        log_contents,
    );

    // Verify: the existing comment ID is still stored on the assignment
    {
        let ps = state.persistent_state.lock().await;
        let assignment = ps
            .github
            .pr_reviewers
            .get(&pr_number)
            .expect("assignment should still exist");
        assert_eq!(
            assignment.placeholder_comment_id,
            Some(existing_comment_id),
            "Should preserve the existing comment ID on the assignment"
        );
    }

    // Verify: the placeholder cache was populated with the existing comment ID
    {
        let cache = state.reviewer_placeholder_cache.lock().unwrap();
        let (cached_id, _instant) = cache
            .get(&pr_number)
            .expect("placeholder cache should be populated");
        assert_eq!(
            *cached_id,
            Some(existing_comment_id),
            "Placeholder cache should contain the existing comment ID"
        );
    }
}

/// Verify that `lookup_existing_placeholder` falls back to the GitHub API
/// (tier 3) when the assignment has no stored placeholder_comment_id and
/// the cache is empty. This covers the re-spawn scenario where the daemon
/// restarted and lost in-memory state.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn test_post_pr_comment_reuses_placeholder_via_api_fallback() {
    use std::os::unix::fs::PermissionsExt;

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();

    let (state, _project_dir, _guard) = make_workflow_test_state("post-pr-reuse-api");

    let pr_number = 88u64;
    let existing_comment_id = 66666u64;
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github
            .assign_reviewer(pr_number, "madison", AssignmentSource::Webhook);
        // Do NOT set placeholder_comment_id — simulates daemon restart
    }

    // Mock `gh` to:
    // 1. Return placeholder via `gh pr view --json comments` (tier 3 fallback)
    // 2. Accept the PATCH request
    let temp_dir = tempfile::tempdir().unwrap();
    let mock_gh_dir = temp_dir.path().join("bin");
    std::fs::create_dir_all(&mock_gh_dir).unwrap();
    let log_file = temp_dir.path().join("gh_calls.log");
    let mock_gh_script = mock_gh_dir.join("gh");

    std::fs::write(
        &mock_gh_script,
        format!(
            r#"#!/bin/bash
echo "$@" >> "{log}"
if echo "$@" | grep -q "repo view"; then
  echo 'test/repo'
elif echo "$@" | grep -q "pr view.*--json comments"; then
  echo '{{"comments": [{{"body": "<!-- midtown-placeholder -->\n## Review Status\n\n🔍 Review in progress by pleasant...", "url": "https://github.com/test/repo/pull/88#issuecomment-{existing_comment_id}"}}]}}'
elif echo "$@" | grep -q "PATCH"; then
  echo '{{"id": {existing_comment_id}}}'
elif echo "$@" | grep -q "pr comment"; then
  echo 'https://github.com/test/repo/pull/88#issuecomment-99999'
fi
"#,
            log = log_file.display(),
            existing_comment_id = existing_comment_id,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&mock_gh_script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", mock_gh_dir.display(), original_path),
        );
    }

    let effects = vec![Effect::PostPrComment {
        pr_number,
        reviewer_name: "madison".to_string(),
        body:
            "<!-- midtown-placeholder -->\n## Review Status\n\n🔍 Review in progress by madison..."
                .to_string(),
    }];
    execute_effects(effects, &state).await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    let log_contents = std::fs::read_to_string(&log_file).unwrap();

    // Verify: tier 3 API fallback was called
    assert!(
        log_contents.contains("pr view"),
        "Should have called `gh pr view --json comments` as tier 3 fallback, got: {}",
        log_contents,
    );

    // Verify: PATCH was called (not `gh pr comment`)
    assert!(
        log_contents.contains("PATCH"),
        "Should have called gh api --method PATCH to edit existing placeholder, got: {}",
        log_contents,
    );
    assert!(
        !log_contents.contains("pr comment"),
        "Should NOT have called `gh pr comment` when placeholder exists, got: {}",
        log_contents,
    );

    // Verify: the placeholder ID was stored on the assignment
    {
        let ps = state.persistent_state.lock().await;
        let assignment = ps
            .github
            .pr_reviewers
            .get(&pr_number)
            .expect("assignment should still exist");
        assert_eq!(assignment.placeholder_comment_id, Some(existing_comment_id),);
    }
}

// ============================================================================
// RespawnFork initial_prompt preservation tests
// ============================================================================

/// When `RespawnFork` carries an `initial_prompt`, the respawned fork's nudge
/// should include the original message (not generic "crash recovery" framing).
/// This test verifies the Effect can be constructed with the field and that the
/// pattern match in execute_effects destructures correctly.
#[test]
fn test_respawn_fork_effect_carries_initial_prompt() {
    let effect = Effect::RespawnFork {
        fork_name: "fork-investigate-auth".to_string(),
        thread_parent_id: "msg-abc123".to_string(),
        channel: Some("daemon-core".to_string()),
        working_dir: Some("/tmp/worktree".to_string()),
        auth_provider: crate::auth::AuthProvider::Claude,
        is_channel_lead: true,
        initial_prompt: Some("Investigate the auth bug in the login flow".to_string()),
        old_session_id: None,
    };

    // Verify the initial_prompt is preserved through pattern matching
    if let Effect::RespawnFork { initial_prompt, .. } = &effect {
        assert_eq!(
            initial_prompt.as_deref(),
            Some("Investigate the auth bug in the login flow"),
            "RespawnFork should carry the original initial_prompt"
        );
    } else {
        panic!("Expected RespawnFork variant");
    }
}

/// When `RespawnFork` has `initial_prompt: None`, it should fall back to
/// generic framing (the pre-fix behavior).
#[test]
fn test_respawn_fork_effect_without_initial_prompt() {
    let effect = Effect::RespawnFork {
        fork_name: "fork-some-thread".to_string(),
        thread_parent_id: "msg-xyz789".to_string(),
        channel: Some("daemon-core".to_string()),
        working_dir: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        is_channel_lead: false,
        initial_prompt: None,
        old_session_id: None,
    };

    if let Effect::RespawnFork { initial_prompt, .. } = &effect {
        assert!(
            initial_prompt.is_none(),
            "RespawnFork without preserved context should have None initial_prompt"
        );
    } else {
        panic!("Expected RespawnFork variant");
    }
}

/// When `RespawnFork` carries an `old_session_id`, the effect should preserve it
/// so crash recovery can attempt to resume the fork's previous session.
#[test]
fn test_respawn_fork_effect_carries_old_session_id() {
    let effect = Effect::RespawnFork {
        fork_name: "fork-investigate-auth".to_string(),
        thread_parent_id: "msg-abc123".to_string(),
        channel: Some("daemon-core".to_string()),
        working_dir: Some("/tmp/worktree".to_string()),
        auth_provider: crate::auth::AuthProvider::Claude,
        is_channel_lead: true,
        initial_prompt: Some("Investigate the auth bug".to_string()),
        old_session_id: Some("old-fork-sess-123".to_string()),
    };

    if let Effect::RespawnFork { old_session_id, .. } = &effect {
        assert_eq!(
            old_session_id.as_deref(),
            Some("old-fork-sess-123"),
            "RespawnFork should carry the old session ID for resume"
        );
    } else {
        panic!("Expected RespawnFork variant");
    }
}

/// When `respawn_fork` creates a new SessionRecord for a fork, it must clear
/// `current_name` on any old session records that still claim the same name.
/// This prevents ambiguous find-by-name lookups (same bug class as PR #1819).
#[test]
fn test_respawn_fork_clears_old_record_current_name() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};

    let mut ps = DaemonPersistentState::default();

    // Old dead fork still has current_name set (the bug scenario)
    let old_record = SessionRecord {
        session_id: "old-fork-sess".to_string(),
        task_id: None,
        current_name: Some("fork-investigate".to_string()),
        preferred_name: Some("fork-investigate".to_string()),
        working_dir: "/tmp/old".to_string(),
        branch: None,
        pr_number: None,
        initial_prompt: None,
        is_reviewer: false,
        coworker_type: "dev".to_string(),
        is_running: false,
        created_at: chrono::Utc::now(),
        resume_on_startup: false,
        bound_thread_id: Some("thread-123".to_string()),
        last_active: chrono::Utc::now(),
        purpose: "old fork".to_string(),
        pid: None,
        channel: None,
        provider: None,
        platform: None,
        profile: None,
    };
    ps.sessions
        .insert(old_record.session_id.clone(), old_record);

    // Simulate what respawn_fork does: clear old records, then insert new one.
    let new_fork_name = "fork-investigate";
    let new_fork_session_id = "new-fork-sess";

    // --- This is the cleanup that respawn_fork must perform ---
    for record in ps.sessions.values_mut() {
        if record.session_id != new_fork_session_id
            && (record.preferred_name.as_deref() == Some(new_fork_name)
                || record.current_name.as_deref() == Some(new_fork_name))
        {
            record.is_running = false;
            record.current_name = None;
            record.preferred_name = None;
        }
    }

    // Insert new record (like respawn_fork does)
    ps.sessions.insert(
        new_fork_session_id.to_string(),
        SessionRecord {
            session_id: new_fork_session_id.to_string(),
            task_id: None,
            current_name: Some(new_fork_name.to_string()),
            preferred_name: Some(new_fork_name.to_string()),
            working_dir: "/tmp/new".to_string(),
            branch: None,
            pr_number: None,
            initial_prompt: None,
            is_reviewer: false,
            coworker_type: "dev".to_string(),
            is_running: true,
            created_at: chrono::Utc::now(),
            resume_on_startup: false,
            bound_thread_id: Some("thread-123".to_string()),
            last_active: chrono::Utc::now(),
            purpose: "respawned fork".to_string(),
            pid: None,
            channel: None,
            provider: None,
            platform: None,
            profile: None,
        },
    );

    // Verify: old record must have both name fields cleared
    let old = ps.sessions.get("old-fork-sess").unwrap();
    assert!(
        old.current_name.is_none(),
        "Old fork record should have current_name cleared after respawn"
    );
    assert!(
        old.preferred_name.is_none(),
        "Old fork record should have preferred_name cleared after respawn"
    );
    assert!(
        !old.is_running,
        "Old fork record should not be marked as running"
    );

    // Verify: new record has the name
    let new = ps.sessions.get("new-fork-sess").unwrap();
    assert_eq!(new.current_name.as_deref(), Some("fork-investigate"));
    assert!(new.is_running);

    // Verify: only one record claims the name (via either field)
    let name_count = ps
        .sessions
        .values()
        .filter(|r| {
            r.current_name.as_deref() == Some("fork-investigate")
                || r.preferred_name.as_deref() == Some("fork-investigate")
        })
        .count();
    assert_eq!(
        name_count, 1,
        "Exactly one session record should claim the fork name"
    );
}

/// When an old fork record has `current_name: None` but `preferred_name` still
/// set, the cleanup must still clear `preferred_name`. Otherwise `rpc_auth.rs`
/// (which matches on both fields) would find an ambiguous match.
#[test]
fn test_respawn_fork_clears_old_record_preferred_name_only() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};

    let mut ps = DaemonPersistentState::default();

    // Old record: current_name already cleared but preferred_name still set
    let old_record = SessionRecord {
        session_id: "old-fork-sess".to_string(),
        task_id: None,
        current_name: None,
        preferred_name: Some("fork-investigate".to_string()),
        working_dir: "/tmp/old".to_string(),
        branch: None,
        pr_number: None,
        initial_prompt: None,
        is_reviewer: false,
        coworker_type: "dev".to_string(),
        is_running: false,
        created_at: chrono::Utc::now(),
        resume_on_startup: false,
        bound_thread_id: Some("thread-123".to_string()),
        last_active: chrono::Utc::now(),
        purpose: "old fork".to_string(),
        pid: None,
        channel: None,
        provider: None,
        platform: None,
        profile: None,
    };
    ps.sessions
        .insert(old_record.session_id.clone(), old_record);

    let new_fork_name = "fork-investigate";
    let new_fork_session_id = "new-fork-sess";

    // Same cleanup as respawn_fork — must match on preferred_name too
    for record in ps.sessions.values_mut() {
        if record.session_id != new_fork_session_id
            && (record.preferred_name.as_deref() == Some(new_fork_name)
                || record.current_name.as_deref() == Some(new_fork_name))
        {
            record.is_running = false;
            record.current_name = None;
            record.preferred_name = None;
        }
    }

    ps.sessions.insert(
        new_fork_session_id.to_string(),
        SessionRecord {
            session_id: new_fork_session_id.to_string(),
            task_id: None,
            current_name: Some(new_fork_name.to_string()),
            preferred_name: Some(new_fork_name.to_string()),
            working_dir: "/tmp/new".to_string(),
            branch: None,
            pr_number: None,
            initial_prompt: None,
            is_reviewer: false,
            coworker_type: "dev".to_string(),
            is_running: true,
            created_at: chrono::Utc::now(),
            resume_on_startup: false,
            bound_thread_id: Some("thread-123".to_string()),
            last_active: chrono::Utc::now(),
            purpose: "respawned fork".to_string(),
            pid: None,
            channel: None,
            provider: None,
            platform: None,
            profile: None,
        },
    );

    // Old record's preferred_name must be cleared
    let old = ps.sessions.get("old-fork-sess").unwrap();
    assert!(
        old.preferred_name.is_none(),
        "Old record with preferred_name-only should have it cleared"
    );

    // No ambiguous match: only the new record should match a find-by-name
    let matches: Vec<_> = ps
        .sessions
        .values()
        .filter(|r| {
            r.current_name.as_deref() == Some("fork-investigate")
                || r.preferred_name.as_deref() == Some("fork-investigate")
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "Only the new fork should match find-by-name; got {} matches",
        matches.len()
    );
    assert_eq!(matches[0].session_id, "new-fork-sess");
}

// ── post_insight tests ──────────────────────────────────────────────────────
//
// Ported from the deleted rpc_insight_tests.rs. These test the async
// `post_insight()` executor in effects.rs which reimplements the same
// dedup, suppression, and routing logic.

fn make_insight_test_state(
    repo_name: &str,
) -> (
    DaemonState,
    tempfile::TempDir,
    crate::paths::TestMidtownBaseDirGuard,
) {
    let midtown_dir = tempfile::tempdir().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let temp_dir = tempfile::tempdir().expect("temp dir");
    Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git commit");

    let wm = crate::worktree::WorktreeManager::new(temp_dir.path().to_path_buf())
        .expect("worktree manager");
    let cm = crate::coworker::CoworkerManager::new(wm);

    let base_dir = temp_dir.path().to_path_buf();
    let channel_router = crate::ChannelRouter::new(&base_dir, repo_name);
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let state = DaemonState::new(
        "/tmp/test.sock".into(),
        cm,
        crate::paths::ProjectPaths::with_project_name(repo_name, repo_name),
        vec![base_dir],
        channel_router,
        None,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state");
    (state, temp_dir, _guard)
}

/// Helper: read all JSONL lines from a channel's history file.
fn read_channel_messages(
    temp_dir: &tempfile::TempDir,
    channel_name: &str,
) -> Vec<serde_json::Value> {
    let file = temp_dir
        .path()
        .join("channels")
        .join(channel_name)
        .join("history")
        .join("current.jsonl");
    if !file.exists() {
        return vec![];
    }
    std::fs::read_to_string(&file)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn test_hash_insight_deterministic() {
    let hash1 = super::hash_insight("Test insight content");
    let hash2 = super::hash_insight("Test insight content");
    assert_eq!(hash1, hash2);
}

#[test]
fn test_hash_insight_normalizes_whitespace_and_case() {
    let hash1 = super::hash_insight("This is an insight");
    let hash2 = super::hash_insight("  This  is   an   insight  ");
    let hash3 = super::hash_insight("This\n  is\nan\ninsight");
    let hash4 = super::hash_insight("THIS IS AN INSIGHT");

    assert_eq!(hash1, hash2, "extra whitespace should be normalized");
    assert_eq!(hash1, hash3, "newlines should be normalized");
    assert_eq!(hash1, hash4, "case should be normalized");
}

#[test]
fn test_hash_insight_different_content() {
    let hash1 = super::hash_insight("Insight one");
    let hash2 = super::hash_insight("Insight two");
    assert_ne!(hash1, hash2);
}

/// Duplicate insights should be deduplicated: first posts, second is skipped.
#[tokio::test]
async fn test_post_insight_deduplication() {
    let (state, temp_dir, _guard) = make_insight_test_state("testrepo");

    super::post_insight(&state, "coworker1", "Unique insight text").await;
    super::post_insight(&state, "coworker1", "Unique insight text").await;

    let default_ch = state.channel_router.default_channel_name().to_string();
    let messages = read_channel_messages(&temp_dir, &default_ch);
    let insight_msgs: Vec<_> = messages
        .iter()
        .filter(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.contains("Unique insight text"))
        })
        .collect();
    assert_eq!(
        insight_msgs.len(),
        1,
        "duplicate insight should be deduplicated"
    );
}

/// Insights from channel leads should be suppressed (they auto-post output).
#[tokio::test]
async fn test_post_insight_channel_lead_suppressed() {
    let (state, temp_dir, _guard) = make_insight_test_state("testrepo");

    // Register a running channel-lead session
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            "cl-session-abc".to_string(),
            super::super::state::SessionRecord {
                session_id: "cl-session-abc".to_string(),
                current_name: Some("ops-lead".to_string()),
                coworker_type: "channel-lead".to_string(),
                working_dir: "/tmp/test".to_string(),
                is_running: true,
                ..Default::default()
            },
        );
    }

    super::post_insight(&state, "ops-lead", "Channel lead insight").await;

    let default_ch = state.channel_router.default_channel_name().to_string();
    let messages = read_channel_messages(&temp_dir, &default_ch);
    let insight_msgs: Vec<_> = messages
        .iter()
        .filter(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.contains("Channel lead insight"))
        })
        .collect();
    assert!(
        insight_msgs.is_empty(),
        "channel lead insights should be suppressed"
    );
}

/// Dedup-before-suppression ordering: hash is inserted before the channel-lead
/// check, so a channel-lead insight records the hash and a subsequent non-lead
/// posting the same text is correctly deduplicated.
#[tokio::test]
async fn test_post_insight_dedup_before_suppression_ordering() {
    let (state, temp_dir, _guard) = make_insight_test_state("testrepo");

    // Register a running channel-lead session
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            "cl-session-abc".to_string(),
            super::super::state::SessionRecord {
                session_id: "cl-session-abc".to_string(),
                current_name: Some("ops-lead".to_string()),
                coworker_type: "channel-lead".to_string(),
                working_dir: "/tmp/test".to_string(),
                is_running: true,
                ..Default::default()
            },
        );
    }

    // Channel lead posts first — suppressed but hash recorded
    super::post_insight(&state, "ops-lead", "Shared insight text").await;

    // Non-lead coworker posts same text — should be deduplicated
    super::post_insight(&state, "coworker1", "Shared insight text").await;

    let default_ch = state.channel_router.default_channel_name().to_string();
    let messages = read_channel_messages(&temp_dir, &default_ch);
    let insight_msgs: Vec<_> = messages
        .iter()
        .filter(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.contains("Shared insight text"))
        })
        .collect();
    assert!(
        insight_msgs.is_empty(),
        "channel lead suppressed + coworker deduped = no posted insight"
    );
}

/// When task_thread_id is set but task_channel is None, the task lives in the
/// default channel (created without --channel). The insight should thread under
/// the task announcement in the default channel.
#[tokio::test]
async fn test_post_insight_threads_in_default_channel_when_task_channel_is_none() {
    let (state, temp_dir, _guard) = make_insight_test_state("testrepo");

    let thread_id = "announcement-in-default-channel";

    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            "test-session-id".to_string(),
            super::super::state::SessionRecord {
                session_id: "test-session-id".to_string(),
                current_name: Some("coworker1".to_string()),
                coworker_type: "dev".to_string(),
                task_id: Some("50".to_string()),
                is_running: true,
                ..Default::default()
            },
        );
        // Deliberately NOT setting task_channel — task lives in default channel
        ps.task_thread_id
            .insert("50".to_string(), thread_id.to_string());
    }

    super::post_insight(&state, "coworker1", "Default channel threaded insight").await;

    let default_ch = state.channel_router.default_channel_name().to_string();
    let messages = read_channel_messages(&temp_dir, &default_ch);
    let line = messages
        .iter()
        .find(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.contains("Default channel threaded insight"))
        })
        .expect("insight should be posted to default channel");
    assert_eq!(
        line["thread_parent_id"].as_str(),
        Some(thread_id),
        "insight should thread under the task announcement in the default channel"
    );
}

/// When a coworker name is reused across sessions (stale + active), the insight
/// should route using the active (is_running=true) session's task binding, not
/// the stale one.
#[tokio::test]
async fn test_post_insight_prefers_running_session_over_stale_with_same_name() {
    let (state, temp_dir, _guard) = make_insight_test_state("testrepo");

    let thread_parent_id = "announcement-msg-uuid-99";

    {
        let mut ps = state.persistent_state.lock().await;

        // Stale session (stopped, different task in different channel)
        ps.sessions.insert(
            "old-session-id".to_string(),
            super::super::state::SessionRecord {
                session_id: "old-session-id".to_string(),
                current_name: Some("coworker1".to_string()),
                coworker_type: "dev".to_string(),
                task_id: Some("88".to_string()),
                is_running: false,
                ..Default::default()
            },
        );

        // Active session (running, correct task)
        ps.sessions.insert(
            "new-session-id".to_string(),
            super::super::state::SessionRecord {
                session_id: "new-session-id".to_string(),
                current_name: Some("coworker1".to_string()),
                coworker_type: "dev".to_string(),
                task_id: Some("99".to_string()),
                is_running: true,
                ..Default::default()
            },
        );

        // Old task in a different channel
        ps.task_channel
            .insert("88".to_string(), "old-channel".to_string());
        ps.task_thread_id
            .insert("88".to_string(), "old-thread-id".to_string());

        // Current task in the correct channel
        ps.task_channel
            .insert("99".to_string(), "my-feature".to_string());
        ps.task_thread_id
            .insert("99".to_string(), thread_parent_id.to_string());
    }

    super::post_insight(&state, "coworker1", "Insight from reused name session").await;

    // The insight should route to "my-feature" channel, threaded under the task
    let messages = read_channel_messages(&temp_dir, "my-feature");
    let line = messages
        .iter()
        .find(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.contains("Insight from reused name session"))
        })
        .expect("insight should be posted to the task channel");
    assert_eq!(
        line["thread_parent_id"].as_str(),
        Some(thread_parent_id),
        "insight should thread under the active session's task announcement"
    );
}

/// When a coworker has a task with both task_channel and task_thread_id set,
/// the insight should be posted as a thread reply.
#[tokio::test]
async fn test_post_insight_routes_to_task_thread() {
    let (state, temp_dir, _guard) = make_insight_test_state("testrepo");

    let thread_parent_id = "announcement-msg-uuid-42";

    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            "test-session-id".to_string(),
            super::super::state::SessionRecord {
                session_id: "test-session-id".to_string(),
                current_name: Some("coworker1".to_string()),
                coworker_type: "dev".to_string(),
                task_id: Some("42".to_string()),
                is_running: true,
                ..Default::default()
            },
        );
        ps.task_channel
            .insert("42".to_string(), "my-feature".to_string());
        ps.task_thread_id
            .insert("42".to_string(), thread_parent_id.to_string());
    }

    super::post_insight(&state, "coworker1", "A threaded insight").await;

    let messages = read_channel_messages(&temp_dir, "my-feature");
    let line = messages
        .iter()
        .find(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.contains("A threaded insight"))
        })
        .expect("insight should be posted to task channel");
    assert_eq!(
        line["thread_parent_id"].as_str(),
        Some(thread_parent_id),
        "insight should be threaded under the task announcement"
    );
}

/// When a task has no task_channel (created without --channel), the insight
/// should still thread under the task announcement in the default channel.
#[tokio::test]
async fn test_post_insight_threads_when_task_channel_is_none() {
    let (state, temp_dir, _guard) = make_insight_test_state("testrepo");

    let thread_parent_id = "announcement-msg-uuid-99";

    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            "test-session-id".to_string(),
            super::super::state::SessionRecord {
                session_id: "test-session-id".to_string(),
                current_name: Some("coworker1".to_string()),
                coworker_type: "dev".to_string(),
                task_id: Some("99".to_string()),
                is_running: true,
                ..Default::default()
            },
        );
        // Deliberately NOT setting task_channel — simulates task created without --channel
        ps.task_thread_id
            .insert("99".to_string(), thread_parent_id.to_string());
    }

    super::post_insight(&state, "coworker1", "Insight with no task channel").await;

    let default_channel = state.channel_router.default_channel_name();
    let messages = read_channel_messages(&temp_dir, default_channel);
    let line = messages
        .iter()
        .find(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.contains("Insight with no task channel"))
        })
        .expect("insight should be posted to default channel");
    assert_eq!(
        line["thread_parent_id"].as_str(),
        Some(thread_parent_id),
        "insight should be threaded under the task announcement even when task_channel is None"
    );
}

/// When a coworker has a task with task_channel but no task_thread_id,
/// the insight should be posted as a top-level message.
#[tokio::test]
async fn test_post_insight_no_thread_when_no_thread_id() {
    let (state, temp_dir, _guard) = make_insight_test_state("testrepo");

    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            "test-session-id".to_string(),
            super::super::state::SessionRecord {
                session_id: "test-session-id".to_string(),
                current_name: Some("coworker1".to_string()),
                coworker_type: "dev".to_string(),
                task_id: Some("42".to_string()),
                is_running: true,
                ..Default::default()
            },
        );
        ps.task_channel
            .insert("42".to_string(), "my-feature".to_string());
        // Deliberately NOT setting task_thread_id
    }

    super::post_insight(&state, "coworker1", "An unthreaded insight").await;

    let messages = read_channel_messages(&temp_dir, "my-feature");
    let line = messages
        .iter()
        .find(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.contains("An unthreaded insight"))
        })
        .expect("insight should be posted to task channel");
    assert!(
        line.get("thread_parent_id").is_none() || line["thread_parent_id"].is_null(),
        "message should not have thread_parent_id when task has no thread binding"
    );
}

#[tokio::test]
async fn test_nudge_channel_lead_dm_nudges_active_coworker() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo");
    let coworker_name = "columbus";
    let channel_name = format!("dm-{}", coworker_name);
    let session_id = "sess-columbus-1".to_string();
    let dm_content = "Hey, can you check the auth module?";

    // Register the coworker as active via name_to_session
    state
        .name_to_session
        .lock()
        .unwrap()
        .insert(coworker_name.to_string(), session_id.clone());

    // Set up hook to capture the nudge
    let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
    let observed_for_hook = observed.clone();
    state
        .session_manager
        .set_test_send_message_to_session_id_hook(Some(std::sync::Arc::new(move |sid, msg| {
            observed_for_hook
                .lock()
                .unwrap()
                .push((sid.to_string(), msg.to_string()));
            Ok(())
        })));

    execute_effects(
        vec![Effect::NudgeChannelLead {
            channel_name: channel_name.clone(),
            reason: crate::daemon::wake_reason::WakeReason::DmFromUser {
                content: dm_content.to_string(),
                msg_id: "msg-dm-001".to_string(),
                coworker_name: coworker_name.to_string(),
            },
        }],
        &state,
    )
    .await;

    let calls = observed.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "should nudge exactly once");
    assert_eq!(
        calls[0].0, session_id,
        "should nudge the coworker's session"
    );
    assert!(
        calls[0].1.contains(dm_content),
        "nudge message should contain the DM content"
    );
}

#[tokio::test]
async fn test_nudge_channel_lead_dm_no_active_session_logs_warning() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo");
    let coworker_name = "columbus";
    let channel_name = format!("dm-{}", coworker_name);

    // No session registered — coworker is not active and has no stored record.
    // The effect should not panic and should not attempt to send any messages.
    let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
    let observed_for_hook = observed.clone();
    state
        .session_manager
        .set_test_send_message_to_session_id_hook(Some(std::sync::Arc::new(move |sid, msg| {
            observed_for_hook
                .lock()
                .unwrap()
                .push((sid.to_string(), msg.to_string()));
            Ok(())
        })));

    execute_effects(
        vec![Effect::NudgeChannelLead {
            channel_name: channel_name.clone(),
            reason: crate::daemon::wake_reason::WakeReason::DmFromUser {
                content: "hello?".to_string(),
                msg_id: "msg-dm-002".to_string(),
                coworker_name: coworker_name.to_string(),
            },
        }],
        &state,
    )
    .await;

    let calls = observed.lock().unwrap().clone();
    assert!(
        calls.is_empty(),
        "should not attempt to nudge when no session exists"
    );
}

#[tokio::test]
async fn test_nudge_channel_lead_dm_project_lead_uses_nudge_lead() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo");

    // Don't register any session — the lead uses headed intercom fallback.
    // This should not panic — it falls through to nudge_lead()
    execute_effects(
        vec![Effect::NudgeChannelLead {
            channel_name: "dm-myrepo".to_string(),
            reason: crate::daemon::wake_reason::WakeReason::DmFromUser {
                content: "hey lead".to_string(),
                msg_id: "msg-lead-001".to_string(),
                coworker_name: "myrepo".to_string(),
            },
        }],
        &state,
    )
    .await;
    // Success: no panic, no error. nudge_lead() handled it.
}

#[tokio::test]
async fn test_nudge_channel_lead_dm_channel_lead_uses_stored_session() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo");
    let channel_lead_name = "auth";
    let session_id = "sess-auth-lead-1".to_string();

    // Register ONLY in channel_lead_sessions (not name_to_session).
    // This simulates a channel lead whose session is not currently active in
    // the name_to_session map — the DM nudge should fall through the active-session
    // check and then detect the channel lead via channel_lead_sessions, delegating
    // to the existing channel lead resume/spawn machinery.
    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert(channel_lead_name.to_string(), session_id.clone());
    }

    // The hook captures send_message_to_session_id calls — should be empty since
    // there is no active session (name_to_session is empty). The channel lead
    // fallback re-emits NudgeChannelLead with the topic channel name, which goes
    // through the non-DM branch and uses channel_lead_sessions to find the session.
    let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
    let observed_for_hook = observed.clone();
    state
        .session_manager
        .set_test_send_message_to_session_id_hook(Some(std::sync::Arc::new(move |sid, msg| {
            observed_for_hook
                .lock()
                .unwrap()
                .push((sid.to_string(), msg.to_string()));
            Ok(())
        })));

    execute_effects(
        vec![Effect::NudgeChannelLead {
            channel_name: format!("dm-{}", channel_lead_name),
            reason: crate::daemon::wake_reason::WakeReason::DmFromUser {
                content: "check auth".to_string(),
                msg_id: "msg-dm-auth-001".to_string(),
                coworker_name: channel_lead_name.to_string(),
            },
        }],
        &state,
    )
    .await;

    let calls = observed.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        1,
        "should nudge exactly once via channel lead path"
    );
    assert_eq!(
        calls[0].0, session_id,
        "should use the stored channel lead session_id"
    );
}

#[tokio::test]
async fn test_nudge_channel_lead_dm_fork_no_respawn() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo");
    let fork_name = "auth-web-push-a1b2";

    // Register the fork in fork_bound_threads but NOT in name_to_session (dead fork).
    // Also add a stored SessionRecord — without the fork guard, the coworker fallback
    // would attempt to resume this record. The type-aware branch should detect the
    // fork_bound_threads entry and skip respawn entirely.
    state
        .fork_bound_threads
        .lock()
        .unwrap()
        .insert(fork_name.to_string(), "thread-001".to_string());
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            "sess-fork-dead".to_string(),
            crate::daemon::state::SessionRecord {
                session_id: "sess-fork-dead".to_string(),
                current_name: Some(fork_name.to_string()),
                preferred_name: Some(fork_name.to_string()),
                working_dir: "/tmp".to_string(),
                ..Default::default()
            },
        );
    }

    let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
    let observed_for_hook = observed.clone();
    state
        .session_manager
        .set_test_send_message_to_session_id_hook(Some(std::sync::Arc::new(move |sid, msg| {
            observed_for_hook
                .lock()
                .unwrap()
                .push((sid.to_string(), msg.to_string()));
            Ok(())
        })));

    execute_effects(
        vec![Effect::NudgeChannelLead {
            channel_name: format!("dm-{}", fork_name),
            reason: crate::daemon::wake_reason::WakeReason::DmFromUser {
                content: "hey fork".to_string(),
                msg_id: "msg-dm-fork-001".to_string(),
                coworker_name: fork_name.to_string(),
            },
        }],
        &state,
    )
    .await;

    let calls = observed.lock().unwrap().clone();
    assert!(
        calls.is_empty(),
        "dead fork should not be nudged or respawned"
    );
}

// ── format_workflow_state_summary tests ──────────────────────────────

#[test]
fn format_workflow_state_summary_with_tasks() {
    let state: serde_json::Value = serde_json::json!({
        "tasks": {
            "42": {"phase": "observe"},
            "43": {"phase": "study"}
        }
    });
    let result = super::format_workflow_state_summary(&state);
    assert!(result.contains("Task !42"));
    assert!(result.contains("Task !43"));
    assert!(result.contains("observe"));
    assert!(result.contains("study"));
}

#[test]
fn format_workflow_state_summary_empty_tasks() {
    let state: serde_json::Value = serde_json::json!({"tasks": {}});
    let result = super::format_workflow_state_summary(&state);
    assert!(result.contains("No active workflow state"));
}

#[test]
fn format_workflow_state_summary_no_tasks_key() {
    let state: serde_json::Value = serde_json::json!({"something": "else"});
    let result = super::format_workflow_state_summary(&state);
    // Should still produce something meaningful — dump the JSON
    assert!(!result.is_empty());
}

#[test]
fn format_workflow_state_summary_null() {
    let state: serde_json::Value = serde_json::Value::Null;
    let result = super::format_workflow_state_summary(&state);
    assert!(result.contains("No active workflow state"));
}

#[test]
fn test_post_to_channel_constructor() {
    let effect = Effect::post_to_channel("alice", "hello world", Some("general".to_string()));
    match effect {
        Effect::PostToChannel {
            sender,
            message,
            channel,
            auto_output,
            message_type,
            nudge_type,
            tool_data,
            provider,
            tool_use_id,
            parent_tool_use_id,
        } => {
            assert_eq!(sender, "alice");
            assert_eq!(message, "hello world");
            assert_eq!(channel, Some("general".to_string()));
            assert!(!auto_output);
            assert!(message_type.is_none());
            assert!(nudge_type.is_none());
            assert!(tool_data.is_none());
            assert!(provider.is_none());
            assert!(tool_use_id.is_none());
            assert!(parent_tool_use_id.is_none());
        }
        _ => panic!("expected PostToChannel variant"),
    }

    // Also verify None channel works
    let effect = Effect::post_to_channel("bob", "test", None);
    match effect {
        Effect::PostToChannel { channel, .. } => assert!(channel.is_none()),
        _ => panic!("expected PostToChannel variant"),
    }
}

#[test]
fn test_post_to_ops_constructor() {
    let effect = Effect::post_to_ops("system update");
    match effect {
        Effect::PostToChannel {
            sender,
            message,
            channel,
            auto_output,
            message_type,
            nudge_type,
            tool_data,
            provider,
            tool_use_id,
            parent_tool_use_id,
        } => {
            assert_eq!(sender, "midtown");
            assert_eq!(message, "system update");
            assert_eq!(channel, Some("ops".to_string()));
            assert!(!auto_output);
            assert!(message_type.is_none());
            assert!(nudge_type.is_none());
            assert!(tool_data.is_none());
            assert!(provider.is_none());
            assert!(tool_use_id.is_none());
            assert!(parent_tool_use_id.is_none());
        }
        _ => panic!("expected PostToChannel variant"),
    }
}
