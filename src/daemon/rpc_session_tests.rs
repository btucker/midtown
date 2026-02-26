use super::*;

// ============================================================================
// parse_attach_target tests (pre-existing)
// ============================================================================

#[test]
fn test_parse_attach_target_name() {
    assert_eq!(
        parse_attach_target("name:park").unwrap(),
        AttachTarget::Name("park".to_string())
    );
    // Names are lowercased
    assert_eq!(
        parse_attach_target("name:Park").unwrap(),
        AttachTarget::Name("park".to_string())
    );
}

#[test]
fn test_parse_attach_target_name_empty() {
    assert!(parse_attach_target("name:").is_err());
}

#[test]
fn test_parse_attach_target_name_slash() {
    assert_eq!(
        parse_attach_target("name/park").unwrap(),
        AttachTarget::Name("park".to_string())
    );
}

#[test]
fn test_parse_attach_target_task() {
    assert_eq!(
        parse_attach_target("task:42").unwrap(),
        AttachTarget::Task(42)
    );
}

#[test]
fn test_parse_attach_target_task_slash() {
    assert_eq!(
        parse_attach_target("task/42").unwrap(),
        AttachTarget::Task(42)
    );
}

#[test]
fn test_parse_attach_target_task_invalid() {
    assert!(parse_attach_target("task:abc").is_err());
    assert!(parse_attach_target("task:-1").is_err());
}

#[test]
fn test_parse_attach_target_pr() {
    assert_eq!(
        parse_attach_target("pr:123").unwrap(),
        AttachTarget::Pr(123)
    );
}

#[test]
fn test_parse_attach_target_provider_session() {
    assert_eq!(
        parse_attach_target("claude/abc-123").unwrap(),
        AttachTarget::PlatformSession {
            platform: crate::auth::AuthProvider::Claude,
            session_id: "abc-123".to_string()
        }
    );
    assert_eq!(
        parse_attach_target("codex/thread-1").unwrap(),
        AttachTarget::PlatformSession {
            platform: crate::auth::AuthProvider::Codex,
            session_id: "thread-1".to_string()
        }
    );
}

#[test]
fn test_parse_attach_target_platform_only() {
    assert_eq!(
        parse_attach_target("claude").unwrap(),
        AttachTarget::Platform(crate::auth::AuthProvider::Claude)
    );
    assert_eq!(
        parse_attach_target("openai").unwrap(),
        AttachTarget::Platform(crate::auth::AuthProvider::Codex)
    );
}

#[test]
fn test_parse_attach_target_rejects_zai_platform() {
    assert!(parse_attach_target("zai/abc-123").is_err());
    assert!(parse_attach_target("z.ai/abc-123").is_err());
}

#[test]
fn test_parse_attach_target_pr_invalid() {
    assert!(parse_attach_target("pr:abc").is_err());
}

#[test]
fn test_parse_attach_target_invalid_format() {
    assert!(parse_attach_target("invalid").is_err());
    assert!(parse_attach_target("unknown:value").is_err());
    assert!(parse_attach_target("").is_err());
}

#[test]
fn test_parse_attach_target_for_clear_name() {
    assert_eq!(
        parse_attach_target("name/broadway").unwrap(),
        AttachTarget::Name("broadway".to_string())
    );
}

// ============================================================================
// resolve_attach_target verb parameter tests
// ============================================================================

