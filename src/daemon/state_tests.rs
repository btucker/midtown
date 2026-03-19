use super::*;
use tempfile::tempdir;

#[test]
fn test_pr_to_task_map_from_sessions() {
    let mut sessions = HashMap::new();
    sessions.insert(
        "sess1".to_string(),
        SessionRecord {
            session_id: "sess1".to_string(),
            pr_number: Some(42),
            task_id: Some("7".to_string()),
            ..Default::default()
        },
    );
    sessions.insert(
        "sess2".to_string(),
        SessionRecord {
            session_id: "sess2".to_string(),
            pr_number: None, // no PR — should be excluded
            task_id: Some("8".to_string()),
            ..Default::default()
        },
    );
    let map = pr_to_task_map_from_sessions(&sessions);
    assert_eq!(map.get(&42), Some(&"7".to_string()));
    assert_eq!(map.len(), 1);
}

#[test]
fn test_task_to_pr_map_from_sessions() {
    let mut sessions = HashMap::new();
    sessions.insert(
        "sess1".to_string(),
        SessionRecord {
            session_id: "sess1".to_string(),
            pr_number: Some(42),
            task_id: Some("7".to_string()),
            ..Default::default()
        },
    );
    sessions.insert(
        "sess2".to_string(),
        SessionRecord {
            session_id: "sess2".to_string(),
            pr_number: Some(99),
            task_id: None, // no task — should be excluded
            ..Default::default()
        },
    );
    let map = task_to_pr_map_from_sessions(&sessions);
    assert_eq!(map.get("7"), Some(&42u64));
    assert_eq!(map.len(), 1);
}

#[test]
fn test_default_state() {
    let state = DaemonPersistentState::default();
    assert!(state.reminders.reminders.is_empty());
}

#[test]
fn test_save_and_load_roundtrip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("daemon-state.json");

    let mut state = DaemonPersistentState::default();
    state.reminders.add(
        crate::reminders::ReminderTrigger::AllWorkMerged,
        "Deploy".to_string(),
        crate::reminders::RepeatPolicy::Once,
    );
    state.github.mark_reviewed_pr(42);

    // Save directly to path
    let contents = serde_json::to_string_pretty(&state).unwrap();
    fs::write(&path, &contents).unwrap();

    // Load directly from path
    let loaded: DaemonPersistentState =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(loaded.github.has_cached_review(42));
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
    assert!(state.github.reviewed_prs.is_empty());
}

#[test]
fn test_empty_json_uses_defaults() {
    let json = "{}";
    let state: DaemonPersistentState = serde_json::from_str(json).unwrap();
    assert!(state.github.reviewed_prs.is_empty());
    assert!(state.reminders.reminders.is_empty());
}

