use super::*;
use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::Projections;

fn make_projections(events: &[DomainEvent]) -> Projections {
    let mut proj = Projections::default();
    proj.apply_all(events);
    proj
}

#[test]
fn respawn_dead_agent_with_task() {
    // Worker created with task, task assigned (in-progress), then agent stopped
    let events = vec![
        DomainEvent::AgentCreated {
            id: "a1".into(),
            name: "worker-1".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: Some("task-1".into()),
            bound_thread_id: None,
        },
        DomainEvent::AgentStarted {
            id: "a1".into(),
            pid: 100,
            session_id: None,
        },
        DomainEvent::TaskCreated {
            id: "task-1".into(),
            subject: "Do something".into(),
            channel: "main".into(),
            blocked_by: vec![],
        },
        DomainEvent::TaskAssigned {
            task_id: "task-1".into(),
            agent_id: "a1".into(),
        },
        DomainEvent::AgentStopped {
            id: "a1".into(),
            reason: "process died".into(),
        },
    ];

    let proj = make_projections(&events);
    let commands = check_dead_workers(&proj);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], Command::ResetTask { task_id } if task_id == "task-1"),
        "expected ResetTask for task-1, got {:?}",
        commands[0]
    );
}

#[test]
fn no_respawn_for_completed_task() {
    // Worker stopped but task is completed — no reset needed
    let events = vec![
        DomainEvent::AgentCreated {
            id: "a1".into(),
            name: "worker-1".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: Some("task-1".into()),
            bound_thread_id: None,
        },
        DomainEvent::AgentStarted {
            id: "a1".into(),
            pid: 100,
            session_id: None,
        },
        DomainEvent::TaskCreated {
            id: "task-1".into(),
            subject: "Do something".into(),
            channel: "main".into(),
            blocked_by: vec![],
        },
        DomainEvent::TaskAssigned {
            task_id: "task-1".into(),
            agent_id: "a1".into(),
        },
        DomainEvent::TaskCompleted {
            task_id: "task-1".into(),
        },
        DomainEvent::AgentStopped {
            id: "a1".into(),
            reason: "done".into(),
        },
    ];

    let proj = make_projections(&events);
    let commands = check_dead_workers(&proj);
    assert!(
        commands.is_empty(),
        "expected no commands, got {:?}",
        commands
    );
}

#[test]
fn ensure_leads_alive_spawns_missing_lead() {
    // No agents at all — should spawn a lead
    let proj = Projections::default();
    let commands = ensure_leads_alive(&proj, "main");

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], Command::SpawnAgent(cfg) if cfg.channel.as_deref() == Some("main") && cfg.kind == AgentKind::Lead),
        "expected SpawnAgent for channel lead, got {:?}",
        commands[0]
    );
}

#[test]
fn ensure_leads_alive_no_op_when_running() {
    // A running lead for the channel — no spawn needed
    let events = vec![
        DomainEvent::AgentCreated {
            id: "lead-1".into(),
            name: "main".into(),
            kind: AgentKind::Lead,
            agent_type: "midtown-channel-lead".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: None,
            bound_thread_id: None,
        },
        DomainEvent::AgentStarted {
            id: "lead-1".into(),
            pid: 42,
            session_id: None,
        },
    ];

    let proj = make_projections(&events);
    let commands = ensure_leads_alive(&proj, "main");
    assert!(
        commands.is_empty(),
        "expected no commands, got {:?}",
        commands
    );
}
