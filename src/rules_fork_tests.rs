//! Tests for dead fork session detection (`decide_dead_fork_respawns`).

use std::collections::HashMap;

use chrono::Utc;

use super::decide_dead_fork_respawns;
use crate::daemon::snapshot::ProcessHealth;
use crate::daemon::state::SessionRecord;

fn dead_health(exit_code: i32) -> ProcessHealth {
    ProcessHealth {
        is_alive: false,
        exit_code: Some(exit_code),
        ..Default::default()
    }
}

fn alive_health() -> ProcessHealth {
    ProcessHealth {
        is_alive: true,
        exit_code: None,
        ..Default::default()
    }
}

fn fork_session_record(
    session_id: &str,
    name: &str,
    thread_parent_id: &str,
    channel: Option<&str>,
) -> SessionRecord {
    SessionRecord {
        session_id: session_id.to_string(),
        task_id: None,
        current_name: Some(name.to_string()),
        preferred_name: Some(name.to_string()),
        working_dir: "/tmp/test-worktree".to_string(),
        branch: None,
        pr_number: None,
        initial_prompt: None,
        is_reviewer: false,
        coworker_type: "channel-lead".to_string(),
        is_running: true,
        created_at: Utc::now(),
        resume_on_startup: false,
        bound_thread_id: Some(thread_parent_id.to_string()),
        last_active: Utc::now(),
        purpose: format!("fork in thread {}", thread_parent_id),
        pid: None,
        channel: channel.map(String::from),
        provider: Some(crate::auth::AuthProvider::Claude),
        platform: Some(crate::platform::Platform::Claude),
        profile: None,
    }
}

#[test]
fn dead_fork_is_detected() {
    let mut topic_sessions = HashMap::new();
    topic_sessions.insert("thread-abc".to_string(), "session-fork-1".to_string());

    let mut sessions = HashMap::new();
    sessions.insert(
        "session-fork-1".to_string(),
        fork_session_record("session-fork-1", "fork-abc", "thread-abc", Some("ops")),
    );

    let mut health = HashMap::new();
    health.insert("fork-abc".to_string(), dead_health(1));

    let respawns = decide_dead_fork_respawns(&topic_sessions, &sessions, &health);

    assert_eq!(respawns.len(), 1);
    assert_eq!(respawns[0].name, "fork-abc");
    assert_eq!(respawns[0].thread_parent_id, "thread-abc");
    assert_eq!(respawns[0].session_id, "session-fork-1");
    assert_eq!(respawns[0].exit_code, 1);
    assert_eq!(respawns[0].channel, Some("ops".to_string()));
    assert!(respawns[0].is_channel_lead);
}

#[test]
fn alive_fork_is_not_respawned() {
    let mut topic_sessions = HashMap::new();
    topic_sessions.insert("thread-abc".to_string(), "session-fork-1".to_string());

    let mut sessions = HashMap::new();
    sessions.insert(
        "session-fork-1".to_string(),
        fork_session_record("session-fork-1", "fork-abc", "thread-abc", Some("ops")),
    );

    let mut health = HashMap::new();
    health.insert("fork-abc".to_string(), alive_health());

    let respawns = decide_dead_fork_respawns(&topic_sessions, &sessions, &health);
    assert!(respawns.is_empty(), "alive fork should not be respawned");
}

#[test]
fn fork_with_no_session_record_is_skipped() {
    let mut topic_sessions = HashMap::new();
    topic_sessions.insert("thread-abc".to_string(), "session-orphan".to_string());

    let sessions: HashMap<String, SessionRecord> = HashMap::new();

    let mut health = HashMap::new();
    health.insert("fork-abc".to_string(), dead_health(1));

    let respawns = decide_dead_fork_respawns(&topic_sessions, &sessions, &health);
    assert!(
        respawns.is_empty(),
        "fork with no session record should be skipped"
    );
}

