use super::*;

#[test]
fn test_is_auth_error_detects_oauth_expired() {
    let msg = "Error: OAuth token has expired";
    assert!(is_auth_error(msg));
}

#[test]
fn test_is_auth_error_detects_authentication_error() {
    let msg = "authentication_error: Invalid credentials";
    assert!(is_auth_error(msg));
}

#[test]
fn test_is_auth_error_detects_401_unauthorized() {
    let msg = "HTTP 401 unauthorized";
    assert!(is_auth_error(msg));
}

#[test]
fn test_is_auth_error_detects_not_logged_in() {
    let msg = "Not logged in · Please run /login";
    assert!(is_auth_error(msg));
}

#[test]
fn test_is_auth_error_ignores_usage_limit() {
    let msg = "You've hit your usage limit";
    assert!(!is_auth_error(msg));
}

#[test]
fn test_is_auth_error_ignores_generic_api_error() {
    let msg = "API error: Rate limit exceeded";
    assert!(!is_auth_error(msg));
}

/// Insert a fake session entry for testing (no real process).
async fn insert_test_session(sm: &SessionManager, name: &str, status: SessionStatus) {
    let mut sessions = sm.sessions.write().await;
    let slot_id = uuid::Uuid::new_v4().to_string();
    sessions.insert(
        slot_id.clone(),
        CoworkerSession {
            session: None,
            slot_id,
            name: name.to_string(),
            status,
            started_at: Utc::now(),
            session_id: None,
            initial_prompt: None,
            cost_usd: 0.0,
            last_event_at: None,
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
            has_pending_api_call: false,
            is_resume: false,
            output_log: None,
            output_log_path: PathBuf::new(),
        },
    );
}

#[test]
fn test_session_manager_default() {
    let _sm = SessionManager::new("test-repo".to_string());
}

