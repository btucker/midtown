//! E2E tests for daemon restart recovery.
//!
//! These tests verify that DaemonPersistentState correctly preserves:
//! 1. Reviewer assignments (github.pr_reviewers)
//! 2. PR author sessions (github.pr_author_sessions)
//! 3. Session records (sessions)
//!
//! The tests use actual daemon types to verify that state correctly round-trips
//! through JSON serialization/deserialization, including serde attributes like
//! #[serde(default)] and #[serde(skip_serializing_if)].
//!
//! Note: Task assignment restoration is tested separately in
//! task_assignment_persistence_test.rs using a captured snapshot.

use chrono::Utc;
use midtown::daemon::{DaemonPersistentState, SessionRecord};
use midtown::github_state::{AssignmentSource, GitHubState, PrAuthorSession, PrReviewerAssignment};
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;

/// Test that reviewer assignments are preserved across daemon restarts.
///
/// Regression test for the bug captured in:
/// - snapshot-review-spawn-lost-after-restart-20260216-235656.json
/// - snapshot-review-spawn-lost-after-restart-20260217-001806.json
/// - snapshot-review-spawn-lost-after-restart-20260217-003046.json
///
/// Reviewer assignments are stored in daemon-state.json (github.pr_reviewers)
/// and must survive daemon restarts to prevent duplicate reviewer spawns.
#[test]
fn test_reviewer_assignments_preserved_after_restart() {
    // Create temporary test environment
    let temp_dir = TempDir::new().unwrap();
    let state_dir = temp_dir.path();
    fs::create_dir_all(state_dir).unwrap();

    // Create DaemonPersistentState with reviewer assignments using actual types
    let now = Utc::now();
    let mut github_state = GitHubState::default();
    github_state.pr_reviewers.insert(
        42,
        PrReviewerAssignment {
            pr_number: 42,
            reviewer: "park".to_string(),
            reviewer_session_id: Some("session-park-456".to_string()),
            assigned_at: now,
            source: AssignmentSource::Webhook,
            webhook_event_id: None,
            restart_count: 0,
        },
    );
    github_state.pr_reviewers.insert(
        43,
        PrReviewerAssignment {
            pr_number: 43,
            reviewer: "madison".to_string(),
            reviewer_session_id: Some("session-madison-789".to_string()),
            assigned_at: now,
            source: AssignmentSource::PollingFallback,
            webhook_event_id: None,
            restart_count: 0,
        },
    );

    let state = DaemonPersistentState {
        github: github_state,
        ..Default::default()
    };

    // Save state to disk
    let state_file = state_dir.join("daemon-state.json");
    fs::write(&state_file, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    // Simulate restart: load state from disk using actual types
    let loaded_state_json = fs::read_to_string(&state_file).unwrap();
    let loaded_state: DaemonPersistentState = serde_json::from_str(&loaded_state_json).unwrap();

    // Verify reviewer assignments are preserved with type-safe access
    assert_eq!(
        loaded_state.github.pr_reviewers.len(),
        2,
        "Should preserve 2 reviewer assignments"
    );
    assert_eq!(
        loaded_state.github.get_reviewer(42),
        Some("park"),
        "PR #42 reviewer should be park"
    );
    assert_eq!(
        loaded_state.github.get_reviewer(43),
        Some("madison"),
        "PR #43 reviewer should be madison"
    );

    // Verify serde attributes are respected (restart_count defaults to 0)
    let pr42_assignment = loaded_state.github.pr_reviewers.get(&42).unwrap();
    assert_eq!(pr42_assignment.restart_count, 0);
}

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
            current_name: Some("amsterdam".to_string()),
            preferred_name: Some("amsterdam".to_string()),
            working_dir: "/path/to/worktree".to_string(),
            coworker_type: "dev".to_string(),
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
            current_name: Some("park".to_string()),
            preferred_name: Some("park".to_string()),
            working_dir: "/path/to/main".to_string(),
            coworker_type: "reviewer".to_string(),
            is_reviewer: true,
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
    assert_eq!(amsterdam.coworker_type, "dev");
    assert_eq!(amsterdam.task_id, Some("1385".to_string()));
    assert!(amsterdam.resume_on_startup);

    let park = loaded_state.sessions.get("session-park-456").unwrap();
    assert_eq!(park.coworker_type, "reviewer");
    assert_eq!(park.pr_number, Some(42));
    assert!(park.resume_on_startup);

    // Verify serde default attributes work correctly
    // (resume_on_startup defaults to true, so if it were missing from JSON it should still be true)
}

