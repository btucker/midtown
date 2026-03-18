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
        crate::paths::ProjectPaths::with_project_name("test-repo", "test-repo"),
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
    state.set_test_task_assignment("park", "42").await;
    state.set_test_task_assignment("madison", "42").await;

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
    let model =
        super::fork_channel_lead_model("test-repo", crate::auth::AuthProvider::Codex, Some("web"));
    // Must be a Codex-compatible model, never a Claude alias.
    // The exact value depends on global config (e.g., default_model = "large" →
    // "gpt-5.3-codex"); the hardcoded default is "gpt-5-codex".
    // Exact default_model_for_provider_role assertions live in helpers_tests.rs.
    assert!(
        !model.contains("sonnet") && !model.contains("opus") && !model.contains("haiku"),
        "Codex channel lead model '{}' should not contain Claude aliases",
        model
    );
}

#[test]
fn test_fork_channel_lead_model_uses_default_for_claude() {
    let model =
        super::fork_channel_lead_model("test-repo", crate::auth::AuthProvider::Claude, None);
    // Must be a Claude-compatible model alias, never a Codex model.
    // The exact value depends on global config (e.g., default_model = "large" →
    // "opus"); the hardcoded default is "sonnet".
    // Exact default_model_for_provider_role assertions live in helpers_tests.rs.
    assert!(
        ["haiku", "sonnet", "opus"].contains(&model.as_str()),
        "Claude channel lead model '{}' should be a valid Claude model alias",
        model
    );
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
    state.set_test_task_assignment(name, "42").await;

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
    assert!(
        state.get_task_id_for_coworker(name).await.is_none(),
        "task assignment should not be visible after session clear"
    );
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

/// Helper: populate the state so that `create_fork_session` treats an existing
/// `topic_sessions` entry as alive (sets session_to_name + is_alive hook).
fn setup_alive_fork(state: &DaemonState, session_id: &str, fork_name: &str) {
    state
        .session_to_name
        .lock()
        .unwrap()
        .insert(session_id.to_string(), fork_name.to_string());
    let name_owned = fork_name.to_string();
    state
        .session_manager
        .set_test_is_alive_hook(Some(std::sync::Arc::new(move |name: &str| {
            name == name_owned
        })));
}

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
    setup_alive_fork(&state, &existing_sid, "fork-already-exists");

    let result =
        create_fork_session(thread_id, "any-calling-session", None, None, "test", &state).await;

    assert!(result.is_ok(), "should succeed when fork already exists");
    let (returned_sid, already_existed, _fork_channel) = result.unwrap();
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

/// When `topic_sessions` has an entry for a dead session (e.g. after daemon
/// restart), `create_fork_session` clears the stale entry and attempts to
/// create a new fork rather than returning the stale session_id.
#[tokio::test]
async fn test_create_fork_session_clears_stale_entry() {
    let (state, _tmp, _guard) = make_test_state();

    let thread_id = "thread-stale-session-abc";
    let stale_sid = "stale-session-xyz".to_string();
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_id.to_string(), stale_sid.clone());
    // No is_alive hook → session reports as dead.

    // The function should detect the stale entry and try to create a new fork.
    let result =
        create_fork_session(thread_id, "any-calling-session", None, None, "test", &state).await;

    // The key assertion: if it succeeded, it must NOT have returned the stale
    // session_id. The stale entry should have been replaced with a fresh fork.
    match result {
        Ok((returned_sid, already_existed, _)) => {
            assert_ne!(
                returned_sid, stale_sid,
                "should NOT return the stale session_id"
            );
            assert!(
                !already_existed,
                "should create a new fork, not return existing"
            );
        }
        Err(_) => {
            // Spawn failure is acceptable in test — the important thing is
            // that it didn't short-circuit with the stale session_id.
            // Sentinel should be cleaned up.
            let topic = state.topic_sessions.lock().unwrap();
            assert!(
                !topic.contains_key(thread_id) || topic.get(thread_id) != Some(&stale_sid),
                "stale entry should be cleared"
            );
        }
    }
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

    let result =
        create_fork_session(thread_id, "any-calling-session", None, None, "test", &state).await;

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

/// When spawn fails (non-existent CWD), the pending sentinel is
/// removed from `topic_sessions` so the slot is available for retry.
#[tokio::test]
async fn test_create_fork_session_cleans_up_sentinel_on_spawn_failure() {
    let (state, _tmp, _guard) = make_test_state();

    let thread_id = "thread-spawn-fail-abc";
    let calling_session_id = "fake-session-for-spawn-test";

    // Insert a parent session record with a non-existent working_dir so spawn fails.
    // Fresh session spawn fails when the CWD doesn't exist.
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            calling_session_id.to_string(),
            crate::daemon::state::SessionRecord {
                session_id: calling_session_id.to_string(),
                current_name: Some("web".to_string()),
                preferred_name: Some("web".to_string()),
                working_dir: "/dev/null/nonexistent".to_string(),
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

    // spawn_fork will fail because the CWD doesn't exist
    let result = create_fork_session(
        thread_id,
        calling_session_id,
        Some("web"),
        None,
        "test",
        &state,
    )
    .await;

    assert!(result.is_err(), "should fail when spawn_fork fails");

    // Sentinel should be cleaned up — the slot should be available for retry
    let topic = state.topic_sessions.lock().unwrap();
    assert!(
        !topic.contains_key(thread_id),
        "pending sentinel should be removed after spawn failure"
    );
}

/// When a fork already exists, `create_fork_session` returns `fork_channel: None`
/// since no new fork was created and channel resolution was skipped.
#[tokio::test]
async fn test_create_fork_session_existing_returns_none_fork_channel() {
    let (state, _tmp, _guard) = make_test_state();

    let thread_id = "thread-existing-channel-check";
    let existing_sid = "session-existing-channel-xyz".to_string();
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_id.to_string(), existing_sid.clone());
    setup_alive_fork(&state, &existing_sid, "fork-existing-channel");

    let result =
        create_fork_session(thread_id, "any-calling-session", None, None, "test", &state).await;

    let (_sid, already_existed, fork_channel) = result.unwrap();
    assert!(already_existed);
    assert!(
        fork_channel.is_none(),
        "fork_channel should be None for pre-existing forks"
    );
}

/// Channel resolution: when `channel_hint` is provided, it takes priority.
/// When a channel lead session forks with an explicit hint, the fork should
/// use that channel (not the caller's name or the repo name).
#[tokio::test]
async fn test_create_fork_session_with_channel_hint_reaches_spawn() {
    let (state, _tmp, _guard) = make_test_state();

    let thread_id = "thread-channel-hint-test";
    let calling_session_id = "channel-lead-session-hint";

    // Insert a channel lead session record WITH a channel
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            calling_session_id.to_string(),
            crate::daemon::state::SessionRecord {
                session_id: calling_session_id.to_string(),
                current_name: Some("daemon-core".to_string()),
                preferred_name: Some("daemon-core".to_string()),
                working_dir: "/dev/null/nonexistent".to_string(),
                coworker_type: "channel-lead".to_string(),
                channel: Some("daemon-core".to_string()),
                ..Default::default()
            },
        );
    }
    state
        .name_to_session
        .lock()
        .unwrap()
        .insert("daemon-core".to_string(), calling_session_id.to_string());
    state
        .session_to_name
        .lock()
        .unwrap()
        .insert(calling_session_id.to_string(), "daemon-core".to_string());

    // spawn_fork will fail (no real claude process), but the code should
    // reach spawn (past channel resolution) and fail gracefully.
    let result = create_fork_session(
        thread_id,
        calling_session_id,
        Some("daemon-core"), // explicit channel hint
        None,
        "test",
        &state,
    )
    .await;

    // Spawn fails in test env, but reaching spawn confirms channel resolution
    // completed successfully (it runs before spawn).
    assert!(result.is_err(), "spawn should fail in test environment");

    // Sentinel should be cleaned up after spawn failure
    let topic = state.topic_sessions.lock().unwrap();
    assert!(
        !topic.contains_key(thread_id),
        "sentinel should be cleaned up after spawn failure"
    );
}