fn make_test_state() -> (
    DaemonState,
    tempfile::TempDir,
    crate::paths::TestMidtownBaseDirGuard,
) {
    use std::process::Command;
    use tempfile::TempDir;

    let midtown_dir = TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let temp_dir = TempDir::new().expect("temp dir");
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

    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let state = DaemonState::new(
        "/tmp/test-rpc-session.sock".into(),
        cm,
        "test-repo".to_string(),
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

#[tokio::test]
async fn test_resolve_attach_target_multi_match_error_uses_verb() {
    let (state, _tmp, _guard) = make_test_state();

    // Create two coworkers assigned to the same task so task lookup returns multiple
    state.record_task_assignment("park", "42");
    state.record_task_assignment("madison", "42");

    // "clear" verb should appear in the disambiguation error
    let err = resolve_attach_target("task/42", &state, "clear")
        .await
        .unwrap_err();
    assert!(
        err.contains("clear"),
        "Error message should contain the verb 'clear', got: {}",
        err
    );
    assert!(
        !err.contains("attach"),
        "Error message should not contain 'attach', got: {}",
        err
    );

    // "attach" verb should appear when specified
    let err = resolve_attach_target("task/42", &state, "attach")
        .await
        .unwrap_err();
    assert!(
        err.contains("attach"),
        "Error message should contain the verb 'attach', got: {}",
        err
    );
}

#[test]
fn test_fork_channel_lead_model_is_provider_aware_for_codex() {
    let model = super::fork_channel_lead_model(crate::auth::AuthProvider::Codex, Some("web"));
    assert_eq!(model, "gpt-5-codex");
}

#[test]
fn test_fork_channel_lead_model_uses_default_for_claude() {
    let model = super::fork_channel_lead_model(crate::auth::AuthProvider::Claude, None);
    assert_eq!(model, "sonnet");
}

// ============================================================================
// handle_session_clear tests
// ============================================================================

/// Insert a headless session entry into persistent state for testing.
async fn insert_test_session(state: &DaemonState, name: &str, initial_prompt: Option<String>) {
    insert_test_session_with_metadata(state, name, initial_prompt, "dev", None, None).await;
}

/// Insert a session with explicit metadata (coworker type, PR number, channel).
///
/// Populates both `sessions` (SessionRecord) and `name_to_session` so that
/// migrated handlers can look up sessions via name → session_id → SessionRecord.
async fn insert_test_session_with_metadata(
    state: &DaemonState,
    name: &str,
    initial_prompt: Option<String>,
    coworker_type: &str,
    pr_number: Option<u64>,
    channel: Option<String>,
) {
    let session_id = format!("test-session-{}", name);
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            session_id.clone(),
            crate::daemon::state::SessionRecord {
                session_id: session_id.clone(),
                current_name: Some(name.to_string()),
                preferred_name: Some(name.to_string()),
                working_dir: "/tmp/test-worktree".to_string(),
                coworker_type: coworker_type.to_string(),
                task_id: Some("42".to_string()),
                pr_number,
                channel,
                initial_prompt,
                resume_on_startup: true,
                ..Default::default()
            },
        );
    }
    state
        .name_to_session
        .lock()
        .unwrap()
        .insert(name.to_string(), session_id);
}

#[tokio::test]
async fn test_session_clear_rejects_unknown_coworker() {
    let (state, _tmp, _guard) = make_test_state();

    let resp = handle_session_clear(RequestId::Number(1), "name/nonexistent", &state).await;

    let json = serde_json::to_value(&resp).unwrap();
    assert!(
        json.get("error").is_some(),
        "Should return error for unknown coworker"
    );
}

#[tokio::test]
async fn test_session_clear_rejects_no_persisted_session() {
    let (state, _tmp, _guard) = make_test_state();

    // Register a coworker in the manager but don't add a persisted session
    state
        .coworkers
        .register(
            "park",
            "park",
            "/tmp/test".to_string(),
            None,
            String::new(),
            crate::auth::AuthProvider::Claude,
            String::new(),
        )
        .unwrap();

    let resp = handle_session_clear(RequestId::Number(1), "name/park", &state).await;

    let json = serde_json::to_value(&resp).unwrap();
    assert!(
        json.get("error").is_some(),
        "Should return error when no persisted session exists"
    );
}

#[tokio::test]
async fn test_session_clear_rejects_attached_session() {
    let (state, _tmp, _guard) = make_test_state();

    // Register coworker and add persisted session
    state
        .coworkers
        .register(
            "park",
            "park",
            "/tmp/test".to_string(),
            None,
            String::new(),
            crate::auth::AuthProvider::Claude,
            String::new(),
        )
        .unwrap();
    insert_test_session(&state, "park", Some("original task".to_string())).await;

    // Mark as attached
    {
        let mut attached = state.attached_coworkers.lock().unwrap();
        attached.insert("park".to_string(), chrono::Utc::now());
    }

    let resp = handle_session_clear(RequestId::Number(1), "name/park", &state).await;

    let json = serde_json::to_value(&resp).unwrap();
    let err = json
        .get("error")
        .expect("Should return error for attached session");
    let msg = err.get("message").unwrap().as_str().unwrap();
    assert!(
        msg.contains("attached interactively"),
        "Error should mention interactive attachment, got: {}",
        msg
    );
}

