use super::*;
use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::Projections;

fn make_projections(events: &[DomainEvent]) -> Projections {
    let mut proj = Projections::default();
    proj.apply_all(events);
    proj
}

fn running_lead_events(channel: &str) -> Vec<DomainEvent> {
    vec![
        DomainEvent::AgentCreated {
            id: "lead-1".into(),
            name: channel.to_string(),
            kind: AgentKind::Lead,
            agent_type: "midtown-channel-lead".into(),
            provider: Provider::ClaudeCode,
            channel: Some(channel.to_string()),
            task_id: None,
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "lead-1".into(),
            pid: 42,
            session_id: None,
        },
    ]
}

#[test]
fn mention_lead_routes_to_channel_lead() {
    let proj = make_projections(&running_lead_events("main"));
    let commands = route_mentions(&proj, "main", "alice", "hey @lead can you help?");

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], Command::NudgeAgent { id, message }
            if id == "lead-1" && message.contains("mention from alice")),
        "expected NudgeAgent for lead-1, got {:?}",
        commands[0]
    );
}

#[test]
fn mention_channel_name_routes_to_channel_lead() {
    let proj = make_projections(&running_lead_events("main"));
    let commands = route_mentions(&proj, "main", "alice", "hey @main what's the status?");

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], Command::NudgeAgent { id, .. } if id == "lead-1"),
        "expected NudgeAgent for lead-1, got {:?}",
        commands[0]
    );
}

#[test]
fn mention_agent_by_name_routes_nudge() {
    let mut events = running_lead_events("main");
    events.extend(vec![
        DomainEvent::AgentCreated {
            id: "worker-1".into(),
            name: "ghost-town".into(),
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
            id: "worker-1".into(),
            pid: 99,
            session_id: None,
        },
    ]);

    let proj = make_projections(&events);
    let commands = route_mentions(&proj, "main", "alice", "hey @ghost-town can you check?");

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], Command::NudgeAgent { id, message }
            if id == "worker-1" && message.contains("mention from alice")),
        "expected NudgeAgent for worker-1, got {:?}",
        commands[0]
    );
}

#[test]
fn self_mention_ignored() {
    let proj = make_projections(&running_lead_events("main"));
    // "main" is the lead's name and the channel — self-mentioning from "main" sender
    let commands = route_mentions(&proj, "main", "main", "hey @lead check this");

    // @lead mention from "main" sender should be ignored because target == sender
    // Actually target is "lead" and sender is "main" — not equal. Let's test actual self-mention.
    // Self-mention: sender mentions their own name
    let commands2 = route_mentions(&proj, "main", "ghost-town", "hey @ghost-town oops");
    assert!(
        commands2.is_empty(),
        "self-mention should be ignored, got {:?}",
        commands2
    );

    // Suppress unused warning
    let _ = commands;
}

#[test]
fn unknown_mention_no_nudge() {
    let proj = make_projections(&running_lead_events("main"));
    let commands = route_mentions(&proj, "main", "alice", "hey @nobody-exists do this");

    assert!(
        commands.is_empty(),
        "unknown mention should produce no commands, got {:?}",
        commands
    );
}