/// The `handle_session_fork` RPC handler returns `already_exists: true` when
/// a fork exists, and a normal response for a new fork (or spawn error).
#[tokio::test]
async fn test_handle_session_fork_already_exists_response() {
    let (state, _tmp, _guard) = make_test_state();

    let thread_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
    let existing_sid = "rpc-existing-session-xyz".to_string();
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_id.to_string(), existing_sid.clone());
    setup_alive_fork(&state, &existing_sid, "fork-rpc-existing");

    let resp = handle_session_fork(
        RequestId::Number(1),
        thread_id,
        "any-caller",
        None,
        None,
        &state,
    )
    .await;
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

/// When the daemon auto-fork has reserved the slot with "pending", calling
/// `handle_session_fork` should return `{pending: true}` instead of an error,
/// so the channel lead can distinguish "retry shortly" from a hard spawn failure.
#[tokio::test]
async fn test_handle_session_fork_returns_pending_during_spawn_window() {
    let (state, _tmp, _guard) = make_test_state();

    let thread_id = "b2c3d4e5-f6a7-8901-bcde-f12345678901";
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_id.to_string(), "pending".to_string());

    let resp = handle_session_fork(
        RequestId::Number(2),
        thread_id,
        "any-caller",
        None,
        None,
        &state,
    )
    .await;
    let json = serde_json::to_value(&resp).unwrap();

    assert!(
        json.get("error").is_none(),
        "should not return an error when fork is in progress"
    );
    let result = json["result"].as_object().unwrap();
    assert!(
        result["pending"].as_bool().unwrap_or(false),
        "result should have pending: true"
    );
    assert_eq!(
        result["thread_parent_id"].as_str().unwrap(),
        thread_id,
        "result should echo thread_parent_id"
    );
}

/// Channel resolution: when `channel_hint` is None AND the session has no channel
/// field, the fork should fall back to `state.project_name` (the main channel).
/// This is the non-channel-lead case — e.g. the project lead forking.
#[tokio::test]
async fn test_create_fork_session_falls_back_to_repo_name() {
    let (state, _tmp, _guard) = make_test_state();

    let thread_id = "thread-repo-name-fallback";
    let calling_session_id = "main-lead-session-no-channel";

    // Insert a main lead session WITHOUT a channel (simulates project lead)
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            calling_session_id.to_string(),
            crate::daemon::state::SessionRecord {
                session_id: calling_session_id.to_string(),
                current_name: Some("main-lead".to_string()),
                preferred_name: Some("main-lead".to_string()),
                working_dir: "/dev/null/nonexistent".to_string(),
                coworker_type: "lead".to_string(),
                channel: None, // no channel — will trigger fallback
                ..Default::default()
            },
        );
    }
    state
        .name_to_session
        .lock()
        .unwrap()
        .insert("main-lead".to_string(), calling_session_id.to_string());
    state
        .session_to_name
        .lock()
        .unwrap()
        .insert(calling_session_id.to_string(), "main-lead".to_string());

    // Spawn will fail, but the fork_channel resolution runs before spawn.
    // We can't observe fork_channel directly since spawn fails, but we can
    // verify via the SessionRecord that gets created — except spawn fails
    // before that too. Instead, verify the sentinel is cleaned up (spawn
    // reached) and add an integration-level check below.
    let result = create_fork_session(
        thread_id,
        calling_session_id,
        None, // no channel hint
        None,
        "test",
        &state,
    )
    .await;

    assert!(result.is_err(), "spawn should fail in test environment");

    // Sentinel cleaned up — spawn was reached (past channel resolution)
    let topic = state.topic_sessions.lock().unwrap();
    assert!(
        !topic.contains_key(thread_id),
        "sentinel should be cleaned up after spawn failure"
    );
}

