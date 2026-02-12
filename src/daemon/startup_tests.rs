use super::*;

use chrono::Utc;

/// Create a test HeadlessSessionInfo.
fn test_session_info(
    name: &str,
    task_id: Option<u64>,
) -> crate::daemon::state::HeadlessSessionInfo {
    crate::daemon::state::HeadlessSessionInfo {
        session_id: format!("session-{}", name),
        last_active: Utc::now(),
        purpose: format!("test session for {}", name),
        pid: Some(99999), // Non-existent PID
        coworker_type: Some("dev".to_string()),
        task_id,
        pr_number: None,
        working_dir: Some("/tmp/test".to_string()),
        provider: None,
        profile: None,
    }
}

#[tokio::test]
async fn test_recover_headless_sessions_generates_resume_effects() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    // Insert test sessions
    {
        let mut state = persistent_state.lock().await;
        state.headless_sessions.insert(
            "amsterdam".to_string(),
            test_session_info("amsterdam", Some(42)),
        );
        state.headless_sessions.insert(
            "columbus".to_string(),
            test_session_info("columbus", Some(43)),
        );
    }

    let effects = recover_headless_sessions(&persistent_state, "test-repo").await;

    // Should generate exactly 2 ResumeCoworker effects (one per session)
    assert_eq!(
        effects.len(),
        2,
        "Should generate one ResumeCoworker per session"
    );

    for effect in &effects {
        match effect {
            Effect::ResumeCoworker {
                name, session_id, ..
            } => {
                assert!(
                    name == "amsterdam" || name == "columbus",
                    "Unexpected coworker name: {}",
                    name
                );
                assert!(
                    session_id.starts_with("session-"),
                    "Session ID should match what was persisted"
                );
            }
            _ => panic!("Expected only ResumeCoworker effects, got {:?}", effect),
        }
    }
}

#[tokio::test]
async fn test_recover_headless_sessions_does_not_kill_processes() {
    // This test verifies that recover_headless_sessions does NOT generate
    // any kill effects. The old behavior was to kill -9 the processes,
    // which defeated the purpose of session detachment.
    //
    // We verify this by checking that only ResumeCoworker effects are returned.
    // If kill behavior were added back, it would need to be an Effect variant,
    // and this test would catch the regression.
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        state
            .headless_sessions
            .insert("park".to_string(), test_session_info("park", Some(100)));
    }

    let effects = recover_headless_sessions(&persistent_state, "test-repo").await;

    // All effects should be ResumeCoworker — no kill effects
    for effect in &effects {
        assert!(
            matches!(effect, Effect::ResumeCoworker { .. }),
            "Recovery should only produce ResumeCoworker effects (no kills), got: {:?}",
            effect
        );
    }
}

#[tokio::test]
async fn test_recover_headless_sessions_empty() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    let effects = recover_headless_sessions(&persistent_state, "test-repo").await;
    assert!(
        effects.is_empty(),
        "No sessions to recover should produce no effects"
    );
}

#[tokio::test]
async fn test_recovering_coworker_names_returns_session_names() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());

    {
        let mut state = persistent_state.lock().await;
        state.headless_sessions.insert(
            "amsterdam".to_string(),
            test_session_info("amsterdam", Some(42)),
        );
        state.headless_sessions.insert(
            "columbus".to_string(),
            test_session_info("columbus", Some(43)),
        );
    }

    let names = recovering_coworker_names(&persistent_state).await;
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"amsterdam".to_string()));
    assert!(names.contains(&"columbus".to_string()));
}

#[tokio::test]
async fn test_recovering_coworker_names_empty_state() {
    let persistent_state = tokio::sync::Mutex::new(DaemonPersistentState::default());
    let names = recovering_coworker_names(&persistent_state).await;
    assert!(names.is_empty());
}
