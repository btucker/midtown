use super::*;
use crate::daemon::trackers::PrIssueType;
use std::process::Command;

/// Helper: create a reviewer task in TaskStore so post_pr_comment can write to it.
fn create_reviewer_task(state: &DaemonState, task_id: &str, pr_number: u64) {
    let task = crate::task_store::Task {
        id: task_id.to_string(),
        subject: "Review PR".to_string(),
        status: crate::task_store::TaskStatus::InProgress,
        pr: Some(pr_number),
        agent_type: "midtown-code-reviewer".to_string(),
        agent_name: "park".to_string(),
        ..Default::default()
    };
    let _ = state.task_store.save(&task);
}

fn mk_session_record(
    session_id: &str,
    task_id: Option<&str>,
    is_running: bool,
) -> crate::daemon::state::SessionRecord {
    crate::daemon::state::SessionRecord {
        session_id: session_id.to_string(),
        task_id: task_id.map(ToString::to_string),
        name: "lexington".to_string(),
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

fn count_nudge_coworker(effects: &[Effect], target_name: &str) -> usize {
    effects
        .iter()
        .filter(|e| matches!(e, Effect::NudgeCoworker { name, .. } if name == target_name))
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
fn test_dedup_removes_duplicate_nudge_coworker() {
    let effects = vec![
        Effect::nudge_coworker(
            "riverside",
            "CI green",
            "ci_green",
            vec![Effect::RecordPrNudge {
                pr_number: 42,
                issue_type: PrIssueType::Approved,
            }],
        ),
        Effect::nudge_coworker(
            "riverside",
            "review complete",
            "review_complete",
            vec![Effect::RecordPrNudge {
                pr_number: 42,
                issue_type: PrIssueType::ReviewComplete,
            }],
        ),
        Effect::nudge_coworker(
            "riverside",
            "merge conflict",
            "merge_conflict",
            vec![Effect::RecordPrNudge {
                pr_number: 42,
                issue_type: PrIssueType::MergeConflict,
            }],
        ),
    ];

    let deduped = dedup_nudge_effects(effects);
    assert_eq!(
        count_nudge_coworker(&deduped, "riverside"),
        1,
        "Should collapse 3 nudges into 1"
    );
    // First message wins, but all callbacks are merged
    if let Effect::NudgeCoworker {
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
        panic!("Expected NudgeCoworker");
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
fn test_dedup_nudge_coworker_merges_callbacks() {
    // First NudgeCoworker has no callbacks, second has callbacks — callbacks
    // merge into the first nudge's (empty) on_success vec.
    let effects = vec![
        Effect::nudge_coworker("riverside", "plain nudge", "nudge", vec![]),
        Effect::nudge_coworker(
            "riverside",
            "callback nudge",
            "nudge",
            vec![Effect::RecordPrNudge {
                pr_number: 42,
                issue_type: PrIssueType::Approved,
            }],
        ),
    ];

    let deduped = dedup_nudge_effects(effects);
    assert_eq!(
        count_nudge_coworker(&deduped, "riverside"),
        1,
        "Should collapse 2 nudges into 1"
    );
    // Callbacks merged into the first nudge
    if let Effect::NudgeCoworker { on_success, .. } = &deduped[0] {
        assert_eq!(on_success.len(), 1, "Callback should be merged");
    } else {
        panic!("Expected NudgeCoworker");
    }
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

    // Register the channel lead session as Running so is_nudgeable() returns true.
    let session_name = crate::launch::channel_lead_session_name(&channel);
    state
        .session_manager
        .insert_test_session(
            &session_name,
            crate::daemon::sessions::SessionStatus::Running,
        )
        .await;
    state
        .session_manager
        .set_test_is_alive_hook(Some(std::sync::Arc::new(|_| true)));

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
fn test_dedup_quadruple_nudge_coworker_scenario() {
    // Reproduces the exact bug: 4 nudges to same coworker in 1 second
    // from different PR issue sources.
    let effects = vec![
        Effect::nudge_coworker(
            "riverside",
            "PR #181 - CI checks passed",
            "ci_green",
            vec![Effect::RecordPrNudge {
                pr_number: 181,
                issue_type: PrIssueType::Approved,
            }],
        ),
        Effect::nudge_coworker(
            "riverside",
            "PR #181 - Review complete",
            "review_complete",
            vec![Effect::RecordPrNudge {
                pr_number: 181,
                issue_type: PrIssueType::ReviewComplete,
            }],
        ),
        Effect::nudge_coworker(
            "riverside",
            "PR #181 - Merge conflict",
            "merge_conflict",
            vec![Effect::RecordPrNudge {
                pr_number: 181,
                issue_type: PrIssueType::MergeConflict,
            }],
        ),
        Effect::nudge_coworker(
            "riverside",
            "PR #181 - Green with feedback",
            "green_with_feedback",
            vec![Effect::RecordPrNudge {
                pr_number: 181,
                issue_type: PrIssueType::GreenWithFeedback,
            }],
        ),
    ];

    let deduped = dedup_nudge_effects(effects);

    // Should have: 1 nudge (with merged callbacks)
    assert_eq!(
        count_nudge_coworker(&deduped, "riverside"),
        1,
        "4 nudges should collapse into 1"
    );

    // The merged nudge should have all 4 on_success callbacks
    if let Effect::NudgeCoworker {
        on_success,
        message,
        ..
    } = &deduped[0]
    {
        assert_eq!(message, "PR #181 - CI checks passed", "First message wins");
        assert_eq!(on_success.len(), 4, "All 4 callbacks should be merged");
    } else {
        panic!("Expected NudgeCoworker");
    }
}

#[tokio::test]
async fn test_execute_effects_cleanup_merged_worktree_removes_registry_entry_and_posts_ops_message()
{
    use chrono::Utc;

    let (state, project_dir, _guard) = make_workflow_test_state("myrepo-cleanup-merged");
    let assignment = crate::worktree_registry::WorktreeAssignment {
        worktree_id: "task-42-dry-up-cleanup".to_string(),
        branch_name: "riverside/task-42-dry-up-cleanup".to_string(),
        task_id: Some("42".to_string()),
        current_coworker: Some("riverside".to_string()),
        pr_number: Some(4242),
        created_at: Utc::now(),
        completed_at: None,
    };

    {
        let mut ps = state.persistent_state.lock().await;
        ps.worktree_registry
            .assign_worktree(assignment.clone())
            .expect("assign worktree");
    }

    execute_effects(
        vec![Effect::CleanupMergedWorktree {
            pr_number: 4242,
            branch: assignment.branch_name.clone(),
        }],
        &state,
    )
    .await;

    {
        let ps = state.persistent_state.lock().await;
        assert!(
            ps.worktree_registry.get(&assignment.worktree_id).is_none(),
            "merged cleanup should remove assignment from registry"
        );
    }

    let ops_messages = read_channel_messages(&project_dir, "ops");
    let cleanup_msg = ops_messages
        .iter()
        .find(|m| {
            m["content"].as_str().is_some_and(|c| {
                c.contains("Cleaned up worktree task-42-dry-up-cleanup")
                    && c.contains("after PR #4242 merged")
                    && c.contains("(task !42)")
            })
        })
        .expect("cleanup message should be posted to ops channel");
    assert_eq!(
        cleanup_msg["channel"].as_str(),
        Some("ops"),
        "cleanup notification should target ops channel"
    );
}

#[tokio::test]
async fn test_execute_effects_cleanup_stale_worktree_removes_registry_entry_and_posts_ops_message()
{
    use chrono::Utc;

    let (state, project_dir, _guard) = make_workflow_test_state("myrepo-cleanup-stale");
    let assignment = crate::worktree_registry::WorktreeAssignment {
        worktree_id: "task-99-expired-worktree".to_string(),
        branch_name: "riverside/task-99-expired-worktree".to_string(),
        task_id: Some("99".to_string()),
        current_coworker: Some("riverside".to_string()),
        pr_number: None,
        created_at: Utc::now(),
        completed_at: None,
    };

    {
        let mut ps = state.persistent_state.lock().await;
        ps.worktree_registry
            .assign_worktree(assignment.clone())
            .expect("assign worktree");
    }

    execute_effects(
        vec![Effect::CleanupStaleWorktree {
            worktree_id: assignment.worktree_id.clone(),
        }],
        &state,
    )
    .await;

    {
        let ps = state.persistent_state.lock().await;
        assert!(
            ps.worktree_registry.get(&assignment.worktree_id).is_none(),
            "stale cleanup should remove assignment from registry"
        );
    }

    let ops_messages = read_channel_messages(&project_dir, "ops");
    let cleanup_msg = ops_messages
        .iter()
        .find(|m| {
            m["content"].as_str().is_some_and(|c| {
                c.contains("Cleaned up worktree task-99-expired-worktree")
                    && c.contains("retention period expired")
                    && c.contains("(task !99)")
            })
        })
        .expect("stale cleanup message should be posted to ops channel");
    assert_eq!(
        cleanup_msg["channel"].as_str(),
        Some("ops"),
        "cleanup notification should target ops channel"
    );
}

// ── Session-centric effect tests ──────────────────────────────────────

#[test]
fn test_coworker_break_updates_session_record() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};
    use std::collections::HashMap;

    let mut persistent_state = DaemonPersistentState::default();
    let record = SessionRecord {
        session_id: "sess-abc-123".to_string(),
        task_id: Some("42".to_string()),
        name: "lexington".to_string(),
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
        record.name = String::new();
    }

    let stored = persistent_state.sessions.get("sess-abc-123").unwrap();
    assert!(!stored.is_running);
    assert!(stored.name.is_empty());
}

#[test]
fn test_shutdown_coworker_impl_updates_session_via_name_lookup() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};
    use std::collections::HashMap;

    let mut persistent_state = DaemonPersistentState::default();
    let mut name_to_session: HashMap<String, String> = HashMap::new();
    let mut session_to_name: HashMap<String, String> = HashMap::new();

    let record = SessionRecord {
        session_id: "sess-123".to_string(),
        task_id: Some("42".to_string()),
        name: "lexington".to_string(),
        working_dir: "/tmp/worktree".to_string(),
        is_running: true,
        ..Default::default()
    };
    persistent_state
        .sessions
        .insert(record.session_id.clone(), record);
    name_to_session.insert("lexington".to_string(), "sess-123".to_string());
    session_to_name.insert("sess-123".to_string(), "lexington".to_string());

    let session_id = name_to_session.get("lexington").cloned();
    if let Some(session_id) = &session_id
        && let Some(sr) = persistent_state.sessions.get_mut(session_id)
    {
        sr.is_running = false;
        // Name is now stable — not cleared on shutdown
    }
    name_to_session.remove("lexington");
    if let Some(sid) = session_id {
        session_to_name.remove(&sid);
    }

    let stored = persistent_state.sessions.get("sess-123").unwrap();
    assert!(!stored.is_running);
    assert_eq!(
        stored.name, "lexington",
        "name should be stable after shutdown"
    );
    assert!(name_to_session.is_empty());
    assert!(session_to_name.is_empty());
}

#[test]
fn test_spawn_session_marks_old_records_with_same_name_as_not_running() {
    use crate::daemon::state::{DaemonPersistentState, SessionRecord};
    use chrono::Utc;

    let mut persistent_state = DaemonPersistentState::default();

    let old_record = SessionRecord {
        session_id: "sess-old-111".to_string(),
        task_id: Some("42".to_string()),
        name: "riverside".to_string(),
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
        name: "riverside".to_string(),
        working_dir: "/tmp/worktree".to_string(),
        pr_number: Some(100),
        agent_type: "midtown-code-reviewer".to_string(),
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
        name: "amsterdam".to_string(),
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
        if record.session_id != new_session_id && record.is_running && record.name == effective_name
        {
            record.is_running = false;
        }
    }

    let new_record = SessionRecord {
        session_id: new_session_id.to_string(),
        task_id: Some("50".to_string()),
        name: effective_name.to_string(),
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
    let (session_agg_tx, _session_agg_rx) = crate::daemon::session_events::channel();
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
        session_agg_tx,
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

#[tokio::test]
async fn plugin_actions_to_effects_channel_post() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-actions");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "channel.post".to_string(),
        params: serde_json::json!({"message": "hello from plugin", "channel": "test-ch"}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state).await;
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::PostSystemMessage { message, channel }
            if message == "hello from plugin" && *channel == Some("test-ch".to_string())
    ));
}

#[tokio::test]
async fn plugin_actions_to_effects_nudge_coworker() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-nudge");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "coworker.nudge".to_string(),
        params: serde_json::json!({"name": "lexington", "message": "PR approved"}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state).await;
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::NudgeSession { .. }));
}

