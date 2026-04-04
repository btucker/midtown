use super::*;
use crate::daemon_v2::events::*;

fn created_event(id: &str, name: &str, kind: AgentKind) -> DomainEvent {
    DomainEvent::AgentCreated {
        id: id.into(),
        name: name.into(),
        kind,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    }
}

#[test]
fn create_and_lookup_by_id() {
    let mut idx = AgentIndex::default();
    idx.apply(&created_event("a1", "ghost-town", AgentKind::Worker));
    let agent = idx.by_id.get("a1").unwrap();
    assert_eq!(agent.name, "ghost-town");
    assert_eq!(agent.kind, AgentKind::Worker);
}

#[test]
fn lookup_by_name() {
    let mut idx = AgentIndex::default();
    idx.apply(&created_event("a1", "ghost-town", AgentKind::Worker));
    assert_eq!(idx.by_name.get("ghost-town"), Some(&"a1".to_string()));
}

#[test]
fn lookup_by_channel() {
    let mut idx = AgentIndex::default();
    idx.apply(&created_event("a1", "lead-1", AgentKind::Lead));
    idx.apply(&created_event("a2", "worker-1", AgentKind::Worker));
    let channel_agents = idx.by_channel.get("main").unwrap();
    assert_eq!(channel_agents.len(), 2);
}

#[test]
fn started_adds_to_running() {
    let mut idx = AgentIndex::default();
    idx.apply(&created_event("a1", "ghost-town", AgentKind::Worker));
    idx.apply(&DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 1234,
        session_id: None,
    });
    assert!(idx.running.contains("a1"));
    assert_eq!(idx.by_id.get("a1").unwrap().pid, Some(1234));
}

#[test]
fn stopped_removes_from_running() {
    let mut idx = AgentIndex::default();
    idx.apply(&created_event("a1", "ghost-town", AgentKind::Worker));
    idx.apply(&DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 1234,
        session_id: None,
    });
    idx.apply(&DomainEvent::AgentStopped {
        id: "a1".into(),
        reason: "completed".into(),
    });
    assert!(!idx.running.contains("a1"));
    assert!(idx.by_id.get("a1").unwrap().stopped_at.is_some());
}