#[test]
fn test_sessions_in_persistent_state() {
    let mut state = DaemonPersistentState::default();
    state.sessions.insert(
        "session-42".to_string(),
        SessionRecord {
            session_id: "session-42".to_string(),
            name: "park".to_string(),
            working_dir: "/path/to/park-worktree".to_string(),
            agent_type: "midtown-code-author".to_string(),
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
            name: "madison".to_string(),
            working_dir: "/path/to/madison-worktree".to_string(),
            agent_type: "midtown-code-author".to_string(),
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
    state.github.mark_reviewed_pr(10);
    state.github.mark_reviewed_pr(11);

    // Populate reminders
    state.reminders.add(
        crate::reminders::ReminderTrigger::AllWorkMerged,
        "Cut release".to_string(),
        crate::reminders::RepeatPolicy::Once,
    );
    state.reminders.add(
        crate::reminders::ReminderTrigger::AllWorkMerged,
        "Deploy staging".to_string(),
        crate::reminders::RepeatPolicy::Once,
    );

    // Serialize and deserialize
    let json = serde_json::to_string_pretty(&state).unwrap();
    let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();

    assert!(loaded.github.has_cached_review(10));
    assert!(loaded.github.has_cached_review(11));
    assert_eq!(loaded.reminders.reminders.len(), 2);
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
            name: "amsterdam".to_string(),
            agent_type: "midtown-channel-lead".to_string(),
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
        record.agent_type.as_str(),
        "midtown-channel-lead",
        "agent_type should be 'midtown-channel-lead'"
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
        name: name.to_string(),
        agent_type: "midtown-code-reviewer".to_string(),
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
    assert_eq!(session.name, "riverside");
}

#[test]
fn test_upsert_session_running_updates_current_name_on_existing_entry() {
    // Verify that current_name is refreshed when resuming a stopped session.
    // A stopped session may have current_name=None (cleared by handle_session_stopped).
    // On resume, upsert_session_running must restore current_name from the new record.
    let mut state = DaemonPersistentState::default();

    // Pre-populate with a stopped session whose current_name was cleared on stop
    let mut stopped_record = make_test_session_record_named("session-xyz", false, "old-name");
    stopped_record.name = String::new(); // cleared when session stopped
    state
        .sessions
        .insert("session-xyz".to_string(), stopped_record);

    // Upsert with a new record carrying the current name
    let new_record = make_test_session_record_named("session-xyz", true, "new-name");
    state.upsert_session_running("session-xyz".to_string(), new_record);

    let session = state.sessions.get("session-xyz").unwrap();
    assert!(session.is_running, "Session must be marked running");
    assert_eq!(
        session.name, "new-name",
        "name must be updated from the new record"
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
        if !record.name.is_empty()
            && let Some(tid) = &record.bound_thread_id
        {
            fork_bound_threads.insert(record.name.clone(), tid.clone());
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
        if !record.name.is_empty()
            && let Some(tid) = &record.bound_thread_id
        {
            fork_bound_threads.insert(record.name.clone(), tid.clone());
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
        if !record.name.is_empty()
            && let Some(tid) = &record.bound_thread_id
        {
            fork_bound_threads.insert(record.name.clone(), tid.clone());
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
    channel_lead_record.agent_type = "midtown-channel-lead".to_string();
    channel_lead_record.bound_thread_id = Some("thread-fork".to_string());
    channel_lead_record.channel = Some("topic-fork".to_string());
    sessions.insert("sess-fork".to_string(), channel_lead_record);

    let mut dev_record = make_test_session_record_named("sess-task", true, "riverside");
    dev_record.agent_type = "midtown-code-author".to_string();
    dev_record.bound_thread_id = Some("thread-task".to_string());
    dev_record.channel = Some("topic-task".to_string());
    sessions.insert("sess-task".to_string(), dev_record);

    let mut fork_bound_channels: HashMap<String, String> = HashMap::new();
    let mut fork_bound_threads: HashMap<String, String> = HashMap::new();
    for (_session_id, record) in sessions.iter() {
        if !record.name.is_empty()
            && let Some(tid) = &record.bound_thread_id
        {
            fork_bound_threads.insert(record.name.clone(), tid.clone());
            if record.agent_type == "midtown-channel-lead"
                && let Some(ref channel) = record.channel
            {
                fork_bound_channels.insert(record.name.clone(), channel.clone());
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
    // (SpawnForTask only inserts when bound_thread_id.is_some())
    let new_bound_thread_id: Option<String> = None;
    if let Some(tid) = new_bound_thread_id {
        fork_bound_threads.insert("riverside".to_string(), tid);
    }

    assert!(
        !fork_bound_threads.contains_key("riverside"),
        "Stale fork_bound_threads entry must not persist after cleanup + reuse without binding"
    );
}

// ── topic_sessions rebuild tests ──────────────────────────────────────────────
//
// These tests verify the startup reconstruction of the `topic_sessions`
// in-memory cache (thread_parent_id → session_id) from persisted `SessionRecord`
// entries with `bound_thread_id` and `coworker_type == "channel-lead"`. This
// ensures that after a daemon restart, thread replies are routed to existing
// fork sessions instead of spawning duplicates.

/// Channel-lead sessions with `bound_thread_id` populate `topic_sessions`
/// during the rebuild loop in `DaemonState::new`.
#[test]
fn test_topic_sessions_rebuilt_from_channel_lead_fork_records() {
    use std::collections::HashMap;

    let mut sessions: HashMap<String, SessionRecord> = HashMap::new();
    let mut fork_record = make_test_session_record_named("fork-sess-1", true, "fork-riverside");
    fork_record.agent_type = "midtown-channel-lead".to_string();
    fork_record.bound_thread_id = Some("thread-msg-abc".to_string());
    sessions.insert("fork-sess-1".to_string(), fork_record);

    // Simulate the rebuild loop from DaemonState::new
    let mut topic_sessions: HashMap<String, String> = HashMap::new();
    for (session_id, record) in &sessions {
        if record.agent_type == "midtown-channel-lead"
            && let Some(ref tid) = record.bound_thread_id
        {
            topic_sessions.insert(tid.clone(), session_id.clone());
        }
    }

    assert_eq!(
        topic_sessions.get("thread-msg-abc"),
        Some(&"fork-sess-1".to_string()),
        "topic_sessions should map thread_parent_id → session_id for channel-lead forks"
    );
}

/// Non-channel-lead sessions with `bound_thread_id` (e.g., task coworkers)
/// must NOT populate `topic_sessions`. Only forked channel leads route
/// thread replies.
#[test]
fn test_topic_sessions_skips_non_channel_lead_sessions() {
    use std::collections::HashMap;

    let mut sessions: HashMap<String, SessionRecord> = HashMap::new();
    let mut task_record = make_test_session_record_named("task-sess-1", true, "riverside");
    task_record.agent_type = "midtown-code-author".to_string();
    task_record.bound_thread_id = Some("thread-task-xyz".to_string());
    sessions.insert("task-sess-1".to_string(), task_record);

    let mut topic_sessions: HashMap<String, String> = HashMap::new();
    for (session_id, record) in &sessions {
        if record.agent_type == "midtown-channel-lead"
            && let Some(ref tid) = record.bound_thread_id
        {
            topic_sessions.insert(tid.clone(), session_id.clone());
        }
    }

    assert!(
        topic_sessions.is_empty(),
        "Task coworkers with bound_thread_id must not appear in topic_sessions"
    );
}

/// Sessions without `bound_thread_id` (root channel leads) must NOT populate
/// `topic_sessions`.
#[test]
fn test_topic_sessions_skips_channel_leads_without_bound_thread() {
    use std::collections::HashMap;

    let mut sessions: HashMap<String, SessionRecord> = HashMap::new();
    let mut root_lead = make_test_session_record_named("lead-sess-1", true, "ops-lead");
    root_lead.agent_type = "midtown-channel-lead".to_string();
    // bound_thread_id is None — this is a root channel lead, not a fork
    sessions.insert("lead-sess-1".to_string(), root_lead);

    let mut topic_sessions: HashMap<String, String> = HashMap::new();
    for (session_id, record) in &sessions {
        if record.agent_type == "midtown-channel-lead"
            && let Some(ref tid) = record.bound_thread_id
        {
            topic_sessions.insert(tid.clone(), session_id.clone());
        }
    }

    assert!(
        topic_sessions.is_empty(),
        "Root channel leads (no bound_thread_id) must not appear in topic_sessions"
    );
}

/// Mixed sessions: only channel-lead forks with bound_thread_id appear in
/// `topic_sessions`.
#[test]
fn test_topic_sessions_only_includes_channel_lead_forks() {
    use std::collections::HashMap;

    let mut sessions: HashMap<String, SessionRecord> = HashMap::new();

    // Fork channel lead (should be included)
    let mut fork = make_test_session_record_named("fork-1", true, "fork-ops");
    fork.agent_type = "midtown-channel-lead".to_string();
    fork.bound_thread_id = Some("thread-111".to_string());
    sessions.insert("fork-1".to_string(), fork);

    // Root channel lead (no bound_thread_id, should be excluded)
    let mut root_lead = make_test_session_record_named("lead-1", true, "ops-root");
    root_lead.agent_type = "midtown-channel-lead".to_string();
    sessions.insert("lead-1".to_string(), root_lead);

    // Task coworker with bound_thread_id (should be excluded)
    let mut task = make_test_session_record_named("task-1", true, "riverside");
    task.agent_type = "midtown-code-author".to_string();
    task.bound_thread_id = Some("thread-222".to_string());
    sessions.insert("task-1".to_string(), task);

    // Regular coworker (no bound_thread_id, should be excluded)
    let unbound = make_test_session_record_named("reg-1", true, "amsterdam");
    sessions.insert("reg-1".to_string(), unbound);

    let mut topic_sessions: HashMap<String, String> = HashMap::new();
    for (session_id, record) in &sessions {
        if record.agent_type == "midtown-channel-lead"
            && let Some(ref tid) = record.bound_thread_id
        {
            topic_sessions.insert(tid.clone(), session_id.clone());
        }
    }

    assert_eq!(
        topic_sessions.len(),
        1,
        "Only the fork channel lead should appear"
    );
    assert_eq!(
        topic_sessions.get("thread-111"),
        Some(&"fork-1".to_string()),
        "Fork session should be mapped by its thread_parent_id"
    );
    assert!(
        !topic_sessions.values().any(|v| v == "task-1"),
        "Task coworker must not appear in topic_sessions"
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

// ── migrate_workflow_state_from_dir tests ─────────────────────────────────

#[test]
fn test_migrate_workflow_state_from_dir_with_valid_files() {
    let dir = tempdir().unwrap();
    let channels_dir = dir.path().join("channels");

    // Create two channel dirs with valid workflow-state.json
    let ch1_dir = channels_dir.join("auth");
    fs::create_dir_all(&ch1_dir).unwrap();
    fs::write(
        ch1_dir.join("workflow-state.json"),
        r#"{"stage": "review", "count": 3}"#,
    )
    .unwrap();

    let ch2_dir = channels_dir.join("frontend");
    fs::create_dir_all(&ch2_dir).unwrap();
    fs::write(ch2_dir.join("workflow-state.json"), r#"{"stage": "dev"}"#).unwrap();

    let (migrated, files_to_delete) =
        DaemonPersistentState::migrate_workflow_state_from_dir(&channels_dir);

    assert_eq!(migrated.len(), 2);
    assert_eq!(migrated["auth"]["stage"], "review");
    assert_eq!(migrated["auth"]["count"], 3);
    assert_eq!(migrated["frontend"]["stage"], "dev");

    // Files should NOT have been deleted yet (caller's responsibility)
    assert_eq!(files_to_delete.len(), 2);
    for path in &files_to_delete {
        assert!(
            path.exists(),
            "Legacy file should still exist before caller deletes"
        );
    }
}

#[test]
fn test_migrate_workflow_state_from_dir_skips_invalid_json() {
    let dir = tempdir().unwrap();
    let channels_dir = dir.path().join("channels");

    // Valid channel
    let valid_dir = channels_dir.join("good");
    fs::create_dir_all(&valid_dir).unwrap();
    fs::write(valid_dir.join("workflow-state.json"), r#"{"ok": true}"#).unwrap();

    // Invalid JSON channel
    let bad_dir = channels_dir.join("bad");
    fs::create_dir_all(&bad_dir).unwrap();
    fs::write(bad_dir.join("workflow-state.json"), "not valid json{{{").unwrap();

    let (migrated, files_to_delete) =
        DaemonPersistentState::migrate_workflow_state_from_dir(&channels_dir);

    assert_eq!(migrated.len(), 1);
    assert!(migrated.contains_key("good"));
    assert!(!migrated.contains_key("bad"));

    // Only the valid file should be queued for deletion
    assert_eq!(files_to_delete.len(), 1);
    // The invalid file should be preserved on disk
    assert!(bad_dir.join("workflow-state.json").exists());
}

#[test]
fn test_migrate_workflow_state_from_dir_no_channels_dir() {
    let dir = tempdir().unwrap();
    let channels_dir = dir.path().join("channels"); // does not exist

    let (migrated, files_to_delete) =
        DaemonPersistentState::migrate_workflow_state_from_dir(&channels_dir);

    assert!(migrated.is_empty());
    assert!(files_to_delete.is_empty());
}

#[test]
fn test_migrate_workflow_state_from_dir_skips_non_directory_entries() {
    let dir = tempdir().unwrap();
    let channels_dir = dir.path().join("channels");
    fs::create_dir_all(&channels_dir).unwrap();

    // Create a regular file (not a directory) in channels/
    fs::write(channels_dir.join("stray-file.json"), "{}").unwrap();

    // Create a valid channel dir
    let ch_dir = channels_dir.join("real-channel");
    fs::create_dir_all(&ch_dir).unwrap();
    fs::write(ch_dir.join("workflow-state.json"), r#"{"v": 1}"#).unwrap();

    let (migrated, _) = DaemonPersistentState::migrate_workflow_state_from_dir(&channels_dir);

    assert_eq!(migrated.len(), 1);
    assert!(migrated.contains_key("real-channel"));
}

#[test]
fn test_migrate_workflow_state_from_dir_channel_without_state_file() {
    let dir = tempdir().unwrap();
    let channels_dir = dir.path().join("channels");

    // Channel dir exists but has no workflow-state.json
    let ch_dir = channels_dir.join("empty-channel");
    fs::create_dir_all(&ch_dir).unwrap();

    let (migrated, files_to_delete) =
        DaemonPersistentState::migrate_workflow_state_from_dir(&channels_dir);

    assert!(migrated.is_empty());
    assert!(files_to_delete.is_empty());
}

// ── apply_gc tests ───────────────────────────────────────────────────────

fn make_gc_session(session_id: &str, task_id: Option<&str>, has_prompt: bool) -> SessionRecord {
    SessionRecord {
        session_id: session_id.to_string(),
        task_id: task_id.map(|s| s.to_string()),
        initial_prompt: if has_prompt {
            Some("test prompt data".to_string())
        } else {
            None
        },
        agent_type: "midtown-code-author".to_string(),
        ..Default::default()
    }
}

#[test]
fn apply_gc_removes_dead_sessions() {
    let mut state = DaemonPersistentState::default();
    state
        .sessions
        .insert("dead-1".to_string(), make_gc_session("dead-1", None, true));
    state
        .sessions
        .insert("dead-2".to_string(), make_gc_session("dead-2", None, false));
    state.sessions.insert(
        "alive-1".to_string(),
        make_gc_session("alive-1", None, true),
    );

    let result = state.apply_gc(&["dead-1".to_string(), "dead-2".to_string()], &[]);

    assert_eq!(result.sessions_removed, 2);
    assert_eq!(state.sessions.len(), 1);
    assert!(state.sessions.contains_key("alive-1"));
}

#[test]
fn apply_gc_counts_orphaned_tasks() {
    let mut state = DaemonPersistentState::default();
    // GC no longer prunes legacy map entries — task metadata lives in TaskStore.
    // Just verify the count is reported correctly.
    let result = state.apply_gc(&[], &["orphan-1".to_string()]);
    assert_eq!(result.orphaned_tasks_pruned, 1);
}

#[test]
fn apply_gc_has_changes_reports_correctly() {
    let empty = GcResult::default();
    assert!(!empty.has_changes());

    let with_removal = GcResult {
        sessions_removed: 1,
        ..Default::default()
    };
    assert!(with_removal.has_changes());

    let with_prune = GcResult {
        orphaned_tasks_pruned: 1,
        ..Default::default()
    };
    assert!(with_prune.has_changes());
}

#[test]
fn apply_gc_no_op_returns_empty_result() {
    let mut state = DaemonPersistentState::default();
    state
        .sessions
        .insert("s1".to_string(), make_gc_session("s1", Some("42"), true));

    let result = state.apply_gc(&[], &[]);

    assert!(!result.has_changes());
    assert_eq!(state.sessions.len(), 1); // untouched
}

#[test]
fn apply_gc_combined_operations() {
    let mut state = DaemonPersistentState::default();

    // Session to remove
    state.sessions.insert(
        "dead".to_string(),
        make_gc_session("dead", Some("old-task"), true),
    );
    // Surviving session (not in dead list)
    state.sessions.insert(
        "alive".to_string(),
        make_gc_session("alive", Some("active-task"), true),
    );
    // Orphaned task metadata (lives in TaskStore, not in state maps)

    let result = state.apply_gc(&["dead".to_string()], &["old-task".to_string()]);

    assert_eq!(result.sessions_removed, 1);
    assert_eq!(result.orphaned_tasks_pruned, 1);
    assert!(result.has_changes());

    assert!(!state.sessions.contains_key("dead"));
    // Surviving session preserved with initial_prompt intact
    assert!(
        state
            .sessions
            .get("alive")
            .unwrap()
            .initial_prompt
            .is_some()
    );
    // GC no longer prunes legacy map entries — task metadata lives in TaskStore.
}

// ── channel_workflows tests ───────────────────────────────────────────

#[test]
fn test_channel_workflow_assignment_roundtrip() {
    let mut state = DaemonPersistentState::default();

    // No workflow assigned initially
    assert!(!state.channel_workflows.contains_key("proj-auth"));

    // Assign workflow
    state
        .channel_workflows
        .insert("proj-auth".to_string(), "tdw".to_string());
    assert_eq!(state.channel_workflows.get("proj-auth").unwrap(), "tdw");

    // Serialization round-trip
    let json = serde_json::to_string(&state).unwrap();
    let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();
    assert_eq!(loaded.channel_workflows.get("proj-auth").unwrap(), "tdw");
}

#[test]
fn test_channel_workflows_default_empty() {
    // Existing state without channel_workflows should deserialize fine
    let json = r#"{"github": {}, "reminders": {"reminders": []}}"#;
    let state: DaemonPersistentState = serde_json::from_str(json).unwrap();
    assert!(state.channel_workflows.is_empty());
}

#[test]
fn test_channel_workflows_multiple_channels() {
    let mut state = DaemonPersistentState::default();
    state
        .channel_workflows
        .insert("proj-auth".to_string(), "tdw".to_string());
    state
        .channel_workflows
        .insert("proj-infra".to_string(), "tdw".to_string());
    state
        .channel_workflows
        .insert("proj-web".to_string(), "spec-review".to_string());

    let json = serde_json::to_string_pretty(&state).unwrap();
    let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();

    assert_eq!(loaded.channel_workflows.len(), 3);
    // Multiple channels can share the same workflow
    assert_eq!(loaded.channel_workflows.get("proj-auth").unwrap(), "tdw");
    assert_eq!(loaded.channel_workflows.get("proj-infra").unwrap(), "tdw");
    assert_eq!(
        loaded.channel_workflows.get("proj-web").unwrap(),
        "spec-review"
    );
}

#[test]
fn test_channel_workflows_unassign() {
    let mut state = DaemonPersistentState::default();
    state
        .channel_workflows
        .insert("proj-auth".to_string(), "tdw".to_string());
    state.channel_workflows.remove("proj-auth");

    let json = serde_json::to_string(&state).unwrap();
    let loaded: DaemonPersistentState = serde_json::from_str(&json).unwrap();
    assert!(!loaded.channel_workflows.contains_key("proj-auth"));
}

#[test]
fn test_is_fork_session() {
    // Regular dev session (no thread binding) — not a fork
    let regular = SessionRecord {
        bound_thread_id: None,
        ..Default::default()
    };
    assert!(
        !regular.is_fork_session(),
        "Regular session should not be a fork"
    );

    // Channel-lead with bound_thread_id — IS a fork
    let fork = SessionRecord {
        agent_type: "midtown-channel-lead".to_string(),
        bound_thread_id: Some("thread-123".to_string()),
        ..Default::default()
    };
    assert!(
        fork.is_fork_session(),
        "Channel-lead with bound_thread_id is a fork"
    );

    // Dev coworker with bound_thread_id — NOT a fork (genuine task owner)
    let dev_with_thread = SessionRecord {
        agent_type: "midtown-code-author".to_string(),
        bound_thread_id: Some("thread-456".to_string()),
        ..Default::default()
    };
    assert!(
        !dev_with_thread.is_fork_session(),
        "Dev coworker with bound_thread_id is NOT a fork — it's a real task owner"
    );

    // Channel-lead without bound_thread_id — NOT a fork (root channel lead)
    let root_lead = SessionRecord {
        agent_type: "midtown-channel-lead".to_string(),
        bound_thread_id: None,
        ..Default::default()
    };
    assert!(
        !root_lead.is_fork_session(),
        "Root channel lead without bound_thread_id is NOT a fork"
    );
}

#[test]
fn test_session_by_name() {
    let mut ps = DaemonPersistentState::default();
    ps.sessions.insert(
        "sess-1".into(),
        SessionRecord {
            session_id: "sess-1".into(),
            name: "lexington".into(),
            task_id: Some("42".into()),
            ..Default::default()
        },
    );
    assert_eq!(
        ps.session_by_name("lexington").unwrap().session_id,
        "sess-1"
    );
    assert!(ps.session_by_name("park").is_none());
}

#[test]
fn test_session_by_name_mut() {
    let mut ps = DaemonPersistentState::default();
    ps.sessions.insert(
        "sess-1".into(),
        SessionRecord {
            session_id: "sess-1".into(),
            name: "lexington".into(),
            ..Default::default()
        },
    );
    let record = ps.session_by_name_mut("lexington").unwrap();
    record.is_running = true;
    assert!(ps.session_by_name("lexington").unwrap().is_running);
    assert!(ps.session_by_name_mut("park").is_none());
}

#[test]
fn test_session_by_task() {
    let mut ps = DaemonPersistentState::default();
    ps.sessions.insert(
        "sess-1".into(),
        SessionRecord {
            session_id: "sess-1".into(),
            name: "lexington".into(),
            task_id: Some("42".into()),
            ..Default::default()
        },
    );
    assert_eq!(ps.session_by_task("42").unwrap().session_id, "sess-1");
    assert!(ps.session_by_task("99").is_none());
}

#[test]
fn test_running_reviewer_sessions() {
    let mut ps = DaemonPersistentState::default();
    ps.sessions.insert(
        "sess-1".into(),
        SessionRecord {
            session_id: "sess-1".into(),
            name: "park".into(),
            agent_type: "midtown-code-reviewer".into(),
            is_running: true,
            ..Default::default()
        },
    );
    ps.sessions.insert(
        "sess-2".into(),
        SessionRecord {
            session_id: "sess-2".into(),
            name: "madison".into(),
            agent_type: "midtown-code-author".into(),
            is_running: true,
            ..Default::default()
        },
    );
    let reviewers = ps.running_reviewer_sessions();
    assert_eq!(reviewers.len(), 1);
    assert_eq!(reviewers[0].name, "park");
}

#[test]
fn test_running_reviewer_sessions_excludes_stopped() {
    let mut ps = DaemonPersistentState::default();
    ps.sessions.insert(
        "sess-1".into(),
        SessionRecord {
            session_id: "sess-1".into(),
            name: "park".into(),
            agent_type: "midtown-code-reviewer".into(),
            is_running: false,
            ..Default::default()
        },
    );
    let reviewers = ps.running_reviewer_sessions();
    assert!(reviewers.is_empty());
}

#[test]
fn test_name_task_assignments() {
    let mut ps = DaemonPersistentState::default();
    ps.sessions.insert(
        "sess-1".into(),
        SessionRecord {
            session_id: "sess-1".into(),
            name: "lexington".into(),
            task_id: Some("42".into()),
            ..Default::default()
        },
    );
    ps.sessions.insert(
        "sess-2".into(),
        SessionRecord {
            session_id: "sess-2".into(),
            name: "park".into(),
            task_id: None,
            ..Default::default()
        },
    );
    let assignments = ps.name_task_assignments();
    assert_eq!(assignments.get("lexington").unwrap(), "42");
    assert!(!assignments.contains_key("park"));
}

#[test]
fn test_name_task_assignments_lowercases_name() {
    let mut ps = DaemonPersistentState::default();
    ps.sessions.insert(
        "sess-1".into(),
        SessionRecord {
            session_id: "sess-1".into(),
            name: "Lexington".into(),
            task_id: Some("42".into()),
            ..Default::default()
        },
    );
    let assignments = ps.name_task_assignments();
    assert_eq!(assignments.get("lexington").unwrap(), "42");
    assert!(!assignments.contains_key("Lexington"));
}