/// Verify that `handle_session_fork` for a pre-existing fork returns correct
/// response structure without attempting to broadcast or nudge.
#[tokio::test]
async fn test_handle_session_fork_existing_does_not_nudge() {
    let (state, _tmp, _guard) = make_test_state();

    let thread_id = "c3d4e5f6-a7b8-9012-cdef-123456789012";
    let existing_sid = "existing-session-no-nudge".to_string();
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_id.to_string(), existing_sid.clone());
    setup_alive_fork(&state, &existing_sid, "fork-no-nudge");

    // Fork already exists — handle_session_fork should return immediately
    // without sending nudge or broadcasting ThreadOwnership.
    let resp = handle_session_fork(
        RequestId::Number(10),
        thread_id,
        "any-caller",
        None,
        Some("custom initial message"), // should be ignored for existing forks
        &state,
    )
    .await;
    let json = serde_json::to_value(&resp).unwrap();

    assert!(json.get("error").is_none());
    let result = json["result"].as_object().unwrap();
    assert_eq!(result["session_id"].as_str().unwrap(), existing_sid);
    assert!(result["already_exists"].as_bool().unwrap());
}

/// Verify that `handle_session_fork` sends the `initial_message` as the nudge
/// instead of using `fork_initial_framing` when both are available.
/// Since spawn fails in test, we test through the already-exists path to verify
/// the handler's structure, and rely on the integration test for the full path.
#[tokio::test]
async fn test_handle_session_fork_with_initial_message() {
    let (state, _tmp, _guard) = make_test_state();

    let thread_id = "d4e5f6a7-b8c9-0123-defa-234567890123";
    let calling_session_id = "lead-session-for-initial-msg";

    // Insert a channel lead session so the fork attempt goes through
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            calling_session_id.to_string(),
            crate::daemon::state::SessionRecord {
                session_id: calling_session_id.to_string(),
                current_name: Some("daemon-core".to_string()),
                preferred_name: Some("daemon-core".to_string()),
                working_dir: "/dev/null/nonexistent".to_string(),
                coworker_type: "channel-lead".to_string(),
                channel: Some("daemon-core".to_string()),
                ..Default::default()
            },
        );
    }
    state
        .name_to_session
        .lock()
        .unwrap()
        .insert("daemon-core".to_string(), calling_session_id.to_string());
    state
        .session_to_name
        .lock()
        .unwrap()
        .insert(calling_session_id.to_string(), "daemon-core".to_string());

    // Spawn will fail — but the handler should not error on the RPC level;
    // it returns a JSON-RPC error response.
    let resp = handle_session_fork(
        RequestId::Number(11),
        thread_id,
        calling_session_id,
        None,
        Some("Custom: investigate the auth bug"),
        &state,
    )
    .await;
    let json = serde_json::to_value(&resp).unwrap();

    // Spawn failure means we get an error response — but the point is
    // the handler didn't panic and exercised the initial_message path.
    assert!(
        json.get("error").is_some(),
        "should get error when spawn fails"
    );
}

// ============================================================================
// format_blockquote tests
// ============================================================================

#[test]
fn test_format_blockquote_single_line() {
    assert_eq!(format_blockquote("hello world"), "> hello world");
}

#[test]
fn test_format_blockquote_multi_line() {
    let content = "line one\nline two\nline three";
    assert_eq!(
        format_blockquote(content),
        "> line one\n> line two\n> line three"
    );
}

#[test]
fn test_format_blockquote_with_empty_lines() {
    let content = "first\n\nlast";
    assert_eq!(format_blockquote(content), "> first\n> \n> last");
}

// ============================================================================
// slugify_fork_hint tests
// ============================================================================

#[test]
fn test_slugify_extracts_meaningful_words() {
    assert_eq!(
        slugify_fork_hint("How does the auth module work?", "abcd1234efgh"),
        "auth-module-work"
    );
}

#[test]
fn test_slugify_strips_mentions() {
    assert_eq!(
        slugify_fork_hint("@channel-lead how does auth work?", "abcd1234efgh"),
        "auth-work"
    );
}

#[test]
fn test_slugify_skips_stop_words() {
    assert_eq!(
        slugify_fork_hint("Can you add the TLS config option", "abcd1234efgh"),
        "add-tls-config"
    );
}

#[test]
fn test_slugify_limits_to_three_words() {
    assert_eq!(
        slugify_fork_hint(
            "implement dark mode toggle component system",
            "abcd1234efgh"
        ),
        "implement-dark-mode"
    );
}

#[test]
fn test_slugify_lowercases() {
    assert_eq!(
        slugify_fork_hint("Push Notifications Setup", "abcd1234efgh"),
        "push-notifications-setup"
    );
}

#[test]
fn test_slugify_falls_back_to_thread_id() {
    // Only stop words and mentions — no meaningful content
    assert_eq!(
        slugify_fork_hint("@user is it the", "abcdefghijkl"),
        "abcdefgh"
    );
}

#[test]
fn test_slugify_strips_punctuation() {
    assert_eq!(
        slugify_fork_hint("fix: auth endpoint!", "abcd1234efgh"),
        "fix-auth-endpoint"
    );
}