#[test]
fn fork_with_no_health_data_is_skipped() {
    let mut topic_sessions = HashMap::new();
    topic_sessions.insert("thread-abc".to_string(), "session-fork-1".to_string());

    let mut sessions = HashMap::new();
    sessions.insert(
        "session-fork-1".to_string(),
        fork_session_record("session-fork-1", "fork-abc", "thread-abc", Some("ops")),
    );

    let health: HashMap<String, ProcessHealth> = HashMap::new();

    let respawns = decide_dead_fork_respawns(&topic_sessions, &sessions, &health);
    assert!(
        respawns.is_empty(),
        "fork with no health data should be skipped"
    );
}

#[test]
fn multiple_dead_forks_are_detected() {
    let mut topic_sessions = HashMap::new();
    topic_sessions.insert("thread-1".to_string(), "session-fork-1".to_string());
    topic_sessions.insert("thread-2".to_string(), "session-fork-2".to_string());

    let mut sessions = HashMap::new();
    sessions.insert(
        "session-fork-1".to_string(),
        fork_session_record("session-fork-1", "fork-1", "thread-1", Some("ops")),
    );
    sessions.insert(
        "session-fork-2".to_string(),
        fork_session_record("session-fork-2", "fork-2", "thread-2", Some("dev")),
    );

    let mut health = HashMap::new();
    health.insert("fork-1".to_string(), dead_health(137));
    health.insert("fork-2".to_string(), dead_health(1));

    let respawns = decide_dead_fork_respawns(&topic_sessions, &sessions, &health);
    assert_eq!(respawns.len(), 2);
}

#[test]
fn fork_with_no_name_uses_preferred_name() {
    let mut topic_sessions = HashMap::new();
    topic_sessions.insert("thread-abc".to_string(), "session-fork-1".to_string());

    let mut record = fork_session_record("session-fork-1", "fork-abc", "thread-abc", Some("ops"));
    record.current_name = None; // Name released (stopped but not cleaned up from topic_sessions)

    let mut sessions = HashMap::new();
    sessions.insert("session-fork-1".to_string(), record);

    let mut health = HashMap::new();
    health.insert("fork-abc".to_string(), dead_health(1));

    let respawns = decide_dead_fork_respawns(&topic_sessions, &sessions, &health);
    assert_eq!(respawns.len(), 1);
    assert_eq!(respawns[0].name, "fork-abc");
}

#[test]
fn dead_fork_without_exit_code_is_detected() {
    // In production, collect_health() always sets exit_code: None.
    // The function must detect dead processes via is_alive alone.
    let mut topic_sessions = HashMap::new();
    topic_sessions.insert("thread-abc".to_string(), "session-fork-1".to_string());

    let mut sessions = HashMap::new();
    sessions.insert(
        "session-fork-1".to_string(),
        fork_session_record("session-fork-1", "fork-abc", "thread-abc", Some("ops")),
    );

    let mut health = HashMap::new();
    health.insert(
        "fork-abc".to_string(),
        ProcessHealth {
            is_alive: false,
            exit_code: None, // production case: exit_code never populated
            ..Default::default()
        },
    );

    let respawns = decide_dead_fork_respawns(&topic_sessions, &sessions, &health);
    assert_eq!(
        respawns.len(),
        1,
        "dead fork with exit_code: None should still be detected"
    );
    assert_eq!(respawns[0].exit_code, -1); // defaults to -1 when unknown
}

#[test]
fn fork_with_empty_working_dir_returns_none() {
    let mut topic_sessions = HashMap::new();
    topic_sessions.insert("thread-abc".to_string(), "session-fork-1".to_string());

    let mut record = fork_session_record("session-fork-1", "fork-abc", "thread-abc", Some("ops"));
    record.working_dir = String::new();

    let mut sessions = HashMap::new();
    sessions.insert("session-fork-1".to_string(), record);

    let mut health = HashMap::new();
    health.insert("fork-abc".to_string(), dead_health(1));

    let respawns = decide_dead_fork_respawns(&topic_sessions, &sessions, &health);
    assert_eq!(respawns.len(), 1);
    assert_eq!(respawns[0].working_dir, None);
}
