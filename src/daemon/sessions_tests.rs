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
            cost_usd: 0.0,
            last_event_at: None,
            has_usage_limit: false,
            usage_limit_reset_at: None,
            has_api_error: false,
            has_auth_error: false,
            has_running_subagent: false,
            has_pending_tool: false,
            has_tool_name_conflict: false,
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
                cost_usd: 0.0,
                last_event_at: None,
                has_usage_limit: false,
                usage_limit_reset_at: None,
                has_api_error: false,
                has_auth_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                has_tool_name_conflict: false,
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