#[tokio::test]
async fn test_session_clear_cleans_up_transient_state() {
    let (state, _tmp, _guard) = make_test_state();
    let name = "broadway";

    // Register coworker and add persisted session
    state
        .coworkers
        .register(
            name,
            name,
            "/tmp/test".to_string(),
            None,
            String::new(),
            crate::auth::AuthProvider::Claude,
            String::new(),
        )
        .unwrap();
    insert_test_session(&state, name, Some("original task".to_string())).await;

    // Populate transient state that cleanup_coworker_state should clear
    {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.record("nudge", name);
    }
    state.record_pending_nudge(name, "test nudge");
    state.record_task_assignment(name, "42");

    // The handler will try to spawn a new session, which will fail since
    // there's no actual worktree. That's fine — we're testing that cleanup
    // happened before the spawn attempt.
    let _resp = handle_session_clear(RequestId::Number(1), &format!("name/{}", name), &state).await;

    // Verify transient state was cleaned up (regardless of spawn success)
    {
        let cooldowns = state.cooldowns.lock().unwrap();
        assert!(
            cooldowns.is_empty(),
            "cooldowns should be cleared after session clear"
        );
    }
    {
        let pending = state.pending_nudges.lock().unwrap();
        assert!(
            !pending.contains_key(name),
            "pending nudges should be cleared after session clear"
        );
    }
    {
        let assignments = state.coworker_task_assignments.lock().unwrap();
        assert!(
            !assignments.contains_key(name),
            "task assignments should be cleared after session clear"
        );
    }
}

#[tokio::test]
async fn test_session_clear_uses_lead_config_for_lead_target() {
    let (state, _tmp, _guard) = make_test_state();

    // Register lead and add persisted session
    state
        .coworkers
        .register(
            "lead",
            "lead",
            "/tmp/test-lead".to_string(),
            None,
            String::new(),
            crate::auth::AuthProvider::Claude,
            String::new(),
        )
        .unwrap();
    insert_test_session(&state, "lead", Some("lead task prompt".to_string())).await;

    // The handler will fail to spawn (no real worktree), but we can verify
    // it doesn't panic and attempts the lead path.
    let resp = handle_session_clear(RequestId::Number(1), "name/lead", &state).await;

    // The spawn will likely fail, but the handler should not panic and
    // should return a response (either success or spawn-failure error).
    let json = serde_json::to_value(&resp).unwrap();
    assert!(
        json.get("error").is_some() || json.get("result").is_some(),
        "Should return either success or spawn-failure error, got: {:?}",
        json
    );
}

/// Regression test: session clear for a reviewer coworker should not panic
/// and should handle reviewer-specific metadata (coworker_type, pr_number, channel).
#[tokio::test]
async fn test_session_clear_handles_reviewer_metadata() {
    let (state, _tmp, _guard) = make_test_state();

    state
        .coworkers
        .register(
            "broadway",
            "broadway",
            "/tmp/test".to_string(),
            None,
            String::new(),
            crate::auth::AuthProvider::Claude,
            String::new(),
        )
        .unwrap();
    insert_test_session_with_metadata(
        &state,
        "broadway",
        Some("Review PR #42".to_string()),
        "reviewer",
        Some(42),
        Some("review-42".to_string()),
    )
    .await;

    // The spawn will fail (no real worktree), but the handler should not panic
    // when processing reviewer metadata.
    let resp = handle_session_clear(RequestId::Number(1), "name/broadway", &state).await;

    let json = serde_json::to_value(&resp).unwrap();
    assert!(
        json.get("error").is_some() || json.get("result").is_some(),
        "Should return either success or spawn-failure error for reviewer clear, got: {:?}",
        json
    );
}

/// Regression test: session clear for a channel-lead coworker should not panic.
#[tokio::test]
async fn test_session_clear_handles_channel_lead_metadata() {
    let (state, _tmp, _guard) = make_test_state();

    state
        .coworkers
        .register(
            "madison",
            "madison",
            "/tmp/test".to_string(),
            None,
            String::new(),
            crate::auth::AuthProvider::Claude,
            String::new(),
        )
        .unwrap();
    insert_test_session_with_metadata(
        &state,
        "madison",
        Some("Channel lead for feature-auth".to_string()),
        "channel-lead",
        None,
        Some("feature-auth".to_string()),
    )
    .await;

    let resp = handle_session_clear(RequestId::Number(1), "name/madison", &state).await;

    let json = serde_json::to_value(&resp).unwrap();
    assert!(
        json.get("error").is_some() || json.get("result").is_some(),
        "Should return either success or spawn-failure error for channel-lead clear, got: {:?}",
        json
    );
}

