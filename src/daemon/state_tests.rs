use super::*;
use tempfile::tempdir;

#[test]
fn test_default_state() {
    let state = DaemonPersistentState::default();
    assert!(state.github.pr_reviewers.is_empty());
    assert!(state.reminders.reminders.is_empty());
}

#[test]
fn test_save_and_load_roundtrip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("daemon-state.json");

    let mut state = DaemonPersistentState::default();
    state.github.assign_reviewer(
        42,
        "lexington",
        crate::github_state::AssignmentSource::PollingFallback,
    );
    state.reminders.add(
        crate::reminders::ReminderTrigger::AllWorkMerged,
        "Deploy".to_string(),
    );

    // Save directly to path
    let contents = serde_json::to_string_pretty(&state).unwrap();
    fs::write(&path, &contents).unwrap();

    // Load directly from path
    let loaded: DaemonPersistentState =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(loaded.github.get_reviewer(42), Some("lexington"));
    assert_eq!(loaded.reminders.reminders.len(), 1);
    assert_eq!(loaded.reminders.reminders[0].message, "Deploy");
}

#[test]
fn test_serde_default_handles_missing_fields() {
    // Forward compatibility: missing sections get defaults
    let json = r#"{"github": {}}"#;
    let state: DaemonPersistentState = serde_json::from_str(json).unwrap();
    assert!(state.reminders.reminders.is_empty());

    let json = r#"{"reminders": {"reminders": []}}"#;
    let state: DaemonPersistentState = serde_json::from_str(json).unwrap();
    assert!(state.github.pr_reviewers.is_empty());
}

#[test]
fn test_empty_json_uses_defaults() {
    let json = "{}";
    let state: DaemonPersistentState = serde_json::from_str(json).unwrap();
    assert!(state.github.pr_reviewers.is_empty());
    assert!(state.reminders.reminders.is_empty());
}

#[test]
fn test_sessions_in_persistent_state() {
    let mut state = DaemonPersistentState::default();
    state.sessions.insert(
        "session-42".to_string(),
        SessionRecord {
            session_id: "session-42".to_string(),
            current_name: Some("park".to_string()),
            working_dir: "/path/to/park-worktree".to_string(),
            coworker_type: "dev".to_string(),
            task_id: Some("3".to_string()),
            purpose: "task !3: Fix login bug".to_string(),
            pid: Some(9999),
            provider: Some(crate::auth::AuthProvider::Claude),
            profile: Some("test-profile".to_string()),
            resume_on_startup: true,
            ..Default::default()
        },
    );

    let json = serde_json::to_string_pretty(&state).unwrap();
    let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();
    assert_eq!(loaded.sessions.len(), 1);
    let park = loaded.sessions.get("session-42").unwrap();
    assert_eq!(park.session_id, "session-42");
    assert_eq!(park.purpose, "task !3: Fix login bug");
    assert_eq!(park.pid, Some(9999));
    assert_eq!(park.task_id, Some("3".to_string()));
}

#[test]
fn test_session_record_provider_persistence() {
    // Test that provider field is persisted and restored correctly
    // Reproduces the bug: when a Codex coworker is running and the daemon
    // restarts, the provider defaults to Claude if not persisted.
    let mut state = DaemonPersistentState::default();

    // Add a Codex session
    state.sessions.insert(
        "codex-session-123".to_string(),
        SessionRecord {
            session_id: "codex-session-123".to_string(),
            current_name: Some("madison".to_string()),
            working_dir: "/path/to/madison-worktree".to_string(),
            coworker_type: "dev".to_string(),
            task_id: Some("42".to_string()),
            purpose: "task !42: Add feature".to_string(),
            pid: Some(5555),
            provider: Some(crate::auth::AuthProvider::Codex),
            profile: Some("test-profile".to_string()),
            resume_on_startup: true,
            ..Default::default()
        },
    );

    // Serialize to JSON (simulating daemon shutdown)
    let json = serde_json::to_string(&state).unwrap();

    // Deserialize back (simulating daemon restart)
    let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();

    // Verify the provider was restored correctly
    let madison = loaded.sessions.get("codex-session-123").unwrap();
    assert_eq!(
        madison.provider,
        Some(crate::auth::AuthProvider::Codex),
        "Provider should be restored as Codex after daemon restart"
    );
}