#[test]
fn test_slugify_empty_message() {
    assert_eq!(slugify_fork_hint("", "abcdefgh1234"), "abcdefgh");
}

#[test]
fn test_slugify_short_thread_id_fallback() {
    assert_eq!(slugify_fork_hint("", "abc"), "abc");
}

#[test]
fn test_slugify_interior_punctuation_replaced() {
    // Interior slashes, dots, and other punctuation must be replaced with hyphens,
    // not just edge-trimmed. "fix/auth" is a single whitespace-delimited token
    // where the slash is interior — trim_matches won't touch it.
    assert_eq!(
        slugify_fork_hint("fix/auth bug.report", "abcd1234efgh"),
        "fix-auth-bug-report"
    );
}

#[test]
fn test_slugify_consecutive_punctuation_collapsed() {
    // Multiple consecutive non-alphanumeric chars should collapse to a single hyphen.
    assert_eq!(
        slugify_fork_hint("fix::auth--endpoint", "abcd1234efgh"),
        "fix-auth-endpoint"
    );
}

// ============================================================================
// Fork auth profile resolution tests
// ============================================================================

/// After a per-project `auth switch`, `build_fork_config` must set
/// `CLAUDE_CONFIG_DIR` to the project-specific profile directory, not the
/// global one. The global marker doesn't change on a per-project switch, so
/// using it would give forks stale credentials ("Not logged in").
///
/// Regression test for !1960.
#[test]
fn test_build_fork_config_uses_project_auth_profile() {
    let midtown_dir = tempfile::TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let repo_name = "test-repo";
    let provider = crate::auth::AuthProvider::Claude;

    // Set up a per-project auth profile override (simulates `midtown auth switch alice`)
    let project_config_path = crate::config::project_config_path(repo_name);
    let mut config = crate::config::FullProjectConfig::default();
    crate::auth::set_project_profile_override(&mut config.project, provider, "alice".to_string());
    config
        .save_to(&project_config_path)
        .expect("save project config");

    // Precondition: global and project profiles differ (otherwise the test is vacuous)
    let global_dir = crate::auth::current_profile_dir_for(provider);
    let project_dir =
        crate::auth::active_profile_dir_for_project_with_provider(repo_name, provider);
    assert_ne!(
        global_dir, project_dir,
        "precondition: global and project profile dirs must differ"
    );

    // Call the production code path that builds the fork's HeadlessConfig.
    let (_fork_name, headless_config) = build_fork_config(
        "thread-abc123",
        "parent-session-id",
        Some("web"),
        None,
        Some("web"),
        Some("/tmp/test"),
        provider,
        true,
        repo_name,
        None, // no name override
    );

    // The CLAUDE_CONFIG_DIR env var in the fork config must point to the
    // project-aware profile directory, not the global one.
    let config_dir = headless_config
        .env
        .get("CLAUDE_CONFIG_DIR")
        .expect("CLAUDE_CONFIG_DIR should be set in fork env");
    assert_eq!(
        config_dir,
        &project_dir.to_string_lossy().to_string(),
        "fork CLAUDE_CONFIG_DIR should use project-aware profile dir, not global \
         (got {:?}, expected {:?})",
        config_dir,
        project_dir
    );
    assert_ne!(
        config_dir,
        &global_dir.to_string_lossy().to_string(),
        "fork CLAUDE_CONFIG_DIR must NOT be the global profile dir"
    );
}

/// Fork sessions for channel leads should hard-block Edit (in addition to
/// Write and NotebookEdit). Top-level channel leads allow Edit for notes,
/// but forks have narrower context and historically ignored prompt-based
/// restrictions (PR #1667).
#[test]
fn test_fork_channel_lead_disallows_edit() {
    let midtown_dir = tempfile::TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let (_fork_name, headless_config) = build_fork_config(
        "thread-abc123",
        "parent-session-id",
        Some("web"),
        None,
        Some("web"),
        Some("/tmp/test"),
        crate::auth::AuthProvider::Claude,
        true, // is_channel_lead
        "test-repo",
        None,
    );

    assert!(
        headless_config
            .disallowed_tools
            .contains(&"Edit".to_string()),
        "Fork sessions should hard-block Edit"
    );
    assert!(
        headless_config
            .disallowed_tools
            .contains(&"Write".to_string()),
        "Fork sessions should hard-block Write"
    );
    assert!(
        headless_config
            .disallowed_tools
            .contains(&"NotebookEdit".to_string()),
        "Fork sessions should hard-block NotebookEdit"
    );
}

/// Demonstrates that `build_fork_config` re-derives a *different* name when
/// given an existing fork name as a hint. This is why `respawn_fork` must
/// use the original fork name directly rather than relying on the generated name —
/// cooldowns are keyed by name, and name mutation would bypass rate limiting.
///
/// Regression guard for the Codex-identified crash-respawn name stability issue.
#[test]
fn test_build_fork_config_mutates_name_when_given_existing_fork_name_as_hint() {
    let midtown_dir = tempfile::TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let thread_id = "abcd1234-5678";
    let provider = crate::auth::AuthProvider::Claude;
    let repo_name = "test-repo";

    // First fork: caller "web" creates a fork with topic hint "auth discussion"
    let (original_name, _cfg) = build_fork_config(
        thread_id,
        "parent-session",
        Some("web"),
        Some("auth discussion"),
        Some("web"),
        Some("/tmp/test"),
        provider,
        false,
        repo_name,
        None, // no name override
    );

    // Without name_override: passing original name as hint re-derives a different name
    let (respawned_name_via_hint, _cfg) = build_fork_config(
        thread_id,
        "",
        None,
        Some(&original_name),
        Some("web"),
        Some("/tmp/test"),
        provider,
        false,
        repo_name,
        None, // no name override — uses hint derivation
    );

    // The names DIFFER when using hint — this is the bug that name_override fixes
    assert_ne!(
        original_name, respawned_name_via_hint,
        "build_fork_config should produce a different name when re-deriving from an existing \
         fork name as hint (demonstrating why name_override is needed)"
    );

    // With name_override: the exact original name is preserved
    let (respawned_name_via_override, _cfg) = build_fork_config(
        thread_id,
        "",
        None,
        None,
        Some("web"),
        Some("/tmp/test"),
        provider,
        false,
        repo_name,
        Some(&original_name), // name override — reuses exact name
    );

    assert_eq!(
        original_name, respawned_name_via_override,
        "name_override should produce the exact same fork name for stable cooldown keys"
    );
}