#[tokio::test]
async fn plugin_actions_to_effects_task_done() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-done");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "task.done".to_string(),
        params: serde_json::json!({"id": "42"}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state).await;
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::CompleteTask { task_id, .. } if task_id == "42"
    ));
}

#[tokio::test]
async fn plugin_actions_to_effects_auto_merge() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-merge");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "pr.auto-merge".to_string(),
        params: serde_json::json!({"pr": 123}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state).await;
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::AutoMergePr { pr_number, .. } if *pr_number == 123
    ));
}

#[tokio::test]
async fn plugin_actions_to_effects_unknown_method_skipped() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-unk");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "unknown.method".to_string(),
        params: serde_json::json!({}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state).await;
    assert!(effects.is_empty(), "unknown methods should be skipped");
}

#[tokio::test]
async fn plugin_actions_to_effects_multiple_actions() {
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

    let effects = plugin_actions_to_effects(&actions, &state).await;
    assert_eq!(effects.len(), 3);
}

#[tokio::test]
async fn plugin_actions_to_effects_channel_post_empty_message_skipped() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-empty-msg");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "channel.post".to_string(),
        params: serde_json::json!({"channel": "test-ch"}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state).await;
    assert!(
        effects.is_empty(),
        "channel.post with missing message should be skipped"
    );
}