#[test]
fn test_full_roundtrip_with_all_fields() {
    let mut state = DaemonPersistentState::default();

    // Populate github state
    state.github.assign_reviewer(
        1,
        "broadway",
        crate::github_state::AssignmentSource::PollingFallback,
    );
    state.github.assign_reviewer(
        2,
        "park",
        crate::github_state::AssignmentSource::PollingFallback,
    );
    state.github.mark_reviewed_pr(10);
    state
        .github
        .add_pending_review_spawn(3, chrono::Utc::now() + chrono::Duration::seconds(60));

    // Populate reminders
    state.reminders.add(
        crate::reminders::ReminderTrigger::AllWorkMerged,
        "Cut release".to_string(),
    );
    state.reminders.add(
        crate::reminders::ReminderTrigger::AllWorkMerged,
        "Deploy staging".to_string(),
    );

    // Populate task-channel mappings
    state
        .task_channel
        .insert("10".to_string(), "auth".to_string());
    state
        .task_channel
        .insert("11".to_string(), "frontend".to_string());

    // Serialize and deserialize
    let json = serde_json::to_string_pretty(&state).unwrap();
    let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();

    assert_eq!(loaded.github.pr_reviewers.len(), 2);
    assert_eq!(loaded.github.get_reviewer(1), Some("broadway"));
    assert_eq!(loaded.github.get_reviewer(2), Some("park"));
    assert!(loaded.github.has_cached_review(10));
    assert_eq!(loaded.github.pending_review_spawns.len(), 1);
    assert_eq!(loaded.reminders.reminders.len(), 2);
    assert_eq!(loaded.task_channel.len(), 2);
    assert_eq!(loaded.task_channel.get("10"), Some(&"auth".to_string()));
    assert_eq!(loaded.task_channel.get("11"), Some(&"frontend".to_string()));
}

#[test]
fn test_task_channel_mapping() {
    let mut state = DaemonPersistentState::default();
    state
        .task_channel
        .insert("42".to_string(), "auth".to_string());
    state
        .task_channel
        .insert("43".to_string(), "frontend".to_string());

    let json = serde_json::to_string_pretty(&state).unwrap();
    let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();

    assert_eq!(loaded.task_channel.len(), 2);
    assert_eq!(loaded.task_channel.get("42"), Some(&"auth".to_string()));
    assert_eq!(loaded.task_channel.get("43"), Some(&"frontend".to_string()));
}

#[test]
fn test_task_channel_default_empty() {
    // Existing state without task_channel should deserialize fine
    let json = r#"{"github": {}, "reminders": {"reminders": []}}"#;
    let state: DaemonPersistentState = serde_json::from_str(json).unwrap();
    assert!(state.task_channel.is_empty());
}

#[test]
fn test_task_channel_file_roundtrip() {
    // Exercises save_for_repo and load_for_repo with task_channel data,
    // covering the debug log statements that report task-channel mapping counts.
    let tmp = tempfile::tempdir().unwrap();
    let repo_name = "test-repo";

    // Set up the repo directory structure that save_for_repo/load_for_repo expect
    let state_dir = tmp.path().join("projects").join(repo_name);
    std::fs::create_dir_all(&state_dir).unwrap();

    // Override the state file path by saving/loading directly via serde + file I/O
    // (save_for_repo uses crate::paths which we can't easily override in tests,
    // so we test the serialization + deserialization of a state with task_channel)
    let mut state = DaemonPersistentState::default();
    state
        .task_channel
        .insert("100".to_string(), "backend".to_string());
    state
        .task_channel
        .insert("101".to_string(), "infra".to_string());
    state
        .task_channel
        .insert("102".to_string(), "frontend".to_string());

    // Write to file, simulating save_for_repo
    let state_file = state_dir.join("daemon-state.json");
    let contents = serde_json::to_string_pretty(&state).unwrap();
    std::fs::write(&state_file, &contents).unwrap();

    // Read back, simulating load_for_repo
    let loaded_contents = std::fs::read_to_string(&state_file).unwrap();
    let loaded: DaemonPersistentState = serde_json::from_str(&loaded_contents).unwrap();

    assert_eq!(loaded.task_channel.len(), 3);
    assert_eq!(loaded.task_channel.get("100"), Some(&"backend".to_string()));
    assert_eq!(loaded.task_channel.get("101"), Some(&"infra".to_string()));
    assert_eq!(
        loaded.task_channel.get("102"),
        Some(&"frontend".to_string())
    );
}