#[test]
fn lookup_by_task() {
    let mut idx = AgentIndex::default();
    idx.apply(&DomainEvent::AgentCreated {
        id: "a1".into(),
        name: "ghost-town".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("task-1".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    assert_eq!(idx.by_task.get("task-1"), Some(&"a1".to_string()));
}

#[test]
fn idle_workers_returns_running_workers_without_tasks() {
    let mut idx = AgentIndex::default();
    idx.apply(&DomainEvent::AgentCreated {
        id: "a1".into(),
        name: "busy".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: None,
        task_id: Some("task-1".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    idx.apply(&DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 1,
        session_id: None,
    });
    idx.apply(&created_event("a2", "idle", AgentKind::Worker));
    idx.apply(&DomainEvent::AgentStarted {
        id: "a2".into(),
        pid: 2,
        session_id: None,
    });
    idx.apply(&created_event("a3", "lead", AgentKind::Lead));
    idx.apply(&DomainEvent::AgentStarted {
        id: "a3".into(),
        pid: 3,
        session_id: None,
    });

    let idle = idx.idle_workers();
    assert_eq!(idle.len(), 1);
    assert_eq!(idle[0], "a2");
}

#[test]
fn fork_indexed_by_thread() {
    let mut idx = AgentIndex::default();
    idx.apply(&DomainEvent::AgentCreated {
        id: "f1".into(),
        name: "fork-abc".into(),
        kind: AgentKind::Fork,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("web".into()),
        task_id: None,
        bound_thread_id: Some("thread-123".into()),
        icon: None,
        color: None,
    });

    let fork = idx.fork_for_thread("thread-123").unwrap();
    assert_eq!(fork.id, "f1");
    assert_eq!(fork.bound_thread_id, Some("thread-123".to_string()));
    assert_eq!(fork.kind, AgentKind::Fork);
}

#[test]
fn thread_binding_persists_through_stop() {
    let mut idx = AgentIndex::default();
    idx.apply(&DomainEvent::AgentCreated {
        id: "f1".into(),
        name: "fork-abc".into(),
        kind: AgentKind::Fork,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("web".into()),
        task_id: None,
        bound_thread_id: Some("thread-123".into()),
        icon: None,
        color: None,
    });
    idx.apply(&DomainEvent::AgentStarted {
        id: "f1".into(),
        pid: 999,
        session_id: None,
    });
    idx.apply(&DomainEvent::AgentStopped {
        id: "f1".into(),
        reason: "exited".into(),
    });

    // Thread binding persists — stopped agents can be resumed on thread activity
    let fork = idx.fork_for_thread("thread-123").unwrap();
    assert_eq!(fork.id, "f1");
    assert!(!idx.running.contains("f1"));
}

#[test]
fn fork_for_thread_returns_none_for_unknown() {
    let idx = AgentIndex::default();
    assert!(idx.fork_for_thread("unknown-thread").is_none());
}

#[test]
fn resumed_updates_pid_and_restores_running() {
    let mut idx = AgentIndex::default();
    idx.apply(&created_event("a1", "ghost-town", AgentKind::Worker));
    idx.apply(&DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 1000,
        session_id: Some("sess-abc".into()),
    });
    idx.apply(&DomainEvent::AgentStopped {
        id: "a1".into(),
        reason: "process not found on startup".into(),
    });

    // After stop: not running, pid cleared, but session_id preserved
    assert!(!idx.running.contains("a1"));
    let agent = idx.by_id.get("a1").unwrap();
    assert_eq!(agent.pid, None);
    assert_eq!(agent.session_id, Some("sess-abc".into()));

    // Resume with new PID
    idx.apply(&DomainEvent::AgentResumed {
        id: "a1".into(),
        pid: 2000,
    });

    assert!(idx.running.contains("a1"));
    let agent = idx.by_id.get("a1").unwrap();
    assert_eq!(agent.pid, Some(2000));
    assert!(agent.stopped_at.is_none());
    // started_at reset so idle checks use resume time, not original spawn time
    assert!(agent.started_at.is_some());
    // session_id unchanged
    assert_eq!(agent.session_id, Some("sess-abc".into()));
}

#[test]
fn channel_lead_returns_running_lead_not_stopped_one() {
    let mut idx = AgentIndex::default();

    // First lead created and stopped
    idx.apply(&DomainEvent::AgentCreated {
        id: "old-lead".into(),
        name: "midtown".into(),
        kind: AgentKind::Lead,
        agent_type: "midtown-project-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("midtown".into()),
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    idx.apply(&DomainEvent::AgentStarted {
        id: "old-lead".into(),
        pid: 100,
        session_id: None,
    });
    idx.apply(&DomainEvent::AgentStopped {
        id: "old-lead".into(),
        reason: "crashed".into(),
    });

    // Second lead created and running
    idx.apply(&DomainEvent::AgentCreated {
        id: "new-lead".into(),
        name: "midtown".into(),
        kind: AgentKind::Lead,
        agent_type: "midtown-project-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("midtown".into()),
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    idx.apply(&DomainEvent::AgentStarted {
        id: "new-lead".into(),
        pid: 200,
        session_id: None,
    });

    let lead = idx.channel_lead("midtown").unwrap();
    assert_eq!(
        lead.id, "new-lead",
        "channel_lead should return the running lead, not the stopped one; got {}",
        lead.id
    );
}

#[test]
fn session_not_found_clears_session_id_on_replay() {
    let mut idx = AgentIndex::default();
    idx.apply(&created_event("a1", "ghost-town", AgentKind::Worker));
    idx.apply(&DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 1234,
        session_id: Some("sess-stale".into()),
    });

    // session_id is set
    assert_eq!(
        idx.by_id.get("a1").unwrap().session_id,
        Some("sess-stale".into())
    );

    // Replay an AgentSessionNotFound event (as would happen on daemon restart)
    idx.apply(&DomainEvent::AgentSessionNotFound {
        name: "ghost-town".into(),
    });

    // session_id should be cleared
    assert_eq!(
        idx.by_id.get("a1").unwrap().session_id,
        None,
        "AgentSessionNotFound must clear session_id during projection replay"
    );
}