#[tokio::test]
async fn plugin_actions_to_effects_channel_post_blank_message_skipped() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-blank-msg");

    let actions = vec![super::super::plugin_daemon::PluginAction {
        method: "channel.post".to_string(),
        params: serde_json::json!({"message": "", "channel": "test-ch"}),
    }];

    let effects = plugin_actions_to_effects(&actions, &state).await;
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

fn mk_task(pr: Option<u64>, status: crate::task_store::TaskStatus) -> crate::task_store::Task {
    crate::task_store::Task {
        id: "1".to_string(),
        subject: "test task".to_string(),
        status,
        agent_name: String::new(),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr,
        ..Default::default()
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
        mk_task(Some(42), crate::task_store::TaskStatus::Completed),
        mk_task(Some(42), crate::task_store::TaskStatus::Completed),
    ];
    assert!(
        !super::create_task_duplicate_exists(&tasks, 42),
        "only completed tasks for this PR → allowed to create a new one"
    );
}

#[test]
fn create_task_duplicate_exists_returns_true_for_pending_task() {
    let tasks = vec![mk_task(Some(42), crate::task_store::TaskStatus::Pending)];
    assert!(
        super::create_task_duplicate_exists(&tasks, 42),
        "pending task for PR → skip creation"
    );
}

#[test]
fn create_task_duplicate_exists_returns_true_for_in_progress_task() {
    let tasks = vec![mk_task(Some(42), crate::task_store::TaskStatus::InProgress)];
    assert!(
        super::create_task_duplicate_exists(&tasks, 42),
        "in-progress task for PR → skip creation"
    );
}

#[test]
fn create_task_duplicate_exists_ignores_other_pr_numbers() {
    // Task exists for PR 99, not PR 42 — must not block PR 42.
    let tasks = vec![mk_task(Some(99), crate::task_store::TaskStatus::Pending)];
    assert!(
        !super::create_task_duplicate_exists(&tasks, 42),
        "task for a different PR → not a duplicate"
    );
}

#[test]
fn create_task_duplicate_exists_ignores_tasks_without_pr() {
    // Task with no associated PR must not block a PR-specific CreateTask.
    let tasks = vec![mk_task(None, crate::task_store::TaskStatus::Pending)];
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

        // Add a session record for "old-coworker" that is active on the SAME
        // worktree, so the collision guard detects a real collision.
        ps.sessions.insert(
            "sess-old".to_string(),
            crate::daemon::state::SessionRecord {
                session_id: "sess-old".to_string(),
                name: "old-coworker".to_string(),
                working_dir: "/tmp/worktrees/wt-collision-test".to_string(),
                is_running: true,
                ..Default::default()
            },
        );
        ps.tick_active_session_ids.insert("sess-old".to_string());
        ps.tick_active_session_names
            .insert("old-coworker".to_string());
    }

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

    // The bind itself must have been blocked — worktree still bound to old-coworker.
    let assignment = ps
        .worktree_registry
        .get("wt-collision-test")
        .expect("worktree should exist");
    assert_eq!(
        assignment.current_coworker.as_deref(),
        Some("old-coworker"),
        "Collision guard must block the bind when the active session IS on this worktree"
    );
}

