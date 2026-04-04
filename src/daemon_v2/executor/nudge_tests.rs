use super::*;
use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::Projections;
use crate::daemon_v2::projections::cooldowns::CooldownCategory;

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

/// Spec 1.4: WHEN nudge target is stopped AND has no session ID THEN the system
/// SHALL spawn a new agent with the same configuration and deliver the nudge
#[test]
fn stopped_agent_without_session_resolves_to_respawn() {
    let events = vec![
        DomainEvent::AgentCreated {
            id: "a1".into(),
            name: "ghost-town".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: Some("t1".into()),
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
        matches!(action, NudgeAction::RespawnAndDeliver { .. }),
        "stopped agent without session_id should resolve to RespawnAndDeliver, got {:?}",
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

/// Spec 1.4: nudge-triggered respawn should respect SpawnFailure cooldown.
/// A stopped lead with active cooldown should resolve to Drop, not RespawnAndDeliver.
#[test]
fn stopped_lead_with_active_cooldown_resolves_to_drop() {
    let events = vec![
        DomainEvent::AgentCreated {
            id: "lead-1".into(),
            name: "daemon-core".into(),
            kind: AgentKind::Lead,
            agent_type: "midtown-channel-lead".into(),
            provider: Provider::ClaudeCode,
            channel: Some("daemon-core".into()),
            task_id: None,
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "lead-1".into(),
            pid: 100,
            session_id: None,
        },
        DomainEvent::AgentStopped {
            id: "lead-1".into(),
            reason: "auth expired".into(),
        },
    ];
    let mut proj = make_projections(&events);

    // Record a spawn failure cooldown (lead key = channel name)
    proj.cooldowns
        .record(CooldownCategory::SpawnFailure, "daemon-core".to_string());

    let action = resolve_nudge_action("lead-1", &proj);
    assert!(
        matches!(action, NudgeAction::Drop),
        "stopped lead with active SpawnFailure cooldown should resolve to Drop, got {:?}",
        action
    );
}

/// Stopped worker with active spawn cooldown should also resolve to Drop.
#[test]
fn stopped_worker_with_active_cooldown_resolves_to_drop() {
    let events = vec![
        DomainEvent::AgentCreated {
            id: "w1".into(),
            name: "ghost-town".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("dm-ghost-town".into()),
            task_id: Some("t1".into()),
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "w1".into(),
            pid: 200,
            session_id: None,
        },
        DomainEvent::AgentStopped {
            id: "w1".into(),
            reason: "auth expired".into(),
        },
    ];
    let mut proj = make_projections(&events);

    // Record a spawn failure cooldown (worker key = task_id)
    proj.cooldowns
        .record(CooldownCategory::SpawnFailure, "t1".to_string());

    let action = resolve_nudge_action("w1", &proj);
    assert!(
        matches!(action, NudgeAction::Drop),
        "stopped worker with active SpawnFailure cooldown should resolve to Drop, got {:?}",
        action
    );
}

/// Spec 1.4: resume should also respect SpawnFailure cooldown.
/// A stopped agent WITH session_id but active cooldown should Drop, not ResumeAndDeliver.
/// This happens when auth errors preserve the session_id but the agent keeps dying.
#[test]
fn stopped_lead_with_session_id_and_active_cooldown_resolves_to_drop() {
    let events = vec![
        DomainEvent::AgentCreated {
            id: "lead-1".into(),
            name: "midtown".into(),
            kind: AgentKind::Lead,
            agent_type: "midtown-project-lead".into(),
            provider: Provider::ClaudeCode,
            channel: Some("midtown".into()),
            task_id: None,
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "lead-1".into(),
            pid: 100,
            session_id: Some("sess-expired".into()),
        },
        DomainEvent::AgentStopped {
            id: "lead-1".into(),
            reason: "auth expired".into(),
        },
    ];
    let mut proj = make_projections(&events);

    // Agent still has session_id (auth errors don't clear it)
    assert!(
        proj.agents
            .by_id
            .get("lead-1")
            .unwrap()
            .session_id
            .is_some(),
        "setup: agent should retain session_id after auth error stop"
    );

    // Record a spawn failure cooldown
    proj.cooldowns
        .record(CooldownCategory::SpawnFailure, "midtown".to_string());

    let action = resolve_nudge_action("lead-1", &proj);
    assert!(
        matches!(action, NudgeAction::Drop),
        "stopped lead with session_id AND active cooldown should Drop, got {:?}",
        action
    );
}