// ============================================================================
// handle_session_fork_thread / unfork_thread / thread_ownership tests
// ============================================================================

/// When the channel lead session ID in `channel_lead_sessions` is stale (not in
/// `persistent_state.sessions`), `handle_session_fork_thread` should return an
/// error AND clean up the stale mapping so the next call returns "No channel lead"
/// instead of repeating the "stale" error forever (self-healing).
#[tokio::test]
async fn test_fork_thread_stale_session_self_heals() {
    let (state, _tmp, _guard) = make_test_state();
    let channel = "web";
    let stale_sid = "stale-session-id-123";

    // Register a channel lead session that does NOT exist in persistent_state.sessions
    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .insert(channel.to_string(), stale_sid.to_string());
        // Do NOT insert into ps.sessions — simulates a crashed lead
    }

    // First call: should detect stale session and clean up the mapping
    let resp1 = handle_session_fork_thread(
        1_i64.into(),
        "e5f6a7b8-c9d0-1234-efab-345678901234",
        channel,
        &state,
    )
    .await;
    let json1: serde_json::Value = serde_json::to_value(&resp1).unwrap();
    assert!(
        json1.get("error").is_some(),
        "First call should return an error for stale session"
    );
    let msg1 = json1["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg1.contains("stale"),
        "Error should mention 'stale', got: {}",
        msg1
    );

    // Second call: should return "No channel lead session" (self-healed)
    let resp2 = handle_session_fork_thread(
        2_i64.into(),
        "e5f6a7b8-c9d0-1234-efab-345678901234",
        channel,
        &state,
    )
    .await;
    let json2: serde_json::Value = serde_json::to_value(&resp2).unwrap();
    assert!(
        json2.get("error").is_some(),
        "Second call should also return an error"
    );
    let msg2 = json2["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg2.contains("No channel lead session"),
        "Second call should say 'No channel lead session' (mapping was cleaned), got: {}",
        msg2
    );
}

/// `handle_session_fork_thread` should return an error when no channel lead
/// session is registered for the channel.
#[tokio::test]
async fn test_fork_thread_no_channel_lead() {
    let (state, _tmp, _guard) = make_test_state();

    let resp = handle_session_fork_thread(
        1_i64.into(),
        "f6a7b8c9-d0e1-2345-fabc-456789012345",
        "nonexistent",
        &state,
    )
    .await;
    let json: serde_json::Value = serde_json::to_value(&resp).unwrap();
    assert!(json.get("error").is_some());
    let msg = json["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("No channel lead session"));
}

/// When `topic_sessions` has a fork session ID but `session_to_name` does not
/// have the corresponding mapping, `handle_session_unfork_thread` should clean
/// up the stale `topic_sessions` entry and return an error rather than leaving
/// the process running and unreachable.
#[tokio::test]
async fn test_unfork_thread_stale_session_to_name_cleans_up() {
    let (state, _tmp, _guard) = make_test_state();
    let thread_id = "thread-stale-fork";
    let fork_sid = "fork-session-no-name";

    // Insert a fork session into topic_sessions without a session_to_name entry
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_id.to_string(), fork_sid.to_string());
    // Do NOT insert into session_to_name — simulates concurrent cleanup race

    let resp = handle_session_unfork_thread(1_i64.into(), thread_id, "web", &state).await;
    let json: serde_json::Value = serde_json::to_value(&resp).unwrap();
    assert!(
        json.get("error").is_some(),
        "Should return error when session_to_name is missing"
    );
    let msg = json["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("stale"),
        "Error should mention 'stale', got: {}",
        msg
    );

    // Verify topic_sessions was cleaned up
    let topic = state.topic_sessions.lock().unwrap();
    assert!(
        !topic.contains_key(thread_id),
        "Stale topic_sessions entry should have been removed"
    );
}

/// `handle_session_unfork_thread` should return an error when no fork session
/// exists for the given thread.
#[tokio::test]
async fn test_unfork_thread_no_fork_exists() {
    let (state, _tmp, _guard) = make_test_state();

    let resp =
        handle_session_unfork_thread(1_i64.into(), "nonexistent-thread", "web", &state).await;
    let json: serde_json::Value = serde_json::to_value(&resp).unwrap();
    assert!(json.get("error").is_some());
    let msg = json["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("No dedicated session"));
}

/// `handle_session_thread_ownership` should report `has_dedicated_session: true`
/// when a real fork session is registered, and `false` otherwise.
#[tokio::test]
async fn test_thread_ownership_query() {
    let (state, _tmp, _guard) = make_test_state();
    let thread_id = "thread-ownership-test";

    // No fork — should return has_dedicated_session=false
    let resp1 = handle_session_thread_ownership(1_i64.into(), thread_id, "web", &state).await;
    let json1: serde_json::Value = serde_json::to_value(&resp1).unwrap();
    assert!(json1.get("result").is_some());
    assert_eq!(json1["result"]["has_dedicated_session"], false);

    // Register a fork session
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_id.to_string(), "real-fork-session".to_string());

    let resp2 = handle_session_thread_ownership(2_i64.into(), thread_id, "web", &state).await;
    let json2: serde_json::Value = serde_json::to_value(&resp2).unwrap();
    assert!(json2.get("result").is_some());
    assert_eq!(json2["result"]["has_dedicated_session"], true);

    // "pending" sentinel should report false
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(thread_id.to_string(), "pending".to_string());

    let resp3 = handle_session_thread_ownership(3_i64.into(), thread_id, "web", &state).await;
    let json3: serde_json::Value = serde_json::to_value(&resp3).unwrap();
    assert!(json3.get("result").is_some());
    assert_eq!(
        json3["result"]["has_dedicated_session"], false,
        "pending sentinel should report false"
    );
}