// ---------------------------------------------------------------------------
// BindCoworkerToWorktree — reused session name on different worktree
//
// When a reviewer session name is reused (common across PR cycles), the
// collision guard must NOT block the bind if the active session with that
// name is working on a DIFFERENT worktree.  The guard should cross-reference
// session records (name + working_dir) rather than checking name-only via
// is_alive().
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bind_coworker_allows_reused_name_on_different_worktree() {
    let (state, _project_dir, _guard) = make_workflow_test_state("myrepo-reuse");

    // Register worktree "wt-old" and bind it to "park-reviewer".
    {
        let mut ps = state.persistent_state.lock().await;
        ps.worktree_registry
            .assign_worktree(crate::worktree_registry::WorktreeAssignment {
                worktree_id: "wt-old".to_string(),
                branch_name: "park-reviewer/task-old".to_string(),
                task_id: None,
                current_coworker: None,
                pr_number: None,
                created_at: chrono::Utc::now(),
                completed_at: None,
            })
            .expect("assign wt-old");
        ps.worktree_registry
            .bind_coworker("wt-old", "park-reviewer")
            .expect("bind park-reviewer to wt-old");

        // Add a session record for "park-reviewer" that is active but on a
        // DIFFERENT worktree ("wt-new").  This simulates the name being reused
        // for a new task/worktree.
        ps.sessions.insert(
            "sess-park-new".to_string(),
            crate::daemon::state::SessionRecord {
                session_id: "sess-park-new".to_string(),
                name: "park-reviewer".to_string(),
                working_dir: "/tmp/worktrees/wt-new".to_string(),
                is_running: true,
                ..Default::default()
            },
        );
        ps.tick_active_session_ids
            .insert("sess-park-new".to_string());
        ps.tick_active_session_names
            .insert("park-reviewer".to_string());
    }

    // is_alive returns true for "park-reviewer" (the reused name IS alive,
    // just on a different worktree).
    state
        .session_manager
        .set_test_is_alive_hook(Some(std::sync::Arc::new(|name: &str| {
            name == "park-reviewer"
        })));

    // Attempt to bind "new-coworker" to "wt-old".
    // The stale binding to "park-reviewer" should NOT block this because
    // the active "park-reviewer" session is on "wt-new", not "wt-old".
    execute_effects(
        vec![Effect::BindCoworkerToWorktree {
            worktree_id: "wt-old".to_string(),
            coworker: "new-coworker".to_string(),
        }],
        &state,
    )
    .await;

    let ps = state.persistent_state.lock().await;
    let assignment = ps
        .worktree_registry
        .get("wt-old")
        .expect("wt-old should exist");
    assert_eq!(
        assignment.current_coworker.as_deref(),
        Some("new-coworker"),
        "Bind must succeed when the active session with the same name is on a different worktree"
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

    // Insert a SessionRecord with bound_thread_id for the sender
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            "sess-test".into(),
            crate::daemon::state::SessionRecord {
                session_id: "sess-test".into(),
                name: sender.clone(),
                bound_thread_id: Some(thread_id.clone()),
                agent_type: "midtown-channel-lead".into(),
                is_running: true,
                ..Default::default()
            },
        );
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

/// Spawning a session for a task produces a PostSystemMessage separator
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

/// Spawning a session for a task without a subject still produces a separator.
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

    // Pre-create a reviewer task in TaskStore and a span so post_pr_comment can store the comment ID
    let pr_number = 42u64;
    let task_id = "42";
    create_reviewer_task(&state, task_id, pr_number);
    {
        let mut ps = state.persistent_state.lock().await;
        ps.insert_session_for_task(task_id, "park", "midtown-code-reviewer", "");
        if let Some(s) = ps
            .sessions
            .values_mut()
            .find(|s| s.task_id.as_deref() == Some(task_id))
        {
            s.pr_number = Some(pr_number);
        }
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
        body: "<!-- midtown task:100 type:review-placeholder -->\n## Review Status\n\n🔍 Review in progress by park..."
            .to_string(),
    }];
    execute_effects(effects, &state).await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    // Verify the comment ID was parsed and stored in task_placeholder_comment_id
    {
        assert_eq!(
            state
                .task_store
                .load(task_id)
                .ok()
                .and_then(|t| t.placeholder_comment_id),
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
    let task_id = "55";
    create_reviewer_task(&state, task_id, pr_number);
    {
        let mut ps = state.persistent_state.lock().await;
        ps.insert_session_for_task(task_id, "madison", "midtown-code-reviewer", "");
        if let Some(s) = ps
            .sessions
            .values_mut()
            .find(|s| s.task_id.as_deref() == Some(task_id))
        {
            s.pr_number = Some(pr_number);
        }
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
        body: "<!-- midtown task:100 type:review-placeholder -->\nReview in progress..."
            .to_string(),
    }];
    execute_effects(effects, &state).await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    {
        assert_eq!(
            state
                .task_store
                .load(task_id)
                .ok()
                .and_then(|t| t.placeholder_comment_id),
            Some(11223),
            "Should parse comment ID 11223 from the bare numeric URL"
        );
    }
}

