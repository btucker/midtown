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
// stop_sessions_for_completed_tasks tests
// ============================================================================

#[test]
fn stops_running_session_for_completed_task() {
    let mut ps = make_ps("proj");
    let tasks = vec![make_task("1", "Fix bug", "york", TaskStatus::Completed)];

    ps.sessions.insert(
        "sess-1".into(),
        make_session("sess-1", Some("1"), "york", true),
    );
    ps.tick_active_session_names.insert("york".into());

    let effects = stop_sessions_for_completed_tasks(&ps, &tasks);

    let shutdown_names: Vec<_> = effects
        .iter()
        .filter_map(|e| {
            if let Effect::ShutdownCoworker { name, .. } = e {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(shutdown_names, vec!["york"]);
}

#[test]
fn skips_in_progress_task() {
    let mut ps = make_ps("proj");
    let tasks = vec![make_task("1", "Fix bug", "york", TaskStatus::InProgress)];

    ps.sessions.insert(
        "sess-1".into(),
        make_session("sess-1", Some("1"), "york", true),
    );
    ps.tick_active_session_names.insert("york".into());

    let effects = stop_sessions_for_completed_tasks(&ps, &tasks);

    let shutdowns: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::ShutdownCoworker { .. }))
        .collect();
    assert!(
        shutdowns.is_empty(),
        "Should not stop in-progress task sessions"
    );
}

#[test]
fn skips_stopped_session() {
    let mut ps = make_ps("proj");
    let tasks = vec![make_task("1", "Fix bug", "york", TaskStatus::Completed)];

    // Session exists but is NOT in tick_active_session_names (already stopped)
    ps.sessions.insert(
        "sess-1".into(),
        make_session("sess-1", Some("1"), "york", false),
    );

    let effects = stop_sessions_for_completed_tasks(&ps, &tasks);

    let shutdowns: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::ShutdownCoworker { .. }))
        .collect();
    assert!(
        shutdowns.is_empty(),
        "Should not stop already-stopped sessions"
    );
}

#[test]
fn skips_session_without_task() {
    let mut ps = make_ps("proj");
    let tasks = vec![make_task("1", "Fix bug", "york", TaskStatus::Completed)];

    // Session has no task_id
    ps.sessions
        .insert("sess-1".into(), make_session("sess-1", None, "york", true));
    ps.tick_active_session_names.insert("york".into());

    let effects = stop_sessions_for_completed_tasks(&ps, &tasks);

    let shutdowns: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::ShutdownCoworker { .. }))
        .collect();
    assert!(
        shutdowns.is_empty(),
        "Should not stop sessions without tasks"
    );
}

#[test]
fn skips_fork_sessions() {
    let mut ps = make_ps("proj");
    let tasks = vec![make_task("1", "Fix bug", "york", TaskStatus::Completed)];

    // Fork session: agent_type is channel-lead and has bound_thread_id
    let mut session = make_session("sess-1", Some("1"), "york", true);
    session.agent_type = "midtown-channel-lead".into();
    session.bound_thread_id = Some("thread-123".into());
    ps.sessions.insert("sess-1".into(), session);
    ps.tick_active_session_names.insert("york".into());

    let effects = stop_sessions_for_completed_tasks(&ps, &tasks);

    let shutdowns: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::ShutdownCoworker { .. }))
        .collect();
    assert!(shutdowns.is_empty(), "Should not stop fork sessions");
}

#[test]
fn stops_multiple_sessions_for_completed_tasks() {
    let mut ps = make_ps("proj");
    let tasks = vec![
        make_task("1", "Fix bug", "york", TaskStatus::Completed),
        make_task("2", "Add feature", "park", TaskStatus::Completed),
        make_task("3", "Refactor", "madison", TaskStatus::InProgress),
    ];

    ps.sessions.insert(
        "sess-1".into(),
        make_session("sess-1", Some("1"), "york", true),
    );
    ps.sessions.insert(
        "sess-2".into(),
        make_session("sess-2", Some("2"), "park", true),
    );
    ps.sessions.insert(
        "sess-3".into(),
        make_session("sess-3", Some("3"), "madison", true),
    );
    ps.tick_active_session_names.insert("york".into());
    ps.tick_active_session_names.insert("park".into());
    ps.tick_active_session_names.insert("madison".into());

    let effects = stop_sessions_for_completed_tasks(&ps, &tasks);

    let mut shutdown_names: Vec<_> = effects
        .iter()
        .filter_map(|e| {
            if let Effect::ShutdownCoworker { name, .. } = e {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();
    shutdown_names.sort();
    assert_eq!(shutdown_names, vec!["park", "york"]);
}

#[test]
fn posts_ops_message_when_stopping() {
    let mut ps = make_ps("proj");
    let tasks = vec![make_task("1", "Fix bug", "york", TaskStatus::Completed)];

    ps.sessions.insert(
        "sess-1".into(),
        make_session("sess-1", Some("1"), "york", true),
    );
    ps.tick_active_session_names.insert("york".into());

    let effects = stop_sessions_for_completed_tasks(&ps, &tasks);

    let ops_messages: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::PostToChannel { channel, .. } if channel.as_deref() == Some("ops")))
        .collect();
    assert!(!ops_messages.is_empty(), "Should post to ops channel");
}