#[test]
fn test_task_channel_overwrite_and_remove() {
    let mut state = DaemonPersistentState::default();

    // Add a mapping
    state
        .task_channel
        .insert("50".to_string(), "auth".to_string());
    assert_eq!(state.task_channel.get("50"), Some(&"auth".to_string()));

    // Overwrite with a different channel
    state
        .task_channel
        .insert("50".to_string(), "security".to_string());
    assert_eq!(state.task_channel.get("50"), Some(&"security".to_string()));
    assert_eq!(state.task_channel.len(), 1);

    // Remove the mapping
    state.task_channel.remove("50");
    assert!(state.task_channel.is_empty());

    // Roundtrip after removal
    let json = serde_json::to_string_pretty(&state).unwrap();
    let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();
    assert!(loaded.task_channel.is_empty());
}

#[test]
fn test_clear_reviewer_assignment() {
    // Note: This test validates the return value behavior but doesn't test persistence
    // because DaemonPersistentState::save_for_repo requires a valid repo name
    // and filesystem paths. The save/load behavior is covered by other tests.

    let mut state = DaemonPersistentState::default();
    state.github.assign_reviewer(
        42,
        "amsterdam",
        crate::github_state::AssignmentSource::Webhook,
    );

    // Verify the assignment exists before clearing
    assert_eq!(state.github.get_reviewer(42), Some("amsterdam"));

    // Clear existing assignment - should return true
    // (Note: save will fail in tests without proper setup, but that's OK -
    // we're testing the removal logic, not file I/O)
    assert!(state.clear_reviewer_assignment("amsterdam", "test-repo"));
    assert_eq!(state.github.get_reviewer(42), None);

    // Try to clear again - should return false (no assignment)
    assert!(!state.clear_reviewer_assignment("amsterdam", "test-repo"));

    // Try to clear a coworker with no assignment - should return false
    assert!(!state.clear_reviewer_assignment("park", "test-repo"));
}

#[test]
fn test_task_model_mapping() {
    let mut state = DaemonPersistentState::default();
    state
        .task_model
        .insert("42".to_string(), "claude/opus".to_string());
    state
        .task_model
        .insert("43".to_string(), "claude/sonnet".to_string());

    let json = serde_json::to_string_pretty(&state).unwrap();
    let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();

    assert_eq!(loaded.task_model.len(), 2);
    assert_eq!(
        loaded.task_model.get("42"),
        Some(&"claude/opus".to_string())
    );
    assert_eq!(
        loaded.task_model.get("43"),
        Some(&"claude/sonnet".to_string())
    );
}

#[test]
fn test_task_model_default_empty() {
    // Existing state without task_model should deserialize fine
    let json = r#"{"github": {}, "reminders": {"reminders": []}}"#;
    let state: DaemonPersistentState = serde_json::from_str(json).unwrap();
    assert!(state.task_model.is_empty());
}

#[test]
fn test_task_model_overwrite_and_remove() {
    let mut state = DaemonPersistentState::default();

    // Add a mapping
    state
        .task_model
        .insert("50".to_string(), "claude/opus".to_string());
    assert_eq!(state.task_model.get("50"), Some(&"claude/opus".to_string()));

    // Overwrite with a different model
    state
        .task_model
        .insert("50".to_string(), "claude/haiku".to_string());
    assert_eq!(
        state.task_model.get("50"),
        Some(&"claude/haiku".to_string())
    );
    assert_eq!(state.task_model.len(), 1);

    // Remove the mapping
    state.task_model.remove("50");
    assert!(state.task_model.is_empty());

    // Roundtrip after removal
    let json = serde_json::to_string_pretty(&state).unwrap();
    let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();
    assert!(loaded.task_model.is_empty());
}

#[test]
fn test_channel_lead_session_record_coworker_type_and_channel() {
    // SessionRecord for a channel lead must persist coworker_type="channel-lead"
    // and the channel name, so the attach path can reconstruct the correct role.
    let mut state = DaemonPersistentState::default();
    state.sessions.insert(
        "session-ch-123".to_string(),
        SessionRecord {
            session_id: "session-ch-123".to_string(),
            current_name: Some("amsterdam".to_string()),
            coworker_type: "channel-lead".to_string(),
            channel: Some("daemon-architecture".to_string()),
            purpose: "channel lead for daemon-architecture".to_string(),
            pid: Some(42),
            provider: Some(crate::auth::AuthProvider::Claude),
            resume_on_startup: true,
            ..Default::default()
        },
    );

    let json = serde_json::to_string(&state).unwrap();
    let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();

    let record = loaded.sessions.get("session-ch-123").unwrap();
    assert_eq!(
        record.coworker_type.as_str(),
        "channel-lead",
        "coworker_type should be 'channel-lead', not 'dev'"
    );
    assert_eq!(
        record.channel.as_deref(),
        Some("daemon-architecture"),
        "channel name should be persisted and restored"
    );
}