/// Verify that when a placeholder comment ID is already stored on the
/// task metadata (from a previous reviewer cycle), `post_pr_comment`
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
    let task_id = "77";
    {
        // Pre-create task with existing placeholder comment ID
        let mut task = crate::task_store::Task {
            id: task_id.to_string(),
            subject: "Review PR".to_string(),
            status: crate::task_store::TaskStatus::InProgress,
            pr: Some(pr_number),
            agent_type: "midtown-code-reviewer".to_string(),
            agent_name: "riverside".to_string(),
            ..Default::default()
        };
        task.placeholder_comment_id = Some(existing_comment_id);
        let _ = state.task_store.save(&task);
    }
    {
        let mut ps = state.persistent_state.lock().await;
        ps.insert_session_for_task(task_id, "riverside", "midtown-code-reviewer", "");
        if let Some(s) = ps
            .sessions
            .values_mut()
            .find(|s| s.task_id.as_deref() == Some(task_id))
        {
            s.pr_number = Some(pr_number);
        }
        // Pre-populate the placeholder_comment_id (as if a previous reviewer
        // cycle posted it before timing out). This is the tier 1 lookup path.
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
        body: "<!-- midtown task:100 type:review-placeholder -->\n## Review Status\n\n🔍 Review in progress by riverside..."
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

    // Verify: the existing comment ID is still stored in task_placeholder_comment_id
    {
        assert_eq!(
            state
                .task_store
                .load(task_id)
                .ok()
                .and_then(|t| t.placeholder_comment_id),
            Some(existing_comment_id),
            "Should preserve the existing comment ID in task_placeholder_comment_id"
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
    let task_id = "88";
    create_reviewer_task(&state, task_id, pr_number);
    {
        let mut ps = state.persistent_state.lock().await;
        ps.insert_session_for_task(task_id, "madison", "midtown-code-reviewer", "");
        if let Some(s) = ps
            .sessions
            .values_mut()
            .find(|s| s.task_id.as_deref() == Some(task_id))
        {
            s.pr_number = Some(pr_number);
        }
        // Do NOT set task_placeholder_comment_id — simulates daemon restart
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
  echo '{{"comments": [{{"body": "<!-- midtown task:100 type:review-placeholder -->\n## Review Status\n\n🔍 Review in progress by pleasant...", "url": "https://github.com/test/repo/pull/88#issuecomment-{existing_comment_id}"}}]}}'
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
            "<!-- midtown task:100 type:review-placeholder -->\n## Review Status\n\n🔍 Review in progress by madison..."
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

    // Verify: the placeholder ID was stored in task_placeholder_comment_id
    {
        assert_eq!(
            state
                .task_store
                .load(task_id)
                .ok()
                .and_then(|t| t.placeholder_comment_id),
            Some(existing_comment_id),
        );
    }
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
    let (session_agg_tx, _session_agg_rx) = crate::daemon::session_events::channel();
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
        session_agg_tx,
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

#[tokio::test]
async fn cleanup_orphaned_worktrees_posts_success_only_after_actual_cleanup() {
    let (state, temp_dir, _guard) = make_insight_test_state("orphan-cleanup-success");
    let orphan_id = "task-9001-orphan";
    let orphan_path = state
        .coworkers
        .worktree_manager()
        .task_worktrees_base()
        .join(orphan_id);
    std::fs::create_dir_all(&orphan_path).expect("create orphan worktree dir");

    execute_effects(
        vec![Effect::CleanupOrphanedWorktrees { retention_hours: 0 }],
        &state,
    )
    .await;

    assert!(
        !orphan_path.exists(),
        "orphaned worktree should be removed on successful cleanup"
    );

    let ops_messages = read_channel_messages(&temp_dir, "ops");
    assert!(
        ops_messages.iter().any(|m| {
            m.get("content")
                .and_then(|v| v.as_str())
                .is_some_and(|content| content.contains(orphan_id))
        }),
        "ops channel should get a success message after cleanup"
    );
}

#[tokio::test]
async fn cleanup_orphaned_worktrees_skips_success_message_on_cleanup_failure() {
    let (state, temp_dir, _guard) = make_insight_test_state("orphan-cleanup-failure");
    let orphan_id = "task-9002-orphan";
    let orphan_path = state
        .coworkers
        .worktree_manager()
        .task_worktrees_base()
        .join(orphan_id);
    std::fs::create_dir_all(&orphan_path).expect("create orphan worktree dir");

    // Force cleanup failure: prune requires a valid git repo at repo_root.
    // Removing .git causes force_cleanup_task_worktree() to return Err.
    std::fs::remove_dir_all(temp_dir.path().join(".git")).expect("remove .git");

    execute_effects(
        vec![Effect::CleanupOrphanedWorktrees { retention_hours: 0 }],
        &state,
    )
    .await;

    let ops_messages = read_channel_messages(&temp_dir, "ops");
    assert!(
        ops_messages.is_empty(),
        "cleanup failure should not emit a false success message to ops"
    );
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
                name: "ops-lead".to_string(),
                agent_type: "midtown-channel-lead".to_string(),
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
                name: "ops-lead".to_string(),
                agent_type: "midtown-channel-lead".to_string(),
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
                name: "coworker1".to_string(),
                agent_type: "midtown-code-author".to_string(),
                task_id: Some("50".to_string()),
                is_running: true,
                ..Default::default()
            },
        );
        // Deliberately NOT setting task_channel — task lives in default channel
    }
    state
        .task_store
        .save(&crate::task_store::Task {
            id: "50".into(),
            channel: None, // no channel — task lives in default channel
            thread_id: Some(thread_id.into()),
            ..Default::default()
        })
        .unwrap();

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
                name: "coworker1".to_string(),
                agent_type: "midtown-code-author".to_string(),
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
                name: "coworker1".to_string(),
                agent_type: "midtown-code-author".to_string(),
                task_id: Some("99".to_string()),
                is_running: true,
                ..Default::default()
            },
        );

        // Old task in a different channel (stale session — not checked, but add for completeness)

        // Current task in the correct channel
    }

    // Old task in a different channel (stale, should not be used)
    state
        .task_store
        .save(&crate::task_store::Task {
            id: "88".into(),
            channel: Some("old-stale-channel".into()),
            thread_id: Some("old-thread-id".into()),
            ..Default::default()
        })
        .unwrap();

    // Active task routes to "my-feature" channel with the expected thread
    state
        .task_store
        .save(&crate::task_store::Task {
            id: "99".into(),
            channel: Some("my-feature".into()),
            thread_id: Some(thread_parent_id.into()),
            ..Default::default()
        })
        .unwrap();

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
                name: "coworker1".to_string(),
                agent_type: "midtown-code-author".to_string(),
                task_id: Some("42".to_string()),
                is_running: true,
                ..Default::default()
            },
        );
    }
    state
        .task_store
        .save(&crate::task_store::Task {
            id: "42".into(),
            channel: Some("my-feature".into()),
            thread_id: Some(thread_parent_id.into()),
            ..Default::default()
        })
        .unwrap();

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
                name: "coworker1".to_string(),
                agent_type: "midtown-code-author".to_string(),
                task_id: Some("99".to_string()),
                is_running: true,
                ..Default::default()
            },
        );
        // Deliberately NOT setting task_channel — simulates task created without --channel
    }
    state
        .task_store
        .save(&crate::task_store::Task {
            id: "99".into(),
            channel: None, // no channel — task lives in default channel
            thread_id: Some(thread_parent_id.into()),
            ..Default::default()
        })
        .unwrap();

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
                name: "coworker1".to_string(),
                agent_type: "midtown-code-author".to_string(),
                task_id: Some("42".to_string()),
                is_running: true,
                ..Default::default()
            },
        );
        // Deliberately NOT setting task_thread_id
    }
    state
        .task_store
        .save(&crate::task_store::Task {
            id: "42".into(),
            channel: Some("my-feature".into()),
            thread_id: None, // no thread binding
            ..Default::default()
        })
        .unwrap();

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

    // Register the coworker as active via session record
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            session_id.clone(),
            crate::daemon::state::SessionRecord {
                session_id: session_id.clone(),
                name: coworker_name.to_string(),
                is_running: true,
                ..Default::default()
            },
        );
    }

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

    // Don't register any session — nudge_lead() sends via session_manager.
    // This should not panic — the session_manager handles missing sessions gracefully.
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

    // Register in channel_lead_sessions and make the channel lead nudgeable.
    // The DM nudge falls through the active-session check (no session in
    // name_to_session for the DM agent), detects the channel lead via
    // channel_lead_sessions, and re-emits NudgeChannelLead for "auth".
    // The non-DM path then needs is_nudgeable("auth") to be true.
    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert(channel_lead_name.to_string(), session_id.clone());
    }

    // Register the channel lead session as Running so is_nudgeable() returns true
    // when the re-emitted NudgeChannelLead goes through the non-DM path.
    state
        .session_manager
        .insert_test_session(
            channel_lead_name,
            crate::daemon::sessions::SessionStatus::Running,
        )
        .await;
    state
        .session_manager
        .set_test_is_alive_hook(Some(std::sync::Arc::new(|_| true)));

    // The hook captures send_message_to_session_id calls.
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
async fn test_nudge_channel_lead_dm_system_nudge_posts_to_dm_channel() {
    let (state, project_dir, _guard) = make_workflow_test_state("myrepo");
    let coworker_name = "columbus";
    let channel_name = format!("dm-{}", coworker_name);
    let session_id = "sess-columbus-1".to_string();

    // Register the coworker as active
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            session_id.clone(),
            crate::daemon::state::SessionRecord {
                session_id: session_id.clone(),
                name: coworker_name.to_string(),
                is_running: true,
                ..Default::default()
            },
        );
    }

    // Hook: stdin delivery succeeds
    state
        .session_manager
        .set_test_send_message_to_session_id_hook(Some(std::sync::Arc::new(|_sid, _msg| Ok(()))));

    // Use a system nudge (Nudge variant), NOT DmFromUser
    execute_effects(
        vec![Effect::NudgeChannelLead {
            channel_name: channel_name.clone(),
            reason: crate::daemon::wake_reason::WakeReason::Nudge {
                message: "Stale note reminder: check task !42".to_string(),
            },
        }],
        &state,
    )
    .await;

    // The system nudge should appear in the DM channel history
    let messages = read_channel_messages(&project_dir, &channel_name);
    assert_eq!(
        messages.len(),
        1,
        "system nudge should be posted to DM channel"
    );
    assert_eq!(
        messages[0]["type"].as_str(),
        Some("nudge"),
        "message type should be nudge"
    );
    assert!(
        messages[0]["content"]
            .as_str()
            .unwrap_or("")
            .contains("Stale note reminder"),
        "nudge content should be in the DM channel message"
    );
}