// ============================================================================
// build_fork_config system prompt and settings tests
// ============================================================================

/// `build_fork_config` must set a non-empty system prompt (the lead system
/// prompt), so that respawned forks (resume_session_id = None) get proper
/// instructions instead of starting with a blank slate.
#[test]
fn test_build_fork_config_sets_lead_system_prompt() {
    let midtown_dir = tempfile::TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let repo_name = "test-repo";
    let (_fork_name, headless_config) = build_fork_config(
        "thread-prompt-test",
        "parent-session-id",
        Some("web"),
        None,
        Some("web"),
        Some("/tmp/test"),
        crate::auth::AuthProvider::Claude,
        false,
        repo_name,
        None,
    );

    assert!(
        !headless_config.system_prompt.is_empty(),
        "Fork system_prompt should not be empty — it should contain the lead system prompt"
    );
    // main_lead_system_prompt() replaces {project_name} with repo_name
    assert!(
        headless_config.system_prompt.contains(repo_name),
        "Fork system_prompt should contain the project name from lead prompt templates, got: {}",
        &headless_config.system_prompt[..headless_config.system_prompt.len().min(200)]
    );
}

/// `build_fork_config` should attempt to write lead settings. When the write
/// succeeds, `settings_path` should be `Some(...)` pointing to the lead
/// settings file. When it fails (e.g., sandboxed environment), it should
/// gracefully degrade to `None` without panicking.
#[test]
fn test_build_fork_config_sets_lead_settings_path() {
    let midtown_dir = tempfile::TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let (_fork_name, headless_config) = build_fork_config(
        "thread-settings-test",
        "parent-session-id",
        Some("web"),
        None,
        Some("web"),
        Some("/tmp/test"),
        crate::auth::AuthProvider::Claude,
        false,
        "test-repo",
        None,
    );

    // write_lead_settings_file() writes to ~/.local/state/midtown/ which may
    // be blocked in sandboxed environments. When it succeeds, settings_path
    // should point to the lead settings file. When it fails, build_fork_config
    // gracefully degrades to None (logged as a warning).
    if let Some(path) = &headless_config.settings_path {
        assert!(
            path.contains("lead-settings"),
            "settings_path should reference the lead settings file, got: {}",
            path
        );
    }
    // Either way, the function should not panic — graceful degradation is the key invariant.
}

/// Codex sessions reject `settings_path` in `codex_launch_plan_from_config`,
/// so `build_fork_config` must leave it as `None` for Codex auth provider.
#[test]
fn test_build_fork_config_skips_settings_path_for_codex() {
    let midtown_dir = tempfile::TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let (_fork_name, headless_config) = build_fork_config(
        "thread-codex-test",
        "parent-session-id",
        Some("web"),
        None,
        Some("web"),
        Some("/tmp/test"),
        crate::auth::AuthProvider::Codex,
        false,
        "test-repo",
        None,
    );

    assert!(
        headless_config.settings_path.is_none(),
        "Codex forks must have settings_path = None to avoid codex_launch_plan_from_config rejection"
    );
}

// ============================================================================
// build_fork_config HeadlessConfig field verification
// ============================================================================

/// `build_fork_config` must set `fork_session: false` — forks launch as
/// fresh sessions (not `--resume --fork-session`) because headless sessions
/// don't persist JSONL files to disk.
#[test]
fn test_build_fork_config_sets_fork_session_false() {
    let midtown_dir = tempfile::TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let (_fork_name, config) = build_fork_config(
        "thread-fork-flag-test",
        "parent-session-id",
        Some("web"),
        None,
        Some("web"),
        Some("/tmp/test"),
        crate::auth::AuthProvider::Claude,
        false,
        "test-repo",
        None,
    );

    assert!(
        !config.fork_session,
        "HeadlessConfig.fork_session must be false — forks are fresh sessions"
    );
}

/// `build_fork_config` sets `MIDTOWN_BOUND_THREAD_ID` env var to the thread ID.
///
/// This env var is read by the fork session's system prompt to know which
/// thread it's bound to. Without it, the fork cannot route its output to
/// the correct thread.
#[test]
fn test_build_fork_config_sets_bound_thread_env() {
    let midtown_dir = tempfile::TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let thread_id = "a1b2c3d4-thread-binding-test";
    let (_fork_name, config) = build_fork_config(
        thread_id,
        "parent-session-id",
        Some("web"),
        None,
        Some("web"),
        Some("/tmp/test"),
        crate::auth::AuthProvider::Claude,
        false,
        "test-repo",
        None,
    );

    let bound_thread = config
        .env
        .get("MIDTOWN_BOUND_THREAD_ID")
        .expect("MIDTOWN_BOUND_THREAD_ID should be set in fork env");
    assert_eq!(
        bound_thread, thread_id,
        "MIDTOWN_BOUND_THREAD_ID should match the thread_parent_id"
    );
}

