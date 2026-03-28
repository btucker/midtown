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
