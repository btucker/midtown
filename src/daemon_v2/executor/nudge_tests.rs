use super::*;
use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::Projections;

fn make_projections(events: &[DomainEvent]) -> Projections {
    let mut proj = Projections::default();
    proj.apply_all(events);
    proj
}

#[test]
fn running_agent_resolves_to_deliver() {
    let events = vec![
        DomainEvent::AgentCreated {
            id: "a1".into(),
            name: "ghost-town".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: None,
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "a1".into(),
            pid: 100,
            session_id: Some("sess-1".into()),
        },
    ];
    let proj = make_projections(&events);

    let action = resolve_nudge_action("a1", &proj);
    assert!(
        matches!(action, NudgeAction::Deliver),
        "running agent should resolve to Deliver, got {:?}",
        action
    );
}

#[test]
fn stopped_agent_with_session_resolves_to_resume() {
    let events = vec![
        DomainEvent::AgentCreated {
            id: "a1".into(),
            name: "ghost-town".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: None,
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "a1".into(),
            pid: 100,
            session_id: Some("sess-1".into()),
        },
        DomainEvent::AgentStopped {
            id: "a1".into(),
            reason: "crashed".into(),
        },
    ];
    let proj = make_projections(&events);

    let action = resolve_nudge_action("a1", &proj);
    assert!(
        matches!(action, NudgeAction::ResumeAndDeliver { .. }),
        "stopped agent with session_id should resolve to ResumeAndDeliver, got {:?}",
        action
    );
}

#[test]
fn stopped_agent_without_session_resolves_to_drop() {
    let events = vec![
        DomainEvent::AgentCreated {
            id: "a1".into(),
            name: "ghost-town".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: None,
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "a1".into(),
            pid: 100,
            session_id: None, // No session_id — can't resume
        },
        DomainEvent::AgentStopped {
            id: "a1".into(),
            reason: "crashed".into(),
        },
    ];
    let proj = make_projections(&events);

    let action = resolve_nudge_action("a1", &proj);
    assert!(
        matches!(action, NudgeAction::Drop),
        "stopped agent without session_id should resolve to Drop, got {:?}",
        action
    );
}

#[test]
fn unknown_agent_resolves_to_drop() {
    let proj = Projections::default();
    let action = resolve_nudge_action("nonexistent", &proj);
    assert!(
        matches!(action, NudgeAction::Drop),
        "unknown agent should resolve to Drop, got {:?}",
        action
    );
}