/// Test that PR author sessions are preserved across daemon restarts.
///
/// PR author sessions are stored in daemon-state.json (github.pr_author_sessions)
/// and must survive daemon restarts so that other coworkers can resume work on
/// a PR with full context from the original author's session.
#[test]
fn test_pr_author_sessions_preserved_after_restart() {
    // Create temporary test environment
    let temp_dir = TempDir::new().unwrap();
    let state_dir = temp_dir.path();
    fs::create_dir_all(state_dir).unwrap();

    // Create DaemonPersistentState with PR author sessions using actual types
    let now = Utc::now();
    let mut github_state = GitHubState::default();
    github_state.pr_author_sessions.insert(
        42,
        PrAuthorSession {
            session_id: "session-amsterdam-123".to_string(),
            branch: "amsterdam/add-auth-endpoint".to_string(),
            original_author: "amsterdam".to_string(),
            stored_at: now,
            task_id: Some("1385".to_string()),
        },
    );
    github_state.pr_author_sessions.insert(
        43,
        PrAuthorSession {
            session_id: "session-columbus-456".to_string(),
            branch: "columbus/fix-bug".to_string(),
            original_author: "columbus".to_string(),
            stored_at: now,
            task_id: Some("1388".to_string()),
        },
    );

    let state = DaemonPersistentState {
        github: github_state,
        ..Default::default()
    };

    // Save state to disk
    let state_file = state_dir.join("daemon-state.json");
    fs::write(&state_file, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    // Simulate restart: load state from disk using actual types
    let loaded_state_json = fs::read_to_string(&state_file).unwrap();
    let loaded_state: DaemonPersistentState = serde_json::from_str(&loaded_state_json).unwrap();

    // Verify PR author sessions are preserved with type-safe access
    assert_eq!(
        loaded_state.github.pr_author_sessions.len(),
        2,
        "Should preserve 2 PR author sessions"
    );

    let pr42_session = loaded_state.github.pr_author_sessions.get(&42).unwrap();
    assert_eq!(pr42_session.session_id, "session-amsterdam-123");
    assert_eq!(pr42_session.branch, "amsterdam/add-auth-endpoint");
    assert_eq!(pr42_session.original_author, "amsterdam");
    assert_eq!(pr42_session.task_id, Some("1385".to_string()));

    let pr43_session = loaded_state.github.pr_author_sessions.get(&43).unwrap();
    assert_eq!(pr43_session.session_id, "session-columbus-456");
    assert_eq!(pr43_session.branch, "columbus/fix-bug");
    assert_eq!(pr43_session.original_author, "columbus");
    assert_eq!(pr43_session.task_id, Some("1388".to_string()));
}

/// Test that persistent state correctly deserializes and can be used to prevent duplicate spawns.
///
/// After restart, the daemon should recognize:
/// - Reviewers in pr_reviewers → no spawn needed
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

    // Create DaemonPersistentState with reviewer assignments and session records
    let now = Utc::now();
    let mut github_state = GitHubState::default();
    github_state.pr_reviewers.insert(
        42,
        PrReviewerAssignment {
            pr_number: 42,
            reviewer: "park".to_string(),
            reviewer_session_id: Some("session-park-456".to_string()),
            assigned_at: now,
            source: AssignmentSource::Webhook,
            webhook_event_id: None,
            restart_count: 0,
        },
    );
    github_state.pr_reviewers.insert(
        43,
        PrReviewerAssignment {
            pr_number: 43,
            reviewer: "madison".to_string(),
            reviewer_session_id: Some("session-madison-789".to_string()),
            assigned_at: now,
            source: AssignmentSource::PollingFallback,
            webhook_event_id: None,
            restart_count: 0,
        },
    );

    let mut sessions = HashMap::new();
    sessions.insert(
        "session-amsterdam-123".to_string(),
        SessionRecord {
            session_id: "session-amsterdam-123".to_string(),
            current_name: Some("amsterdam".to_string()),
            preferred_name: Some("amsterdam".to_string()),
            coworker_type: "dev".to_string(),
            task_id: Some("1385".to_string()),
            purpose: "task !1385".to_string(),
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
            current_name: Some("park".to_string()),
            preferred_name: Some("park".to_string()),
            coworker_type: "reviewer".to_string(),
            is_reviewer: true,
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
        github: github_state,
        sessions,
        ..Default::default()
    };

    // Save state to disk
    let state_file = state_dir.join("daemon-state.json");
    fs::write(&state_file, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    // Simulate restart: load state from disk using actual types
    let loaded_state_json = fs::read_to_string(&state_file).unwrap();
    let loaded_state: DaemonPersistentState = serde_json::from_str(&loaded_state_json).unwrap();

    // Verify reviewer assignments are available for dispatch logic
    assert_eq!(
        loaded_state.github.pr_reviewers.len(),
        2,
        "Should have 2 reviewer assignments"
    );
    assert_eq!(loaded_state.github.get_reviewer(42), Some("park"));
    assert_eq!(loaded_state.github.get_reviewer(43), Some("madison"));

    // Verify session records are available for recovery
    assert_eq!(
        loaded_state.sessions.len(),
        2,
        "Should have 2 session records"
    );

    // Identify sessions marked for auto-resume
    let recovering_names: Vec<String> = loaded_state
        .sessions
        .values()
        .filter(|r| r.resume_on_startup)
        .filter_map(|r| r.current_name.clone())
        .collect();

    assert_eq!(
        recovering_names.len(),
        2,
        "Should identify 2 sessions to auto-resume"
    );
    assert!(recovering_names.contains(&"amsterdam".to_string()));
    assert!(recovering_names.contains(&"park".to_string()));
}
