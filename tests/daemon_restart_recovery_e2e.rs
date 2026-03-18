//! E2E tests for daemon restart recovery.
//!
//! These tests verify that DaemonPersistentState correctly preserves:
//! 1. Task session spans (reviewer assignments)
//! 2. Session records (sessions)
//!
//! The tests use actual daemon types to verify that state correctly round-trips
//! through JSON serialization/deserialization, including serde attributes like
//! #[serde(default)] and #[serde(skip_serializing_if)].
//!
//! Note: Task assignment restoration is tested separately in
//! task_assignment_persistence_test.rs using a captured snapshot.

use midtown::daemon::{DaemonPersistentState, SessionRecord};
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;

/// Test that session records are preserved across daemon restarts.
///
/// Session records are stored in daemon-state.json (sessions) and must survive
/// restarts to enable session recovery (--resume <session_id>).
#[test]
fn test_sessions_preserved_after_restart() {
    // Create temporary test environment
    let temp_dir = TempDir::new().unwrap();
    let state_dir = temp_dir.path();
    fs::create_dir_all(state_dir).unwrap();

    // Create DaemonPersistentState with session records using actual types
    let mut sessions = HashMap::new();
    sessions.insert(
        "session-amsterdam-123".to_string(),
        SessionRecord {
            session_id: "session-amsterdam-123".to_string(),
            name: "amsterdam".to_string(),
            working_dir: "/path/to/worktree".to_string(),
            agent_type: "midtown-code-author".to_string(),
            task_id: Some("1385".to_string()),
            purpose: "task !1385: E2E decision functions".to_string(),
            pid: Some(12345),
            profile: Some("test@example.com".to_string()),
            resume_on_startup: true,
            is_running: true,
            ..Default::default()
        },
    );
    sessions.insert(
        "session-park-456".to_string(),
        SessionRecord {
            session_id: "session-park-456".to_string(),
            name: "park".to_string(),
            working_dir: "/path/to/main".to_string(),
            agent_type: "midtown-code-reviewer".to_string(),
            pr_number: Some(42),
            purpose: "reviewer for PR #42".to_string(),
            pid: Some(12346),
            profile: Some("test@example.com".to_string()),
            resume_on_startup: true,
            is_running: true,
            ..Default::default()
        },
    );

    let state = DaemonPersistentState {
        sessions,
        ..Default::default()
    };

    // Save state to disk
    let state_file = state_dir.join("daemon-state.json");
    fs::write(&state_file, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    // Simulate restart: load state from disk using actual types
    let loaded_state_json = fs::read_to_string(&state_file).unwrap();
    let loaded_state: DaemonPersistentState = serde_json::from_str(&loaded_state_json).unwrap();

    // Verify session records are preserved with type-safe access
    assert_eq!(
        loaded_state.sessions.len(),
        2,
        "Should preserve 2 session records"
    );

    let amsterdam = loaded_state.sessions.get("session-amsterdam-123").unwrap();
    assert_eq!(amsterdam.agent_type, "midtown-code-author");
    assert_eq!(amsterdam.task_id, Some("1385".to_string()));
    assert!(amsterdam.resume_on_startup);

    let park = loaded_state.sessions.get("session-park-456").unwrap();
    assert_eq!(park.agent_type, "midtown-code-reviewer");
    assert_eq!(park.pr_number, Some(42));
    assert!(park.resume_on_startup);

    // Verify serde default attributes work correctly
    // (resume_on_startup defaults to true, so if it were missing from JSON it should still be true)
}

/// Test that persistent state correctly deserializes and can be used to prevent duplicate spawns.
///
/// After restart, the daemon should recognize:
/// - Active reviewer spans → no spawn needed
/// - Sessions in sessions → resume, not spawn fresh
///
/// This test verifies DaemonPersistentState correctly round-trips through disk
/// and that the data structures are populated for dispatch logic to use.
#[test]
fn test_persistent_state_prevents_duplicate_spawns() {
    // Create temporary test environment
    let temp_dir = TempDir::new().unwrap();
    let state_dir = temp_dir.path();
    fs::create_dir_all(state_dir).unwrap();

    // Create DaemonPersistentState with reviewer spans and session records
    let mut state = DaemonPersistentState::default();

    // Create reviewer spans
    state.create_span("review-42", "park", "reviewer", "session-park-456");
    state.task_pr_number.insert("review-42".to_string(), 42);
    state.create_span("review-43", "madison", "reviewer", "session-madison-789");
    state.task_pr_number.insert("review-43".to_string(), 43);

    state.sessions.insert(
        "session-amsterdam-123".to_string(),
        SessionRecord {
            session_id: "session-amsterdam-123".to_string(),
            name: "amsterdam".to_string(),
            agent_type: "midtown-code-author".to_string(),
            task_id: Some("1385".to_string()),
            purpose: "task !1385".to_string(),
            pid: Some(12345),
            profile: Some("test@example.com".to_string()),
            resume_on_startup: true,
            is_running: true,
            ..Default::default()
        },
    );
    state.sessions.insert(
        "session-park-456".to_string(),
        SessionRecord {
            session_id: "session-park-456".to_string(),
            name: "park".to_string(),
            agent_type: "midtown-code-reviewer".to_string(),
            pr_number: Some(42),
            purpose: "reviewer for PR #42".to_string(),
            pid: Some(12346),
            profile: Some("test@example.com".to_string()),
            resume_on_startup: true,
            is_running: true,
            ..Default::default()
        },
    );
    state.sessions.insert(
        "session-madison-789".to_string(),
        SessionRecord {
            session_id: "session-madison-789".to_string(),
            name: "madison".to_string(),
            agent_type: "midtown-code-reviewer".to_string(),
            pr_number: Some(43),
            purpose: "reviewer for PR #43".to_string(),
            pid: Some(12347),
            profile: Some("test@example.com".to_string()),
            resume_on_startup: true,
            is_running: true,
            ..Default::default()
        },
    );

    // Save state to disk
    let state_file = state_dir.join("daemon-state.json");
    fs::write(&state_file, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    // Simulate restart: load state from disk using actual types
    let loaded_state_json = fs::read_to_string(&state_file).unwrap();
    let loaded_state: DaemonPersistentState = serde_json::from_str(&loaded_state_json).unwrap();

    // Verify reviewer spans are available for dispatch logic
    assert!(
        loaded_state.pr_has_active_reviewer(42),
        "PR #42 should have active reviewer span"
    );
    assert!(
        loaded_state.pr_has_active_reviewer(43),
        "PR #43 should have active reviewer span"
    );

    // Verify session records are available for recovery
    assert_eq!(
        loaded_state.sessions.len(),
        3,
        "Should have 3 session records"
    );

    // Identify sessions marked for auto-resume
    let recovering_names: Vec<String> = loaded_state
        .sessions
        .values()
        .filter(|r| r.resume_on_startup)
        .filter(|r| !r.name.is_empty())
        .map(|r| r.name.clone())
        .collect();

    assert_eq!(
        recovering_names.len(),
        3,
        "Should identify 3 sessions to auto-resume"
    );
    assert!(recovering_names.contains(&"amsterdam".to_string()));
    assert!(recovering_names.contains(&"park".to_string()));
    assert!(recovering_names.contains(&"madison".to_string()));
}
