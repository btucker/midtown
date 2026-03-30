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
fn dispatches_pending_task_when_no_agents() {
    let events = vec![DomainEvent::TaskCreated {
        id: "task-1".into(),
        subject: "Implement the feature".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    }];

    let proj = make_projections(&events);
    let commands = dispatch_pending_tasks(&proj, 3);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            Command::SpawnAgent(cfg)
                if cfg.task_id.as_deref() == Some("task-1")
                && cfg.initial_prompt.as_deref() == Some("Implement the feature")
                && cfg.kind == AgentKind::Worker
                && cfg.agent_type == "midtown-code-author"
        ),
        "expected SpawnAgent for task-1, got {:?}",
        commands[0]
    );
}

#[test]
fn respects_max_in_progress_limit() {
    // 3 tasks already in-progress, limit is 3 — no new spawns
    let events = vec![
        DomainEvent::TaskCreated {
            id: "task-1".into(),
            subject: "Task 1".into(),
            channel: "main".into(),
            blocked_by: vec![],
            agent_type: None,
            agent_name: None,
            icon: None,
            color: None,
            parent: None,
        },
        DomainEvent::TaskCreated {
            id: "task-2".into(),
            subject: "Task 2".into(),
            channel: "main".into(),
            blocked_by: vec![],
            agent_type: None,
            agent_name: None,
            icon: None,
            color: None,
            parent: None,
        },
        DomainEvent::TaskCreated {
            id: "task-3".into(),
            subject: "Task 3".into(),
            channel: "main".into(),
            blocked_by: vec![],
            agent_type: None,
            agent_name: None,
            icon: None,
            color: None,
            parent: None,
        },
        DomainEvent::TaskCreated {
            id: "task-4".into(),
            subject: "Task 4 (pending)".into(),
            channel: "main".into(),
            blocked_by: vec![],
            agent_type: None,
            agent_name: None,
            icon: None,
            color: None,
            parent: None,
        },
        DomainEvent::TaskAssigned {
            task_id: "task-1".into(),
            agent_id: "a1".into(),
        },
        DomainEvent::TaskAssigned {
            task_id: "task-2".into(),
            agent_id: "a2".into(),
        },
        DomainEvent::TaskAssigned {
            task_id: "task-3".into(),
            agent_id: "a3".into(),
        },
    ];

    let proj = make_projections(&events);
    let commands = dispatch_pending_tasks(&proj, 3);

    assert!(
        commands.is_empty(),
        "expected no commands with 3 in-progress and limit 3, got {:?}",
        commands
    );
}

#[test]
fn skips_blocked_tasks() {
    // task-1 is unblocked, task-2 is blocked by task-1
    let events = vec![
        DomainEvent::TaskCreated {
            id: "task-1".into(),
            subject: "First task".into(),
            channel: "main".into(),
            blocked_by: vec![],
            agent_type: None,
            agent_name: None,
            icon: None,
            color: None,
            parent: None,
        },
        DomainEvent::TaskCreated {
            id: "task-2".into(),
            subject: "Blocked task".into(),
            channel: "main".into(),
            blocked_by: vec!["task-1".into()],
            agent_type: None,
            agent_name: None,
            icon: None,
            color: None,
            parent: None,
        },
    ];

    let proj = make_projections(&events);
    let commands = dispatch_pending_tasks(&proj, 3);

    assert_eq!(
        commands.len(),
        1,
        "expected only 1 spawn, got {:?}",
        commands
    );
    assert!(
        matches!(
            &commands[0],
            Command::SpawnAgent(cfg) if cfg.task_id.as_deref() == Some("task-1")
        ),
        "expected SpawnAgent for task-1, got {:?}",
        commands[0]
    );
}

#[test]
fn stops_agents_for_completed_tasks() {
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
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "a1".into(),
            pid: 100,
            session_id: None,
        },
        DomainEvent::TaskCreated {
            id: "task-1".into(),
            subject: "Done task".into(),
            channel: "main".into(),
            blocked_by: vec![],
            agent_type: None,
            agent_name: None,
            icon: None,
            color: None,
            parent: None,
        },
        DomainEvent::TaskAssigned {
            task_id: "task-1".into(),
            agent_id: "a1".into(),
        },
        DomainEvent::TaskCompleted {
            task_id: "task-1".into(),
        },
    ];

    let proj = make_projections(&events);
    let commands = stop_completed_agents(&proj);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            Command::StopAgent { id, reason }
                if id == "a1" && reason == "task completed"
        ),
        "expected StopAgent for a1, got {:?}",
        commands[0]
    );
}

#[test]
fn skips_lead_driven_channel_tasks() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::ChannelLeadDrivenSet {
        channel: "manual".into(),
        lead_driven: true,
    });
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Manual task".into(),
        channel: "manual".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });

    let commands = dispatch_pending_tasks(&proj, 5);
    assert!(commands.is_empty());
}

#[test]
fn check_duplicate_workers_stops_older_of_two() {
    use chrono::{Duration, Utc};

    let events = vec![
        DomainEvent::TaskCreated {
            id: "task-1".into(),
            subject: "Fix bug".into(),
            channel: "main".into(),
            blocked_by: vec![],
            agent_type: None,
            agent_name: None,
            icon: None,
            color: None,
            parent: None,
        },
        // First (older) worker
        DomainEvent::AgentCreated {
            id: "a1".into(),
            name: "worker-old".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: Some("task-1".into()),
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "a1".into(),
            pid: 100,
            session_id: None,
        },
        // Second (newer) worker for the same task
        DomainEvent::AgentCreated {
            id: "a2".into(),
            name: "worker-new".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: Some("task-1".into()),
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "a2".into(),
            pid: 101,
            session_id: None,
        },
    ];

    let mut proj = make_projections(&events);

    // Make a1 older so the sort order is deterministic.
    proj.agents.by_id.get_mut("a1").unwrap().started_at = Some(Utc::now() - Duration::minutes(10));
    proj.agents.by_id.get_mut("a2").unwrap().started_at = Some(Utc::now());

    let commands = check_duplicate_workers(&proj);

    assert_eq!(
        commands.len(),
        1,
        "expected 1 StopAgent, got {:?}",
        commands
    );
    assert!(
        matches!(&commands[0], Command::StopAgent { id, reason } if id == "a2" && reason == "duplicate worker for task"),
        "expected StopAgent for newer worker a2, got {:?}",
        commands[0]
    );
}

#[test]
fn check_duplicate_workers_no_op_when_no_duplicates() {
    let events = vec![
        DomainEvent::TaskCreated {
            id: "task-1".into(),
            subject: "Solo task".into(),
            channel: "main".into(),
            blocked_by: vec![],
            agent_type: None,
            agent_name: None,
            icon: None,
            color: None,
            parent: None,
        },
        DomainEvent::AgentCreated {
            id: "a1".into(),
            name: "solo-worker".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: Some("task-1".into()),
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "a1".into(),
            pid: 100,
            session_id: None,
        },
    ];

    let proj = make_projections(&events);
    let commands = check_duplicate_workers(&proj);
    assert!(
        commands.is_empty(),
        "expected no commands with single worker per task, got {:?}",
        commands
    );
}