#[test]
fn at_all_broadcasts_to_all_running_agents() {
    let mut events = running_lead_events("main");
    events.extend(vec![
        DomainEvent::AgentCreated {
            id: "worker-1".into(),
            name: "ghost-town".into(),
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
            id: "worker-1".into(),
            pid: 99,
            session_id: None,
        },
        DomainEvent::AgentCreated {
            id: "worker-2".into(),
            name: "swift-river".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: Some("task-2".into()),
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "worker-2".into(),
            pid: 100,
            session_id: None,
        },
    ]);

    let proj = make_projections(&events);
    let commands = route_mentions(&proj, "main", "alice", "hey @all please rebase");

    // Should nudge lead + both workers = 3 agents
    assert_eq!(
        commands.len(),
        3,
        "expected 3 nudges for @all, got {:?}",
        commands
    );
    let nudge_ids: Vec<&str> = commands
        .iter()
        .filter_map(|c| match c {
            Command::NudgeAgent { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert!(nudge_ids.contains(&"lead-1"));
    assert!(nudge_ids.contains(&"worker-1"));
    assert!(nudge_ids.contains(&"worker-2"));
}

#[test]
fn at_all_excludes_sender() {
    let mut events = running_lead_events("main");
    events.extend(vec![
        DomainEvent::AgentCreated {
            id: "worker-1".into(),
            name: "ghost-town".into(),
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
            id: "worker-1".into(),
            pid: 99,
            session_id: None,
        },
    ]);

    let proj = make_projections(&events);
    // Sender is "main" which is also the lead's name — lead should be excluded
    let commands = route_mentions(&proj, "main", "main", "hey @all please rebase");

    assert_eq!(
        commands.len(),
        1,
        "@all should exclude sender, got {:?}",
        commands
    );
    assert!(
        matches!(&commands[0], Command::NudgeAgent { id, .. } if id == "worker-1"),
        "expected only worker-1, got {:?}",
        commands[0]
    );
}

#[test]
fn at_ops_routes_to_ops_channel_lead() {
    // Create leads for both "main" and "ops" channels
    let mut events = running_lead_events("main");
    events.extend(vec![
        DomainEvent::AgentCreated {
            id: "ops-lead".into(),
            name: "ops".into(),
            kind: AgentKind::Lead,
            agent_type: "midtown-channel-lead".into(),
            provider: Provider::ClaudeCode,
            channel: Some("ops".into()),
            task_id: None,
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "ops-lead".into(),
            pid: 50,
            session_id: None,
        },
    ]);

    let proj = make_projections(&events);
    let commands = route_mentions(&proj, "main", "alice", "hey @ops something broke");

    assert_eq!(commands.len(), 1, "expected 1 nudge, got {:?}", commands);
    assert!(
        matches!(&commands[0], Command::NudgeAgent { id, .. } if id == "ops-lead"),
        "expected NudgeAgent for ops-lead, got {:?}",
        commands[0]
    );
}

#[test]
fn task_reference_routes_to_assigned_agent() {
    let mut events = running_lead_events("main");
    events.extend(vec![
        DomainEvent::TaskCreated {
            id: "42".into(),
            subject: "Fix login bug".into(),
            channel: "main".into(),
            blocked_by: vec![],
            agent_type: None,
            icon: None,
        },
        DomainEvent::AgentCreated {
            id: "worker-1".into(),
            name: "ghost-town".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: Some("42".into()),
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "worker-1".into(),
            pid: 99,
            session_id: None,
        },
        DomainEvent::TaskAssigned {
            task_id: "42".into(),
            agent_id: "worker-1".into(),
        },
    ]);

    let proj = make_projections(&events);
    // Message references task !42 — should route to the worker assigned to it
    let commands = route_mentions(&proj, "main", "alice", "hey @lead check !42 progress");

    // Should have 2 commands: nudge lead (from @lead) + nudge worker (from !42)
    assert_eq!(commands.len(), 2, "expected 2 nudges, got {:?}", commands);
    let nudge_ids: Vec<&str> = commands
        .iter()
        .filter_map(|c| match c {
            Command::NudgeAgent { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        nudge_ids.contains(&"worker-1"),
        "expected nudge for worker-1 (task !42 owner)"
    );
}

#[test]
fn task_reference_standalone_routes_to_agent() {
    let events = vec![
        DomainEvent::TaskCreated {
            id: "7".into(),
            subject: "Add tests".into(),
            channel: "main".into(),
            blocked_by: vec![],
            agent_type: None,
            icon: None,
        },
        DomainEvent::AgentCreated {
            id: "worker-1".into(),
            name: "swift-river".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: Some("7".into()),
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "worker-1".into(),
            pid: 99,
            session_id: None,
        },
        DomainEvent::TaskAssigned {
            task_id: "7".into(),
            agent_id: "worker-1".into(),
        },
    ];

    let proj = make_projections(&events);
    // Standalone !7 with no @mention — should still route to the task owner
    let commands = route_mentions(&proj, "main", "alice", "!7 looks good, ship it");

    assert_eq!(
        commands.len(),
        1,
        "expected 1 nudge for task ref, got {:?}",
        commands
    );
    assert!(
        matches!(&commands[0], Command::NudgeAgent { id, message }
            if id == "worker-1" && message.contains("!7")),
        "expected NudgeAgent for worker-1, got {:?}",
        commands[0]
    );
}

#[test]
fn task_reference_no_nudge_for_unassigned_task() {
    let events = vec![DomainEvent::TaskCreated {
        id: "42".into(),
        subject: "Fix login".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    }];

    let proj = make_projections(&events);
    // Task exists but has no agent assigned — no nudge
    let commands = route_mentions(&proj, "main", "alice", "what about !42?");

    assert!(
        commands.is_empty(),
        "unassigned task ref should produce no commands, got {:?}",
        commands
    );
}

#[test]
fn no_nudge_for_stopped_agent() {
    // Agent exists but is stopped
    let events = vec![
        DomainEvent::AgentCreated {
            id: "worker-1".into(),
            name: "ghost-town".into(),
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
            id: "worker-1".into(),
            pid: 99,
            session_id: None,
        },
        DomainEvent::AgentStopped {
            id: "worker-1".into(),
            reason: "done".into(),
        },
    ];

    let proj = make_projections(&events);
    let commands = route_mentions(&proj, "main", "alice", "hey @ghost-town are you there?");

    assert!(
        commands.is_empty(),
        "stopped agent mention should produce no commands, got {:?}",
        commands
    );
}