#[tokio::test]
async fn test_nudge_channel_lead_dm_from_user_skips_dm_post() {
    let (state, project_dir, _guard) = make_workflow_test_state("myrepo");
    let coworker_name = "columbus";
    let channel_name = format!("dm-{}", coworker_name);
    let session_id = "sess-columbus-1".to_string();

    // Register the coworker as active
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            session_id.clone(),
            crate::daemon::state::SessionRecord {
                session_id: session_id.clone(),
                name: coworker_name.to_string(),
                is_running: true,
                ..Default::default()
            },
        );
    }

    state
        .session_manager
        .set_test_send_message_to_session_id_hook(Some(std::sync::Arc::new(|_sid, _msg| Ok(()))));

    // DmFromUser: rpc_channel.rs already posted to the DM channel, so
    // NudgeChannelLead should NOT double-post.
    execute_effects(
        vec![Effect::NudgeChannelLead {
            channel_name: channel_name.clone(),
            reason: crate::daemon::wake_reason::WakeReason::DmFromUser {
                content: "hey columbus".to_string(),
                msg_id: "msg-dm-skip-001".to_string(),
                coworker_name: coworker_name.to_string(),
            },
        }],
        &state,
    )
    .await;

    let messages = read_channel_messages(&project_dir, &channel_name);
    assert!(
        messages.is_empty(),
        "DmFromUser should NOT be double-posted to the DM channel"
    );
}

