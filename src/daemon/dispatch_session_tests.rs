//! Tests for session-centric dispatch.

use super::*;
use crate::daemon::state::{DaemonPersistentState, SessionRecord};
use crate::task_store::{Task, TaskStatus};

fn make_task(id: &str, subject: &str, owner: &str, status: TaskStatus) -> Task {
    Task {
        id: id.to_string(),
        subject: subject.to_string(),
        agent_name: owner.to_string(),
        status,
        ..Default::default()
    }
}

#[allow(clippy::field_reassign_with_default)]
fn make_ps(project: &str) -> DaemonPersistentState {
    let mut ps = DaemonPersistentState::default();
    ps.tick_dir_key = project.to_string();
    ps.tick_project_name = project.to_string();
    ps.tick_default_channel = project.to_string();
    ps.tick_max_in_progress_tasks = 8;
    ps.tick_now = chrono::Utc::now();
    ps
}

fn make_session(
    session_id: &str,
    task_id: Option<&str>,
    name: &str,
    running: bool,
) -> SessionRecord {
    SessionRecord {
        session_id: session_id.to_string(),
        task_id: task_id.map(|s| s.to_string()),
        name: name.to_string(),
        is_running: running,
        working_dir: "/tmp/test".to_string(),
        ..Default::default()
    }
}

// ============================================================================
// dispatch_via_sessions tests
// ============================================================================

#[test]
fn session_dispatch_skips_when_cooldown_active() {
    let mut ps = make_ps("test");
    ps.tick_session_dispatch_cooldown_active = true;
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];

    let tasks = vec![make_task("1", "Fix", "park", TaskStatus::InProgress)];
    let effects = dispatch_via_sessions_inner(&ps, &tasks);
    assert!(effects.is_empty());
}

#[test]
fn session_dispatch_resumes_stopped_session() {
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];
    ps.sessions.insert(
        "sess-1".into(),
        make_session("sess-1", Some("1"), "park", false),
    );
    ps.tick_session_task_map.insert("1".into(), "sess-1".into());

    let tasks = vec![make_task("1", "Fix", "park", TaskStatus::InProgress)];
    let effects = dispatch_via_sessions_inner(&ps, &tasks);

    let has_resume_spawn = effects.iter().any(|e| {
        if let Effect::SpawnForTask { config, .. } = e {
            matches!(
                config.session_mode,
                crate::launch::SessionMode::ResumeSession(_)
            )
        } else {
            false
        }
    });
    assert!(has_resume_spawn, "Should resume the stopped session");
}

#[test]
fn session_dispatch_skips_running_session() {
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];
    ps.sessions.insert(
        "sess-1".into(),
        make_session("sess-1", Some("1"), "park", true),
    );
    ps.tick_session_task_map.insert("1".into(), "sess-1".into());

    let tasks = vec![make_task("1", "Fix", "park", TaskStatus::InProgress)];
    let effects = dispatch_via_sessions_inner(&ps, &tasks);
    assert!(effects.is_empty(), "Should skip — session is running");
}

#[test]
fn session_dispatch_skips_lead_owned_task() {
    let mut ps = make_ps("test-repo");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "test-repo".into())];

    let tasks = vec![make_task("1", "Fix", "test-repo", TaskStatus::InProgress)];
    let effects = dispatch_via_sessions_inner(&ps, &tasks);
    assert!(effects.is_empty(), "Should skip — owned by project lead");
}

#[test]
fn session_dispatch_skips_recently_recovered_session() {
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];
    ps.sessions.insert(
        "sess-1".into(),
        make_session("sess-1", Some("1"), "park", false),
    );
    ps.tick_session_task_map.insert("1".into(), "sess-1".into());
    ps.tick_recently_recovered_session_ids = ["sess-1".to_string()].into_iter().collect();

    let tasks = vec![make_task("1", "Fix", "park", TaskStatus::InProgress)];
    let effects = dispatch_via_sessions_inner(&ps, &tasks);
    assert!(
        effects.is_empty(),
        "Should skip — session was recently recovered"
    );
}

#[test]
fn session_dispatch_falls_back_to_orphan_when_no_session() {
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];
    // No session record for task 1

    let tasks = vec![make_task("1", "Fix", "park", TaskStatus::InProgress)];
    let effects = dispatch_via_sessions_inner(&ps, &tasks);
    // Should produce no effects (falls back to orphan path, handled elsewhere)
    assert!(
        effects.is_empty(),
        "No session → should fall back to orphan path (no effects here)"
    );
}

#[test]
fn session_dispatch_skips_channel_lead_owned_task() {
    let mut ps = make_ps("test");
    ps.channel_lead_sessions
        .insert("web".to_string(), "sess-lead".to_string());
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "web".into())];

    let tasks = vec![make_task("1", "Fix", "web", TaskStatus::InProgress)];
    let effects = dispatch_via_sessions_inner(&ps, &tasks);
    assert!(effects.is_empty(), "Should skip — owned by channel lead");
}

#[test]
fn session_dispatch_skips_spawn_failure_cooldown() {
    // GIVEN: a stopped session for a task, but the coworker is on spawn failure cooldown
    let mut ps = make_ps("test");
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];
    ps.sessions.insert(
        "sess-1".into(),
        make_session("sess-1", Some("1"), "park", false),
    );
    ps.tick_session_task_map.insert("1".into(), "sess-1".into());
    ps.tick_spawn_failure_cooldown_names = ["park".to_string()].into_iter().collect();

    let tasks = vec![make_task("1", "Fix", "park", TaskStatus::InProgress)];

    // WHEN: session dispatch runs
    let effects = dispatch_via_sessions_inner(&ps, &tasks);

    // THEN: should skip because spawn failure cooldown is active for "park"
    assert!(
        effects.is_empty(),
        "Should skip session recovery when spawn failure cooldown active"
    );
}

#[test]
fn session_dispatch_skips_lead_driven_channel_task() {
    let mut ps = make_ps("test");
    ps.lead_driven_channels = ["web".to_string()].into_iter().collect();
    ps.tick_in_progress_tasks = vec![("1".into(), "Fix".into(), "park".into())];
    ps.sessions.insert(
        "sess-1".into(),
        make_session("sess-1", Some("1"), "park", false),
    );
    ps.tick_session_task_map.insert("1".into(), "sess-1".into());

    let mut task = make_task("1", "Fix", "park", TaskStatus::InProgress);
    task.channel = Some("web".to_string());
    let tasks = vec![task];
    let effects = dispatch_via_sessions_inner(&ps, &tasks);
    assert!(
        effects.is_empty(),
        "Should skip — task is in lead-driven channel"
    );
}