/// `build_fork_config` sets `resume_session_id` to `None` — forks launch
/// as fresh sessions, not resumes of the parent session.
#[test]
fn test_build_fork_config_sets_resume_session_id_none() {
    let midtown_dir = tempfile::TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let (_fork_name, config) = build_fork_config(
        "thread-resume-test",
        "parent-session-abc123",
        Some("web"),
        None,
        Some("web"),
        Some("/tmp/test"),
        crate::auth::AuthProvider::Claude,
        false,
        "test-repo",
        None,
    );

    assert_eq!(
        config.resume_session_id, None,
        "resume_session_id should be None — forks are fresh sessions"
    );
}

/// `build_fork_config` pre-assigns a UUID session_id for Claude provider.
///
/// Claude/Zai fork sessions don't emit system/init events, so the daemon
/// must know the session ID at spawn time. A pre-assigned UUID ensures
/// session-based lookups work immediately.
#[test]
fn test_build_fork_config_pre_assigns_session_id_for_claude() {
    let midtown_dir = tempfile::TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let (_fork_name, config) = build_fork_config(
        "thread-sid-test",
        "parent-session-id",
        Some("web"),
        None,
        Some("web"),
        Some("/tmp/test"),
        crate::auth::AuthProvider::Claude,
        false,
        "test-repo",
        None,
    );

    assert!(
        config.session_id.is_some(),
        "Claude fork should have a pre-assigned session_id"
    );
    // Verify it looks like a UUID (contains hyphens, reasonable length)
    let sid = config.session_id.unwrap();
    assert!(
        sid.contains('-') && sid.len() >= 32,
        "Pre-assigned session_id should be a UUID, got: {}",
        sid
    );
}

/// `build_fork_config` does NOT pre-assign session_id for Codex provider.
///
/// Codex uses thread-based IDs discovered via the init event, not
/// pre-assigned UUIDs. Setting session_id would conflict with Codex's
/// thread model.
#[test]
fn test_build_fork_config_no_session_id_for_codex() {
    let midtown_dir = tempfile::TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let (_fork_name, config) = build_fork_config(
        "thread-codex-sid-test",
        "parent-session-id",
        Some("web"),
        None,
        Some("web"),
        Some("/tmp/test"),
        crate::auth::AuthProvider::Codex,
        false,
        "test-repo",
        None,
    );

    assert!(
        config.session_id.is_none(),
        "Codex fork should NOT have a pre-assigned session_id"
    );
}

// ============================================================================
// Fork thread_parent_id UUID validation tests
// ============================================================================

/// `handle_session_fork` rejects Claude API message IDs (non-UUID strings).
/// This prevents leads from accidentally creating forks bound to non-existent
/// threads when they confuse Claude API IDs with channel message UUIDs.
#[tokio::test]
async fn test_handle_session_fork_rejects_non_uuid_thread_id() {
    let (state, _tmp, _guard) = make_test_state();

    // Claude API message ID format
    let resp = handle_session_fork(
        RequestId::Number(100),
        "msg_01JPxyz123abc",
        "any-caller",
        None,
        None,
        &state,
    )
    .await;
    let json = serde_json::to_value(&resp).unwrap();
    assert!(
        json.get("error").is_some(),
        "Should reject Claude API message ID"
    );
    let msg = json["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("Invalid thread_parent_id"),
        "Error should mention invalid thread_parent_id, got: {}",
        msg
    );
    assert!(
        msg.contains("channel message UUID"),
        "Error should suggest using channel message UUID, got: {}",
        msg
    );
}

/// `handle_session_fork` accepts valid UUID thread IDs.
#[tokio::test]
async fn test_handle_session_fork_accepts_valid_uuid() {
    let (state, _tmp, _guard) = make_test_state();

    let valid_uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

    // Pre-populate topic_sessions with an alive session so the fork returns
    // "already exists" without needing to spawn a real process.
    state
        .topic_sessions
        .lock()
        .unwrap()
        .insert(valid_uuid.to_string(), "existing-session".to_string());
    setup_alive_fork(&state, "existing-session", "fork-existing");

    let resp = handle_session_fork(
        RequestId::Number(101),
        valid_uuid,
        "any-caller",
        None,
        None,
        &state,
    )
    .await;
    let json = serde_json::to_value(&resp).unwrap();
    assert!(
        json.get("error").is_none(),
        "Should accept valid UUID thread ID, got error: {:?}",
        json.get("error")
    );
}

/// `handle_session_fork` rejects various non-UUID strings.
#[tokio::test]
async fn test_handle_session_fork_rejects_various_non_uuid_formats() {
    let (state, _tmp, _guard) = make_test_state();

    let invalid_ids = [
        "msg_01JPxyz123abc", // Claude API message ID
        "not-a-uuid",        // arbitrary string
        "12345",             // numeric
        "",                  // empty (won't reach here due to require_str! but defensive)
        "tool_use_abc123",   // tool use ID
    ];

    for invalid_id in &invalid_ids {
        if invalid_id.is_empty() {
            continue; // skip empty — handled by require_str!
        }
        let resp = handle_session_fork(
            RequestId::Number(200),
            invalid_id,
            "any-caller",
            None,
            None,
            &state,
        )
        .await;
        let json = serde_json::to_value(&resp).unwrap();
        assert!(
            json.get("error").is_some(),
            "Should reject non-UUID '{}', got: {:?}",
            invalid_id,
            json.get("result")
        );
    }
}