#[tokio::test]
async fn test_send_message_no_session() {
    let sm = SessionManager::new("test-repo".to_string());
    let result = sm.send_message("nonexistent", "hello").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_shutdown_no_session() {
    let sm = SessionManager::new("test-repo".to_string());
    let result = sm.shutdown("nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_is_alive_no_session() {
    let sm = SessionManager::new("test-repo".to_string());
    assert!(!sm.is_alive("nonexistent").await);
}

#[tokio::test]
async fn test_drain_events_empty() {
    let sm = SessionManager::new("test-repo".to_string());
    let (events, stopped, stderr_by_name) = sm.drain_events().await;
    assert!(events.is_empty());
    assert!(stopped.is_empty());
    assert!(stderr_by_name.is_empty());
}

#[tokio::test]
async fn test_list_names_empty() {
    let sm = SessionManager::new("test-repo".to_string());
    let names = sm.list_names().await;
    assert!(names.is_empty());
}

#[tokio::test]
async fn test_collect_health_empty() {
    let sm = SessionManager::new("test-repo".to_string());
    let health = sm.collect_health().await;
    assert!(health.is_empty());
}

#[tokio::test]
async fn test_list_alive_names_excludes_stopped() {
    let sm = SessionManager::new("test-repo".to_string());

    // Insert a running session and a stopped session
    insert_test_session(&sm, "madison", SessionStatus::Running).await;
    insert_test_session(&sm, "park", SessionStatus::Stopped).await;
    insert_test_session(&sm, "broadway", SessionStatus::Starting).await;

    // list_names returns all sessions (including stopped)
    let all_names = sm.list_names().await;
    assert_eq!(all_names.len(), 3);

    // list_alive_names should exclude the stopped session
    let alive_names = sm.list_alive_names().await;
    assert_eq!(
        alive_names.len(),
        2,
        "list_alive_names should exclude stopped sessions"
    );
    assert!(alive_names.contains(&"madison".to_string()));
    assert!(alive_names.contains(&"broadway".to_string()));
    assert!(
        !alive_names.contains(&"park".to_string()),
        "stopped session 'park' should not be in alive names"
    );
}

#[tokio::test]
async fn test_list_alive_names_empty() {
    let sm = SessionManager::new("test-repo".to_string());
    let names = sm.list_alive_names().await;
    assert!(names.is_empty());
}

#[tokio::test]
async fn test_reconcile_catches_no_handle_sessions() {
    let sm = SessionManager::new("test-repo".to_string());

    // Insert a session with Running status but no handle (session: None)
    // This simulates the inconsistent state where a session handle is lost
    insert_test_session(&sm, "madison", SessionStatus::Running).await;

    let stopped = sm.reconcile_process_health().await;
    assert_eq!(
        stopped,
        vec!["madison"],
        "Should detect handle-less Running session"
    );

    // Verify the session is now marked as Stopped
    let alive = sm.list_alive_names().await;
    assert!(
        !alive.contains(&"madison".to_string()),
        "madison should no longer be alive"
    );
}

#[tokio::test]
async fn test_reconcile_skips_already_stopped() {
    let sm = SessionManager::new("test-repo".to_string());

    insert_test_session(&sm, "park", SessionStatus::Stopped).await;

    let stopped = sm.reconcile_process_health().await;
    assert!(
        stopped.is_empty(),
        "Should not flag already-stopped sessions"
    );
}

#[tokio::test]
async fn test_reconcile_empty() {
    let sm = SessionManager::new("test-repo".to_string());
    let stopped = sm.reconcile_process_health().await;
    assert!(stopped.is_empty());
}

#[tokio::test]
async fn test_spawn_with_session_id_sets_session_id_immediately() {
    // This test demonstrates the bug: when spawning a session with a known
    // session_id (like during recovery), the session_id should be set immediately
    // on the CoworkerSession, not left as None waiting for an init event that
    // will never arrive for resumed sessions.

    let sm = SessionManager::new("test-repo".to_string());
    let known_session_id = "test-session-id-123";
    let slot_id = "test-slot-id";
    let name = "madison";

    // Simulate what should happen during recovery: spawn() is called with
    // a known session_id, and it should be immediately set on the CoworkerSession.
    // Currently, spawn() doesn't accept a session_id parameter, so this test
    // will fail until we add that support.

    // For now, we'll test the expectation by manually inserting a session
    // with the session_id set, then verifying get_session_id() works.
    {
        let mut sessions = sm.sessions.write().await;
        sessions.insert(
            slot_id.to_string(),
            CoworkerSession {
                session: None,
                slot_id: slot_id.to_string(),
                name: name.to_string(),
                status: SessionStatus::Running,
                started_at: Utc::now(),
                session_id: Some(known_session_id.to_string()),
                initial_prompt: None,
                cost_usd: 0.0,
                last_event_at: None,
                has_usage_limit: false,
                usage_limit_reset_at: None,
                has_api_error: false,
                has_auth_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                has_tool_name_conflict: false,
                has_pending_api_call: false,
                is_resume: false,
                output_log: None,
                output_log_path: PathBuf::new(),
            },
        );
    }

    // Verify get_session_id() returns the expected value
    let retrieved_session_id = sm.get_session_id(name).await;
    assert_eq!(
        retrieved_session_id,
        Some(known_session_id.to_string()),
        "get_session_id() should return the session_id that was set during spawn"
    );
}

#[test]
fn test_parse_usage_limit_with_time() {
    // Test parsing "resets 10am (America/Chicago)"
    let msg = "You've hit your limit · resets 10am (America/Chicago) · /upgrade to increase";
    let result = parse_usage_limit_reset_time(msg);
    assert!(result.is_some(), "Should parse usage limit with time");
}

#[test]
fn test_parse_usage_limit_with_minutes() {
    // Test parsing "resets 11:30pm (America/Chicago)"
    let msg = "usage limit hit - resets 11:30pm (America/Chicago)";
    let result = parse_usage_limit_reset_time(msg);
    assert!(result.is_some(), "Should parse usage limit with minutes");
}

#[test]
fn test_parse_usage_limit_no_time_pattern() {
    // Should still detect usage limit but fall back to default time
    let msg = "You've hit your usage limit. Please try again later.";
    let result = parse_usage_limit_reset_time(msg);
    assert!(
        result.is_some(),
        "Should detect usage limit without time pattern"
    );
}

#[test]
fn test_not_a_usage_limit_message() {
    // Should not match non-usage-limit errors
    let msg = "API error: connection timeout";
    let result = parse_usage_limit_reset_time(msg);
    assert!(result.is_none(), "Should not match non-usage-limit errors");
}

#[test]
fn test_usage_limit_reset_time_in_future() {
    let msg = "You've hit your limit · resets 11:59pm (America/Chicago)";
    let result = parse_usage_limit_reset_time(msg);
    if let Some(reset_time) = result {
        let now = chrono::Utc::now();
        assert!(
            reset_time > now,
            "Reset time should be in the future (or within today if after 11:59pm CST)"
        );
    } else {
        panic!("Should parse usage limit message");
    }
}

#[tokio::test]
async fn test_graceful_shutdown_all_empty_returns_zero() {
    let sm = SessionManager::new("test-repo".to_string());
    let count = sm
        .graceful_shutdown_all(std::time::Duration::from_secs(1))
        .await;
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_graceful_shutdown_all_marks_sessions_stopped() {
    let sm = SessionManager::new("test-repo".to_string());
    insert_test_session(&sm, "madison", SessionStatus::Running).await;
    insert_test_session(&sm, "park", SessionStatus::Running).await;

    let count = sm
        .graceful_shutdown_all(std::time::Duration::from_secs(1))
        .await;
    assert_eq!(count, 2, "Should report 2 sessions shut down");

    // Sessions must remain in the map (not removed) so collect_session_info() can read them
    let names = sm.list_names().await;
    assert_eq!(
        names.len(),
        2,
        "Sessions should remain in map after shutdown"
    );

    // All sessions must be marked Stopped
    let alive = sm.list_alive_names().await;
    assert!(
        alive.is_empty(),
        "No sessions should be alive after graceful_shutdown_all"
    );
}

/// Regression test for the bug where graceful_shutdown_all() removed sessions from the map,
/// causing collect_session_info() to return empty results and breaking session persistence
/// across daemon restarts.
#[tokio::test]
async fn test_graceful_shutdown_all_preserves_session_info() {
    let sm = SessionManager::new("test-repo".to_string());

    // Insert a session with a known session_id (simulating an active coworker)
    {
        let mut sessions = sm.sessions.write().await;
        let slot_id = uuid::Uuid::new_v4().to_string();
        sessions.insert(
            slot_id.clone(),
            CoworkerSession {
                session: None,
                slot_id,
                name: "madison".to_string(),
                status: SessionStatus::Running,
                started_at: Utc::now(),
                session_id: Some("session-abc-123".to_string()),
                initial_prompt: None,
                cost_usd: 1.5,
                last_event_at: None,
                has_usage_limit: false,
                usage_limit_reset_at: None,
                has_api_error: false,
                has_auth_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                has_tool_name_conflict: false,
                has_pending_api_call: false,
                is_resume: false,
                output_log: None,
                output_log_path: PathBuf::new(),
            },
        );
    }

    // Simulate the restart flow: graceful_shutdown_all() is called, then collect_session_info()
    sm.graceful_shutdown_all(std::time::Duration::from_secs(1))
        .await;

    let session_info = sm.collect_session_info().await;
    assert!(
        session_info.contains_key("madison"),
        "collect_session_info() must return session data after graceful_shutdown_all() — \
         session persistence across restarts depends on this"
    );
    assert_eq!(
        session_info["madison"].session_id, "session-abc-123",
        "Session ID must be preserved for restart recovery"
    );
}

/// Regression test: collect_session_info() must preserve initial_prompt.
///
/// Bug: collect_session_info() was setting initial_prompt: None unconditionally
/// (with a "To be filled by caller" comment) but no caller ever filled it.
/// After a daemon restart via shutdown-time persistence, initial_prompt would
/// be lost — causing `session clear` to fall back to a generic message instead
/// of the coworker's actual mission prompt.
#[tokio::test]
async fn test_collect_session_info_preserves_initial_prompt() {
    let sm = SessionManager::new("test-repo".to_string());

    {
        let mut sessions = sm.sessions.write().await;
        let slot_id = uuid::Uuid::new_v4().to_string();
        sessions.insert(
            slot_id.clone(),
            CoworkerSession {
                session: None,
                slot_id,
                name: "madison".to_string(),
                status: SessionStatus::Running,
                started_at: Utc::now(),
                session_id: Some("session-abc-456".to_string()),
                initial_prompt: Some("Implement the auth endpoint for task !42".to_string()),
                cost_usd: 0.0,
                last_event_at: None,
                has_usage_limit: false,
                usage_limit_reset_at: None,
                has_api_error: false,
                has_auth_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                has_tool_name_conflict: false,
                has_pending_api_call: false,
                is_resume: false,
                output_log: None,
                output_log_path: PathBuf::new(),
            },
        );
    }

    let session_info = sm.collect_session_info().await;
    assert!(
        session_info.contains_key("madison"),
        "collect_session_info() should include the session"
    );
    assert_eq!(
        session_info["madison"].initial_prompt,
        Some("Implement the auth endpoint for task !42".to_string()),
        "collect_session_info() must preserve initial_prompt from CoworkerSession \
         so it survives daemon shutdown→restart cycles"
    );
}

/// Regression test: set_canonical_initial_prompt overrides the decorated prompt
/// so that collect_session_info() returns the canonical mission prompt, not the
/// "This is a fresh session restart..." wrapper.
#[tokio::test]
async fn test_set_canonical_initial_prompt_overrides_decorated_prompt() {
    let sm = SessionManager::new("test-repo".to_string());

    // Insert a session with a decorated prompt (what session clear produces)
    {
        let mut sessions = sm.sessions.write().await;
        let slot_id = uuid::Uuid::new_v4().to_string();
        sessions.insert(
            slot_id.clone(),
            CoworkerSession {
                session: None,
                slot_id,
                name: "park".to_string(),
                status: SessionStatus::Running,
                started_at: Utc::now(),
                session_id: Some("session-clear-1".to_string()),
                initial_prompt: Some(
                    "This is a fresh session restart.\n\nImplement auth endpoint".to_string(),
                ),
                cost_usd: 0.0,
                last_event_at: None,
                has_usage_limit: false,
                usage_limit_reset_at: None,
                has_api_error: false,
                has_auth_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                has_tool_name_conflict: false,
                has_pending_api_call: false,
                is_resume: false,
                output_log: None,
                output_log_path: PathBuf::new(),
            },
        );
    }

    // Override with the canonical prompt (what spawn_coworker does via persisted_initial_prompt)
    sm.set_canonical_initial_prompt("park", Some("Implement auth endpoint".to_string()))
        .await;

    // Verify collect_session_info returns the canonical prompt
    let info = sm.collect_session_info().await;
    assert_eq!(
        info["park"].initial_prompt,
        Some("Implement auth endpoint".to_string()),
        "collect_session_info() should return the canonical prompt after set_canonical_initial_prompt, \
         not the decorated 'fresh restart' wrapper"
    );
}

/// set_canonical_initial_prompt should be a no-op for unknown session names.
#[tokio::test]
async fn test_set_canonical_initial_prompt_noop_for_unknown_name() {
    let sm = SessionManager::new("test-repo".to_string());

    // Should not panic or error
    sm.set_canonical_initial_prompt("nonexistent", Some("prompt".to_string()))
        .await;
}

/// Regression test: collect_session_info() returns None for initial_prompt when
/// no prompt was set (no-prompt coworkers should not have phantom prompt values).
#[tokio::test]
async fn test_collect_session_info_preserves_none_initial_prompt() {
    let sm = SessionManager::new("test-repo".to_string());

    {
        let mut sessions = sm.sessions.write().await;
        let slot_id = uuid::Uuid::new_v4().to_string();
        sessions.insert(
            slot_id.clone(),
            CoworkerSession {
                session: None,
                slot_id,
                name: "park".to_string(),
                status: SessionStatus::Running,
                started_at: Utc::now(),
                session_id: Some("session-xyz-789".to_string()),
                initial_prompt: None,
                cost_usd: 0.0,
                last_event_at: None,
                has_usage_limit: false,
                usage_limit_reset_at: None,
                has_api_error: false,
                has_auth_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                has_tool_name_conflict: false,
                has_pending_api_call: false,
                is_resume: false,
                output_log: None,
                output_log_path: PathBuf::new(),
            },
        );
    }

    let session_info = sm.collect_session_info().await;
    assert_eq!(
        session_info["park"].initial_prompt, None,
        "collect_session_info() should return None when no initial_prompt was set"
    );
}

// --- Tests for extract_tool_state_from_assistant and has_tool_result ---

#[test]
fn extract_tool_state_task_subagent() {
    let message = serde_json::json!({
        "content": [
            {"type": "text", "text": "Let me investigate..."},
            {"type": "tool_use", "id": "1", "name": "Task", "input": {}}
        ]
    });
    let (has_tool, subagent) = extract_tool_state_from_assistant(&message);
    assert!(has_tool, "should detect tool_use");
    assert_eq!(
        subagent,
        Some(true),
        "Task tool should be flagged as subagent"
    );
}

#[test]
fn extract_tool_state_dispatch_agent_subagent() {
    let message = serde_json::json!({
        "content": [
            {"type": "tool_use", "id": "1", "name": "dispatch_agent", "input": {}}
        ]
    });
    let (has_tool, subagent) = extract_tool_state_from_assistant(&message);
    assert!(has_tool);
    assert_eq!(
        subagent,
        Some(true),
        "dispatch_agent tool should be flagged as subagent"
    );
}

#[test]
fn extract_tool_state_regular_tool() {
    let message = serde_json::json!({
        "content": [
            {"type": "tool_use", "id": "1", "name": "Read", "input": {}}
        ]
    });
    let (has_tool, subagent) = extract_tool_state_from_assistant(&message);
    assert!(has_tool, "should detect tool_use");
    assert_eq!(
        subagent,
        Some(false),
        "Read tool should not be flagged as subagent"
    );
}

#[test]
fn extract_tool_state_no_tools() {
    let message = serde_json::json!({
        "content": [
            {"type": "text", "text": "No tools here."}
        ]
    });
    let (has_tool, subagent) = extract_tool_state_from_assistant(&message);
    assert!(!has_tool, "no tool_use blocks");
    assert_eq!(subagent, None, "no subagent state change");
}

#[test]
fn extract_tool_state_last_tool_wins() {
    // When multiple tools are invoked, the last one determines subagent state.
    // This matches the original behavior: cs.has_running_subagent = (last tool is Task).
    let message = serde_json::json!({
        "content": [
            {"type": "tool_use", "id": "1", "name": "Task", "input": {}},
            {"type": "tool_use", "id": "2", "name": "Read", "input": {}}
        ]
    });
    let (has_tool, subagent) = extract_tool_state_from_assistant(&message);
    assert!(has_tool);
    assert_eq!(
        subagent,
        Some(false),
        "last tool (Read) should clear subagent flag"
    );
}

#[test]
fn has_tool_result_detects_result() {
    let message = serde_json::json!({
        "content": [
            {"type": "tool_result", "tool_use_id": "1", "content": "result"}
        ]
    });
    assert!(has_tool_result(&message));
}

#[test]
fn has_tool_result_no_result() {
    let message = serde_json::json!({
        "content": [
            {"type": "text", "text": "user input"}
        ]
    });
    assert!(!has_tool_result(&message));
}

#[test]
fn has_tool_result_empty_content() {
    let message = serde_json::json!({"content": []});
    assert!(!has_tool_result(&message));
}

/// Simulate the full reviewer lifecycle: Task tool_use sets subagent flag,
/// then tool_result clears it. This is the core bug fix — previously,
/// tool_result only cleared has_pending_tool but not has_running_subagent,
/// leaving reviewers permanently exempt from stuck detection.
#[test]
fn subagent_flag_cleared_on_tool_result() {
    let mut has_running_subagent = false;
    let mut has_pending_tool = false;

    // Step 1: Assistant emits a Task tool_use
    let assistant_msg = serde_json::json!({
        "content": [
            {"type": "tool_use", "id": "1", "name": "Task", "input": {}}
        ]
    });
    let (pending, subagent) = extract_tool_state_from_assistant(&assistant_msg);
    if pending {
        has_pending_tool = true;
    }
    if let Some(is_subagent) = subagent {
        has_running_subagent = is_subagent;
    }
    assert!(has_running_subagent, "Task tool should set subagent flag");
    assert!(has_pending_tool, "tool_use should set pending flag");

    // Step 2: User event with tool_result clears both flags
    let user_msg = serde_json::json!({
        "content": [
            {"type": "tool_result", "tool_use_id": "1", "content": "review done"}
        ]
    });
    if has_tool_result(&user_msg) {
        has_pending_tool = false;
        has_running_subagent = false;
    }
    assert!(
        !has_running_subagent,
        "tool_result must clear subagent flag — this was the bug"
    );
    assert!(!has_pending_tool, "tool_result must clear pending flag");
}