#[tokio::test]
async fn test_nudge_channel_lead_dm_fork_skips_dm_post() {
    let (state, project_dir, _guard) = make_workflow_test_state("myrepo");
    let fork_name = "research-fork";
    let channel_name = format!("dm-{}", fork_name);
    let session_id = "sess-fork-1".to_string();

    // Register the fork as an active fork session (bound_thread_id + channel-lead agent type)
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            session_id.clone(),
            crate::daemon::state::SessionRecord {
                session_id: session_id.clone(),
                name: fork_name.to_string(),
                is_running: true,
                agent_type: "midtown-channel-lead".to_string(),
                bound_thread_id: Some("thread-123".to_string()),
                ..Default::default()
            },
        );
    }

    state
        .session_manager
        .set_test_send_message_to_session_id_hook(Some(std::sync::Arc::new(|_sid, _msg| Ok(()))));

    // System nudge to a fork session — should NOT post to DM channel
    execute_effects(
        vec![Effect::NudgeChannelLead {
            channel_name: channel_name.clone(),
            reason: crate::daemon::wake_reason::WakeReason::Nudge {
                message: "health check".to_string(),
            },
        }],
        &state,
    )
    .await;

    let messages = read_channel_messages(&project_dir, &channel_name);
    assert!(
        messages.is_empty(),
        "fork sessions should NOT get DM channel posts"
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

// ── RecordTaskAssignment session update tests ───────────────────────────────

/// RecordTaskAssignment should update the persistent session record's task_id
/// so that post_insight can resolve the correct channel for insights.
#[tokio::test]
async fn test_record_task_assignment_updates_session_task_id() {
    let (state, _temp_dir, _guard) = make_insight_test_state("task-assign-session");

    let session_id = "session-abc";
    let coworker_name = "houston";

    // Set up a session record with no task_id (simulating an idle session)
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            session_id.to_string(),
            super::super::state::SessionRecord {
                session_id: session_id.to_string(),
                name: coworker_name.to_string(),
                agent_type: "midtown-code-author".to_string(),
                is_running: true,
                task_id: None,
                ..Default::default()
            },
        );
    }

    // Execute RecordTaskAssignment
    let effects = vec![Effect::RecordTaskAssignment {
        coworker: coworker_name.to_string(),
        task_id: "42".to_string(),
    }];
    execute_effects(effects, &state).await;

    // Verify sessions[].task_id was updated
    {
        let ps = state.persistent_state.lock().await;
        let record = ps.sessions.get(session_id).expect("session should exist");
        assert_eq!(
            record.task_id.as_deref(),
            Some("42"),
            "RecordTaskAssignment should update session record's task_id"
        );
    }

    // Verify task can be looked up via session_by_task
    {
        let ps = state.persistent_state.lock().await;
        let found = ps.session_by_task("42").map(|s| s.session_id.clone());
        assert_eq!(
            found.as_deref(),
            Some(session_id),
            "session_by_task should find the session for the new task"
        );
    }
}

/// When a session is reassigned to a new task via RecordTaskAssignment,
/// insights should route to the new task's channel (not the old one).
#[tokio::test]
async fn test_record_task_assignment_fixes_insight_routing() {
    let (state, temp_dir, _guard) = make_insight_test_state("insight-routing-fix");

    let session_id = "session-xyz";
    let coworker_name = "houston";

    // Set up a session with a stale task_id from a previous assignment
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            session_id.to_string(),
            super::super::state::SessionRecord {
                session_id: session_id.to_string(),
                name: coworker_name.to_string(),
                agent_type: "midtown-code-author".to_string(),
                is_running: true,
                task_id: Some("old-task".to_string()),
                ..Default::default()
            },
        );
    }

    // Old task routes to old channel
    state
        .task_store
        .save(&crate::task_store::Task {
            id: "old-task".into(),
            channel: Some("old-channel".into()),
            ..Default::default()
        })
        .unwrap();

    // New task routes to new channel
    state
        .task_store
        .save(&crate::task_store::Task {
            id: "new-task".into(),
            channel: Some("new-channel".into()),
            ..Default::default()
        })
        .unwrap();

    // Reassign the session to the new task
    execute_effects(
        vec![Effect::RecordTaskAssignment {
            coworker: coworker_name.to_string(),
            task_id: "new-task".to_string(),
        }],
        &state,
    )
    .await;

    // Now post an insight — it should route to "new-channel"
    super::post_insight(&state, coworker_name, "Insight after reassignment").await;

    let messages = read_channel_messages(&temp_dir, "new-channel");
    let found = messages.iter().any(|m| {
        m["content"]
            .as_str()
            .is_some_and(|c| c.contains("Insight after reassignment"))
    });
    assert!(
        found,
        "insight should route to the new task's channel after RecordTaskAssignment"
    );

    // Verify it did NOT go to the old channel
    let old_messages = read_channel_messages(&temp_dir, "old-channel");
    let in_old = old_messages.iter().any(|m| {
        m["content"]
            .as_str()
            .is_some_and(|c| c.contains("Insight after reassignment"))
    });
    assert!(
        !in_old,
        "insight should NOT route to the old task's channel"
    );
}

#[test]
fn link_pr_to_session_backfills_pr_number_on_session_record() {
    let mut ps = crate::daemon::state::DaemonPersistentState::default();
    let session = crate::daemon::state::SessionRecord {
        session_id: "session-abc".to_string(),
        task_id: Some("42".to_string()),
        ..Default::default()
    };
    ps.sessions.insert("session-abc".to_string(), session);

    // Simulate what LinkPrToSession handler does:
    let pr_number: u64 = 100;
    let session_id = "session-abc";
    if let Some(record) = ps.sessions.get_mut(session_id)
        && record.pr_number.is_none()
    {
        record.pr_number = Some(pr_number);
    }

    assert_eq!(ps.sessions.get("session-abc").unwrap().pr_number, Some(100));
}

#[test]
fn link_pr_to_session_does_not_overwrite_existing_pr_number() {
    let mut ps = crate::daemon::state::DaemonPersistentState::default();
    let session = crate::daemon::state::SessionRecord {
        session_id: "session-abc".to_string(),
        task_id: Some("42".to_string()),
        pr_number: Some(50), // Already has a PR number
        ..Default::default()
    };
    ps.sessions.insert("session-abc".to_string(), session);

    // Simulate backfill — should NOT overwrite
    let pr_number: u64 = 100;
    let session_id = "session-abc";
    if let Some(record) = ps.sessions.get_mut(session_id)
        && record.pr_number.is_none()
    {
        record.pr_number = Some(pr_number);
    }

    // Should keep the original PR number
    assert_eq!(ps.sessions.get("session-abc").unwrap().pr_number, Some(50));
}

#[test]
fn link_pr_to_session_backfills_branch_on_session_record() {
    let mut ps = crate::daemon::state::DaemonPersistentState::default();
    let session = crate::daemon::state::SessionRecord {
        session_id: "session-abc".to_string(),
        task_id: Some("42".to_string()),
        branch: None,
        ..Default::default()
    };
    ps.sessions.insert("session-abc".to_string(), session);

    // Simulate what LinkPrToSession handler does:
    let branch = "feature-branch".to_string();
    let session_id = "session-abc";
    if let Some(record) = ps.sessions.get_mut(session_id)
        && record.branch.is_none()
    {
        record.branch = Some(branch.clone());
    }

    assert_eq!(
        ps.sessions.get("session-abc").unwrap().branch,
        Some("feature-branch".to_string())
    );
}