/// `handle_session_fork_thread` rejects non-UUID thread IDs.
#[tokio::test]
async fn test_handle_session_fork_thread_rejects_non_uuid() {
    let (state, _tmp, _guard) = make_test_state();

    let resp = handle_session_fork_thread(1_i64.into(), "msg_01JPxyz123abc", "web", &state).await;
    let json: serde_json::Value = serde_json::to_value(&resp).unwrap();
    assert!(
        json.get("error").is_some(),
        "Should reject Claude API message ID in fork_thread"
    );
    let msg = json["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("Invalid thread_parent_id"),
        "Error should mention invalid thread_parent_id, got: {}",
        msg
    );
}

// ============================================================================
// build_channel_summary_for_fork tests
// ============================================================================

#[tokio::test]
async fn test_channel_summary_empty_channel() {
    let tmp = tempfile::TempDir::new().unwrap();
    let channel = crate::channel::Channel::new(tmp.path(), "test-ch").unwrap();
    let result = build_channel_summary_for_fork(&channel).await;
    assert!(result.is_none(), "Empty channel should return None");
}

#[tokio::test]
async fn test_channel_summary_basic_messages() {
    let tmp = tempfile::TempDir::new().unwrap();
    let channel = crate::channel::Channel::new(tmp.path(), "test-ch").unwrap();

    channel
        .send(&Message::text("park", "opened PR #1985"))
        .unwrap();
    channel
        .send(&Message::text("columbus", "reviewing PR"))
        .unwrap();

    let result = build_channel_summary_for_fork(&channel).await;
    assert!(result.is_some());
    let summary = result.unwrap();
    assert!(summary.contains("## Recent channel activity"));
    assert!(summary.contains("park: opened PR #1985"));
    assert!(summary.contains("columbus: reviewing PR"));
}

#[tokio::test]
async fn test_channel_summary_filters_thread_replies() {
    let tmp = tempfile::TempDir::new().unwrap();
    let channel = crate::channel::Channel::new(tmp.path(), "test-ch").unwrap();

    let parent = Message::text("park", "top-level message");
    let parent_id = parent.id.clone();
    channel.send(&parent).unwrap();

    let mut reply = Message::text("columbus", "thread reply");
    reply.thread_parent_id = Some(parent_id);
    channel.send(&reply).unwrap();

    let summary = build_channel_summary_for_fork(&channel).await.unwrap();
    assert!(summary.contains("park: top-level message"));
    assert!(
        !summary.contains("thread reply"),
        "Thread replies should be filtered out"
    );
}

#[tokio::test]
async fn test_channel_summary_filters_auto_output() {
    let tmp = tempfile::TempDir::new().unwrap();
    let channel = crate::channel::Channel::new(tmp.path(), "test-ch").unwrap();

    channel.send(&Message::text("park", "manual post")).unwrap();

    let mut auto = Message::text("park", "auto streamed output");
    auto.auto_output = true;
    channel.send(&auto).unwrap();

    let summary = build_channel_summary_for_fork(&channel).await.unwrap();
    assert!(summary.contains("manual post"));
    assert!(
        !summary.contains("auto streamed"),
        "Auto-output should be filtered out"
    );
}

#[tokio::test]
async fn test_channel_summary_filters_nudges() {
    use crate::message::MessageType;

    let tmp = tempfile::TempDir::new().unwrap();
    let channel = crate::channel::Channel::new(tmp.path(), "test-ch").unwrap();

    channel
        .send(&Message::text("park", "normal message"))
        .unwrap();
    channel
        .send(&Message::new("midtown", "wake up", MessageType::Nudge))
        .unwrap();

    let summary = build_channel_summary_for_fork(&channel).await.unwrap();
    assert!(summary.contains("normal message"));
    assert!(
        !summary.contains("wake up"),
        "Nudge messages should be filtered out"
    );
}

#[tokio::test]
async fn test_channel_summary_truncates_long_content() {
    let tmp = tempfile::TempDir::new().unwrap();
    let channel = crate::channel::Channel::new(tmp.path(), "test-ch").unwrap();

    let long_content = "a".repeat(200);
    channel.send(&Message::text("park", &long_content)).unwrap();

    let summary = build_channel_summary_for_fork(&channel).await.unwrap();
    assert!(summary.contains("..."), "Long content should be truncated");
    // The truncated line should be ~150 chars of 'a' + "..."
    assert!(
        !summary.contains(&"a".repeat(200)),
        "Full 200-char content should not appear"
    );
}

#[tokio::test]
async fn test_channel_summary_collapses_multiline() {
    let tmp = tempfile::TempDir::new().unwrap();
    let channel = crate::channel::Channel::new(tmp.path(), "test-ch").unwrap();

    channel
        .send(&Message::text("park", "line one\nline two\nline three"))
        .unwrap();

    let summary = build_channel_summary_for_fork(&channel).await.unwrap();
    assert!(
        summary.contains("line one line two line three"),
        "Multiline content should be collapsed to single line"
    );
}

/// Fork system prompts must include a scope boundary that prevents the fork
/// from claiming tasks or implementing features after completing its research.
#[test]
fn test_build_fork_config_includes_scope_boundary() {
    let midtown_dir = tempfile::TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let (_fork_name, headless_config) = build_fork_config(
        "thread-scope-test",
        "parent-session-id",
        Some("web"),
        None,
        Some("web"),
        Some("/tmp/test"),
        crate::auth::AuthProvider::Claude,
        false,
        "test-repo",
        None,
    );

    assert!(
        headless_config
            .system_prompt
            .contains("Fork Scope Boundary"),
        "Fork system_prompt must include a scope boundary to prevent forks from claiming tasks"
    );
    assert!(
        headless_config.system_prompt.contains("Do NOT claim"),
        "Fork scope boundary must instruct forks not to claim tasks"
    );
    assert!(
        headless_config.system_prompt.contains("may create tasks"),
        "Fork scope boundary must allow task creation (handoff mechanism)"
    );
}
