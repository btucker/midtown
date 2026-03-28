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
    });
    idx.apply(&DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 1,
    });
    idx.apply(&created_event("a2", "idle", AgentKind::Worker));
    idx.apply(&DomainEvent::AgentStarted {
        id: "a2".into(),
        pid: 2,
    });
    idx.apply(&created_event("a3", "lead", AgentKind::Lead));
    idx.apply(&DomainEvent::AgentStarted {
        id: "a3".into(),
        pid: 3,
    });

    let idle = idx.idle_workers();
    assert_eq!(idle.len(), 1);
    assert_eq!(idle[0], "a2");
}