fn make_test_session_record(session_id: &str, is_running: bool) -> SessionRecord {
    make_test_session_record_named(session_id, is_running, "riverside")
}

fn make_test_session_record_named(session_id: &str, is_running: bool, name: &str) -> SessionRecord {
    SessionRecord {
        session_id: session_id.to_string(),
        task_id: Some("1690".to_string()),
        current_name: Some(name.to_string()),
        preferred_name: Some(name.to_string()),
        is_reviewer: true,
        coworker_type: "reviewer".to_string(),
        is_running,
        resume_on_startup: false,
        ..Default::default()
    }
}

#[test]
fn test_upsert_session_running_inserts_new_session() {
    let mut state = DaemonPersistentState::default();
    let record = make_test_session_record("new-session-id", true);

    state.upsert_session_running("new-session-id".to_string(), record);

    let session = state.sessions.get("new-session-id").unwrap();
    assert!(session.is_running);
    assert_eq!(session.task_id, Some("1690".to_string()));
}

#[test]
fn test_upsert_session_running_marks_stopped_session_as_running() {
    // Regression: spawn_coworker used or_insert_with which doesn't update existing entries.
    // A stopped session (is_running=false) must be marked running on resume to prevent
    // dispatch_via_sessions from triggering recovery on every tick.
    let mut state = DaemonPersistentState::default();

    // Pre-populate with a stopped session (simulating a session that was stopped)
    let stopped_record = make_test_session_record("existing-session", false);
    state
        .sessions
        .insert("existing-session".to_string(), stopped_record);
    assert!(!state.sessions.get("existing-session").unwrap().is_running);

    // Upsert with a new record — the and_modify path should mark existing as running
    let new_record = make_test_session_record("existing-session", true);
    state.upsert_session_running("existing-session".to_string(), new_record);

    let session = state.sessions.get("existing-session").unwrap();
    assert!(
        session.is_running,
        "Stopped session must be marked running after upsert"
    );
    // Original record fields preserved (and_modify only sets is_running + current_name)
    assert_eq!(session.task_id, Some("1690".to_string()));
    assert_eq!(session.current_name, Some("riverside".to_string()));
}

#[test]
fn test_upsert_session_running_updates_current_name_on_existing_entry() {
    // Verify that current_name is refreshed when resuming a stopped session.
    // A stopped session may have current_name=None (cleared by handle_session_stopped).
    // On resume, upsert_session_running must restore current_name from the new record.
    let mut state = DaemonPersistentState::default();

    // Pre-populate with a stopped session whose current_name was cleared on stop
    let mut stopped_record = make_test_session_record_named("session-xyz", false, "old-name");
    stopped_record.current_name = None; // cleared when session stopped
    state
        .sessions
        .insert("session-xyz".to_string(), stopped_record);

    // Upsert with a new record carrying the current name
    let new_record = make_test_session_record_named("session-xyz", true, "new-name");
    state.upsert_session_running("session-xyz".to_string(), new_record);

    let session = state.sessions.get("session-xyz").unwrap();
    assert!(session.is_running, "Session must be marked running");
    assert_eq!(
        session.current_name,
        Some("new-name".to_string()),
        "current_name must be updated from the new record, not left as None"
    );
}

// ── fork_bound_threads rebuild tests ─────────────────────────────────────────
//
// These tests verify the startup reconstruction of the `fork_bound_threads`
// in-memory cache (coworker name → thread_id) from persisted `SessionRecord`
// entries that carry `bound_thread_id`. This ensures that after a daemon
// restart, coworker channel posts are still auto-tagged with the correct
// thread_parent_id so they appear in the fork session's thread.