// ============================================================================
// lookup_existing_placeholder — task_placeholder_comment_id path
// ============================================================================

/// Verify that `post_pr_comment` reuses a placeholder stored in
/// `task_placeholder_comment_id` via active reviewer spans.
///
/// This covers the tier-1 lookup path in `lookup_existing_placeholder` that
/// finds the placeholder via spans + task_placeholder_comment_id.
#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await.
#[tokio::test]
async fn test_post_pr_comment_reuses_placeholder_from_task_placeholder_comment_id() {
    use std::os::unix::fs::PermissionsExt;

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();

    let (state, _project_dir, _guard) = make_workflow_test_state("post-pr-task-reviewer-meta");

    let pr_number = 66u64;
    let existing_comment_id = 77777u64;
    let task_id = "review-task-66";
    {
        // Pre-create task with placeholder comment ID
        let mut task = crate::task_store::Task {
            id: task_id.to_string(),
            subject: "Review PR".to_string(),
            status: crate::task_store::TaskStatus::InProgress,
            pr: Some(pr_number),
            agent_type: "midtown-code-reviewer".to_string(),
            agent_name: "lexington".to_string(),
            ..Default::default()
        };
        task.placeholder_comment_id = Some(existing_comment_id);
        let _ = state.task_store.save(&task);
    }
    {
        let mut ps = state.persistent_state.lock().await;
        ps.insert_session_for_task(task_id, "lexington", "midtown-code-reviewer", "sess-lex-1");
        if let Some(s) = ps
            .sessions
            .values_mut()
            .find(|s| s.task_id.as_deref() == Some(task_id))
        {
            s.pr_number = Some(pr_number);
        }
    }

    // Mock `gh` to accept the PATCH and log all calls.
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
  echo 'https://github.com/test/repo/pull/66#issuecomment-99998'
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
        reviewer_name: "lexington".to_string(),
        body: "<!-- midtown task:100 type:review-placeholder -->\n## Review Status\n\n🔍 Review in progress by lexington..."
            .to_string(),
    }];
    execute_effects(effects, &state).await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    // Verify: the PATCH endpoint was called (editing existing comment, not creating new)
    let log_contents = std::fs::read_to_string(&log_file).unwrap_or_default();
    assert!(
        log_contents.contains("PATCH"),
        "Should have called gh api --method PATCH to edit the placeholder from task_placeholder_comment_id, got: {}",
        log_contents,
    );
    assert!(
        !log_contents.contains("pr comment"),
        "Should NOT have called `gh pr comment` when task_placeholder_comment_id has a placeholder, got: {}",
        log_contents,
    );
}

// ── refresh_channel_lead_worktree tests ─────────────────────────────────

/// Create a temporary git repo with one commit and return its path.
fn create_temp_git_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo_path = dir.path().to_path_buf();

    Command::new("git")
        .args(["init", "--initial-branch=main"])
        .current_dir(&repo_path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&repo_path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&repo_path)
        .output()
        .unwrap();

    // Create initial commit
    std::fs::write(repo_path.join("file.txt"), "initial").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(&repo_path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(&repo_path)
        .output()
        .unwrap();

    (dir, repo_path)
}

#[tokio::test]
async fn test_refresh_channel_lead_worktree_updates_to_origin() {
    let (_dir, repo_path) = create_temp_git_repo();

    // Create a detached worktree
    let worktree_path = repo_path.join("worktrees").join("ops");
    std::fs::create_dir_all(worktree_path.parent().unwrap()).unwrap();
    let output = Command::new("git")
        .args([
            "worktree",
            "add",
            "--detach",
            worktree_path.to_str().unwrap(),
            "HEAD",
        ])
        .current_dir(&repo_path)
        .output()
        .unwrap();
    assert!(output.status.success(), "Failed to create worktree");

    // Record the worktree's current HEAD
    let old_head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&worktree_path)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    // Make a new commit in the main repo
    std::fs::write(repo_path.join("file.txt"), "updated").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(&repo_path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "second commit"])
        .current_dir(&repo_path)
        .output()
        .unwrap();

    let new_head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo_path)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    assert_ne!(old_head, new_head, "New commit should have different SHA");

    // Simulate "origin/main" by creating a bare clone as origin.
    let origin_tmpdir = tempfile::tempdir().unwrap();
    let origin_dir = origin_tmpdir.path().join("origin.git");
    let output = Command::new("git")
        .args([
            "clone",
            "--bare",
            repo_path.to_str().unwrap(),
            origin_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Failed to create bare clone: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Add origin remote to the main repo (worktree shares the git config)
    Command::new("git")
        .args(["remote", "add", "origin", origin_dir.to_str().unwrap()])
        .current_dir(&repo_path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["fetch", "origin"])
        .current_dir(&repo_path)
        .output()
        .unwrap();

    // Verify the worktree is still at the old HEAD
    let wt_head_before = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&worktree_path)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    assert_eq!(
        wt_head_before, old_head,
        "Worktree should still be at old HEAD"
    );

    // Run the refresh
    super::refresh_channel_lead_worktree(&worktree_path, "main").await;

    // Verify the worktree is now at origin/main
    let wt_head_after = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&worktree_path)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    assert_eq!(
        wt_head_after, new_head,
        "Worktree should be updated to origin/main"
    );
}

#[tokio::test]
async fn test_refresh_channel_lead_worktree_nonexistent_path() {
    // Should be a no-op (not panic) when the worktree doesn't exist
    let path = std::path::PathBuf::from("/tmp/nonexistent-worktree-test-12345");
    super::refresh_channel_lead_worktree(&path, "main").await;
    // No panic = success
}