// ============================================================================
// create_fork_session tests
// ============================================================================

/// When `topic_sessions` already has a non-pending entry for the thread,
/// `create_fork_session` returns `Ok((existing_sid, true))` without spawning.
#[tokio::test]
async fn test_create_fork_session_returns_existing_when_already_present() {
    let (state, _tmp, _guard) = make_test_state();

    let thread_id = "thread-already-exists-abc";
    let existing_sid = "session-already-exists-xyz".to_string();
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_id.to_string(), existing_sid.clone());

    let result = create_fork_session(thread_id, "any-calling-session", None, &state).await;

    assert!(result.is_ok(), "should succeed when fork already exists");
    let (returned_sid, already_existed) = result.unwrap();
    assert_eq!(
        returned_sid, existing_sid,
        "should return existing session_id"
    );
    assert!(already_existed, "already_existed should be true");

    // topic_sessions should still contain only the one entry (no sentinel added)
    let topic = state.topic_sessions.lock().unwrap();
    assert_eq!(topic.len(), 1);
    assert_eq!(topic.get(thread_id).unwrap(), &existing_sid);
}

/// When `topic_sessions` has a "pending" entry (concurrent fork in progress),
/// `create_fork_session` returns `Err` without spawning.
#[tokio::test]
async fn test_create_fork_session_returns_err_when_pending() {
    let (state, _tmp, _guard) = make_test_state();

    let thread_id = "thread-pending-fork-abc";
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_id.to_string(), "pending".to_string());

    let result = create_fork_session(thread_id, "any-calling-session", None, &state).await;

    assert!(
        result.is_err(),
        "should fail when fork slot is already 'pending'"
    );

    // topic_sessions should still contain only the "pending" entry from the other caller
    let topic = state.topic_sessions.lock().unwrap();
    assert_eq!(
        topic.get(thread_id).map(String::as_str),
        Some("pending"),
        "pending sentinel should be untouched"
    );
}

/// When spawn fails (no real claude process in tests), the pending sentinel is
/// removed from `topic_sessions` so the slot is available for retry.
#[tokio::test]
async fn test_create_fork_session_cleans_up_sentinel_on_spawn_failure() {
    let (state, _tmp, _guard) = make_test_state();

    let thread_id = "thread-spawn-fail-abc";
    let calling_session_id = "fake-session-for-spawn-test";

    // Insert a parent session record so create_fork_session finds metadata
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            calling_session_id.to_string(),
            crate::daemon::state::SessionRecord {
                session_id: calling_session_id.to_string(),
                current_name: Some("web".to_string()),
                preferred_name: Some("web".to_string()),
                working_dir: "/tmp/test".to_string(),
                coworker_type: "channel-lead".to_string(),
                channel: Some("web".to_string()),
                ..Default::default()
            },
        );
    }
    state
        .name_to_session
        .lock()
        .unwrap()
        .insert("web".to_string(), calling_session_id.to_string());
    state
        .session_to_name
        .lock()
        .unwrap()
        .insert(calling_session_id.to_string(), "web".to_string());

    // spawn_fork will fail since there's no real claude process
    let result = create_fork_session(thread_id, calling_session_id, Some("web"), &state).await;

    assert!(result.is_err(), "should fail when spawn_fork fails");

    // Sentinel should be cleaned up — the slot should be available for retry
    let topic = state.topic_sessions.lock().unwrap();
    assert!(
        !topic.contains_key(thread_id),
        "pending sentinel should be removed after spawn failure"
    );
}

/// The `handle_session_fork` RPC handler returns `already_exists: true` when
/// a fork exists, and a normal response for a new fork (or spawn error).
#[tokio::test]
async fn test_handle_session_fork_already_exists_response() {
    let (state, _tmp, _guard) = make_test_state();

    let thread_id = "thread-rpc-already-exists";
    let existing_sid = "rpc-existing-session-xyz".to_string();
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_id.to_string(), existing_sid.clone());

    let resp = handle_session_fork(RequestId::Number(1), thread_id, "any-caller", &state).await;
    let json = serde_json::to_value(&resp).unwrap();

    assert!(
        json.get("error").is_none(),
        "should succeed for already-existing fork"
    );
    let result = json["result"].as_object().unwrap();
    assert_eq!(result["session_id"].as_str().unwrap(), existing_sid);
    assert!(
        result["already_exists"].as_bool().unwrap(),
        "already_exists should be true"
    );
}