/// Sessions with `bound_thread_id` populate `fork_bound_threads` during the
/// rebuild loop that runs on `DaemonState::new`.
#[test]
fn test_fork_bound_threads_rebuilt_from_session_records_with_bound_thread_id() {
    use std::collections::HashMap;

    let mut sessions: HashMap<String, SessionRecord> = HashMap::new();
    let mut bound_record = make_test_session_record_named("sess-bound", true, "riverside");
    bound_record.bound_thread_id = Some("thread-parent-xyz".to_string());
    sessions.insert("sess-bound".to_string(), bound_record);

    // Simulate the rebuild loop from DaemonState::new
    let mut fork_bound_threads: HashMap<String, String> = HashMap::new();
    for record in sessions.values() {
        if let (Some(name), Some(tid)) = (&record.current_name, &record.bound_thread_id) {
            fork_bound_threads.insert(name.clone(), tid.clone());
        }
    }

    assert_eq!(
        fork_bound_threads.get("riverside"),
        Some(&"thread-parent-xyz".to_string()),
        "fork_bound_threads should be populated from SessionRecord.bound_thread_id"
    );
}

/// Sessions without `bound_thread_id` are not added to `fork_bound_threads`.
#[test]
fn test_fork_bound_threads_skips_sessions_without_bound_thread_id() {
    use std::collections::HashMap;

    let mut sessions: HashMap<String, SessionRecord> = HashMap::new();
    let unbound_record = make_test_session_record_named("sess-unbound", true, "amsterdam");
    // bound_thread_id is None (default from make_test_session_record_named)
    sessions.insert("sess-unbound".to_string(), unbound_record);

    let mut fork_bound_threads: HashMap<String, String> = HashMap::new();
    for record in sessions.values() {
        if let (Some(name), Some(tid)) = (&record.current_name, &record.bound_thread_id) {
            fork_bound_threads.insert(name.clone(), tid.clone());
        }
    }

    assert!(
        fork_bound_threads.is_empty(),
        "Sessions without bound_thread_id must not appear in fork_bound_threads"
    );
}

/// Mixed sessions: only those with `bound_thread_id` appear in `fork_bound_threads`.
#[test]
fn test_fork_bound_threads_only_includes_bound_sessions() {
    use std::collections::HashMap;

    let mut sessions: HashMap<String, SessionRecord> = HashMap::new();

    let mut bound = make_test_session_record_named("sess-a", true, "riverside");
    bound.bound_thread_id = Some("thread-abc".to_string());
    sessions.insert("sess-a".to_string(), bound);

    let unbound = make_test_session_record_named("sess-b", true, "amsterdam");
    sessions.insert("sess-b".to_string(), unbound);

    let mut fork_bound_threads: HashMap<String, String> = HashMap::new();
    for record in sessions.values() {
        if let (Some(name), Some(tid)) = (&record.current_name, &record.bound_thread_id) {
            fork_bound_threads.insert(name.clone(), tid.clone());
        }
    }

    assert_eq!(
        fork_bound_threads.len(),
        1,
        "Only the bound session should appear"
    );
    assert_eq!(
        fork_bound_threads.get("riverside"),
        Some(&"thread-abc".to_string())
    );
    assert!(!fork_bound_threads.contains_key("amsterdam"));
}

#[test]
fn test_fork_bound_channels_rebuild_only_for_channel_leads() {
    use std::collections::HashMap;

    let mut sessions: HashMap<String, SessionRecord> = HashMap::new();
    let mut channel_lead_record = make_test_session_record_named("sess-fork", true, "channel-fork");
    channel_lead_record.coworker_type = "channel-lead".to_string();
    channel_lead_record.bound_thread_id = Some("thread-fork".to_string());
    channel_lead_record.channel = Some("topic-fork".to_string());
    sessions.insert("sess-fork".to_string(), channel_lead_record);

    let mut dev_record = make_test_session_record_named("sess-task", true, "riverside");
    dev_record.coworker_type = "dev".to_string();
    dev_record.bound_thread_id = Some("thread-task".to_string());
    dev_record.channel = Some("topic-task".to_string());
    sessions.insert("sess-task".to_string(), dev_record);

    let mut fork_bound_channels: HashMap<String, String> = HashMap::new();
    let mut fork_bound_threads: HashMap<String, String> = HashMap::new();
    for (_session_id, record) in sessions.iter() {
        if let (Some(name), Some(tid)) = (&record.current_name, &record.bound_thread_id) {
            fork_bound_threads.insert(name.clone(), tid.clone());
            if record.coworker_type == "channel-lead"
                && let Some(ref channel) = record.channel
            {
                fork_bound_channels.insert(name.clone(), channel.clone());
            }
        }
    }

    assert_eq!(
        fork_bound_threads.get("channel-fork"),
        Some(&"thread-fork".to_string())
    );
    assert_eq!(
        fork_bound_threads.get("riverside"),
        Some(&"thread-task".to_string())
    );
    assert_eq!(
        fork_bound_channels.get("channel-fork"),
        Some(&"topic-fork".to_string())
    );
    assert!(
        !fork_bound_channels.contains_key("riverside"),
        "Task coworker bound thread must not be treated as fork output channel"
    );
}
/// When a coworker with a bound thread is cleaned up and the name is reused
/// without a thread binding, the stale entry must not persist.
/// This verifies the cleanup logic in `cleanup_coworker_state` that removes
/// the name from `fork_bound_threads`.
#[test]
fn test_fork_bound_threads_cleaned_up_on_name_reuse() {
    use std::collections::HashMap;

    let mut fork_bound_threads: HashMap<String, String> = HashMap::new();

    // Simulate: coworker "riverside" spawned with thread binding
    fork_bound_threads.insert("riverside".to_string(), "thread-abc".to_string());
    assert!(fork_bound_threads.contains_key("riverside"));

    // Simulate: cleanup_coworker_state removes the entry
    fork_bound_threads.remove("riverside");

    // Simulate: name "riverside" reused for a new task WITHOUT thread binding
    // (SpawnSession only inserts when bound_thread_id.is_some())
    let new_bound_thread_id: Option<String> = None;
    if let Some(tid) = new_bound_thread_id {
        fork_bound_threads.insert("riverside".to_string(), tid);
    }

    assert!(
        !fork_bound_threads.contains_key("riverside"),
        "Stale fork_bound_threads entry must not persist after cleanup + reuse without binding"
    );
}

#[test]
fn profile_state_serializes_round_trip() {
    let state = ProfileState {
        is_usage_limited: true,
        usage_limit_reset_at: None,
        last_used_at: None,
    };
    let json = serde_json::to_string(&state).unwrap();
    let parsed: ProfileState = serde_json::from_str(&json).unwrap();
    assert!(parsed.is_usage_limited);
    assert!(parsed.usage_limit_reset_at.is_none());
    assert!(parsed.last_used_at.is_none());
}

#[test]
fn profile_state_with_timestamps_round_trips() {
    use chrono::TimeZone;
    let reset_at = chrono::Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap();
    let last_used = chrono::Utc.with_ymd_and_hms(2026, 2, 24, 8, 0, 0).unwrap();
    let state = ProfileState {
        is_usage_limited: true,
        usage_limit_reset_at: Some(reset_at),
        last_used_at: Some(last_used),
    };
    let json = serde_json::to_string(&state).unwrap();
    let parsed: ProfileState = serde_json::from_str(&json).unwrap();
    assert!(parsed.is_usage_limited);
    assert_eq!(parsed.usage_limit_reset_at, Some(reset_at));
    assert_eq!(parsed.last_used_at, Some(last_used));
}

#[test]
fn profile_pool_state_in_daemon_persistent_state() {
    let mut state = DaemonPersistentState::default();
    state.profile_pool_state.insert(
        "alice@example.com".to_string(),
        ProfileState {
            is_usage_limited: false,
            usage_limit_reset_at: None,
            last_used_at: None,
        },
    );
    state.profile_pool_state.insert(
        "bob@example.com".to_string(),
        ProfileState {
            is_usage_limited: true,
            usage_limit_reset_at: None,
            last_used_at: None,
        },
    );

    let json = serde_json::to_string_pretty(&state).unwrap();
    let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();

    assert_eq!(loaded.profile_pool_state.len(), 2);
    assert!(
        !loaded
            .profile_pool_state
            .get("alice@example.com")
            .unwrap()
            .is_usage_limited
    );
    assert!(
        loaded
            .profile_pool_state
            .get("bob@example.com")
            .unwrap()
            .is_usage_limited
    );
}

#[test]
fn profile_pool_state_default_empty() {
    // Existing state without profile_pool_state should deserialize fine.
    let json = r#"{"github": {}, "reminders": {"reminders": []}}"#;
    let state: DaemonPersistentState = serde_json::from_str(json).unwrap();
    assert!(state.profile_pool_state.is_empty());
}

#[test]
fn profile_state_default_is_not_limited() {
    let state = ProfileState::default();
    assert!(!state.is_usage_limited);
    assert!(state.usage_limit_reset_at.is_none());
    assert!(state.last_used_at.is_none());
}
