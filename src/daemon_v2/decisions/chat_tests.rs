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

fn worker_events(id: &str, name: &str, channel: &str, task_id: Option<&str>) -> Vec<DomainEvent> {
    vec![
        DomainEvent::AgentCreated {
            id: id.into(),
            name: name.into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some(channel.into()),
            task_id: task_id.map(String::from),
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: id.into(),
            pid: 99,
            session_id: None,
        },
    ]
}

// ── route_message: channel lead nudging ─────────────────────────────

#[test]
fn channel_lead_nudged_on_every_message() {
    let proj = make_projections(&running_lead_events("main"));
    let commands = route_message(&proj, "main", "alice", "just chatting", None);

    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "lead-1")),
        "lead nudged on plain message, got {:?}",
        commands
    );
}

#[test]
fn stopped_lead_still_nudged() {
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
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "lead-1".into(),
            pid: 42,
            session_id: None,
        },
        DomainEvent::AgentStopped {
            id: "lead-1".into(),
            reason: "crashed".into(),
        },
    ];

    let proj = make_projections(&events);
    let commands = route_message(&proj, "main", "alice", "hey lead", None);

    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "lead-1")),
        "stopped lead should still be nudged (executor resumes), got {:?}",
        commands
    );
}

#[test]
fn no_self_nudge_for_lead() {
    let proj = make_projections(&running_lead_events("main"));
    let commands = route_message(&proj, "main", "main", "I'm posting", None);

    assert!(
        !commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "lead-1")),
        "lead should not self-nudge, got {:?}",
        commands
    );
}

// ── route_message: thread / fork routing ────────────────────────────

#[test]
fn thread_bound_agent_nudged_on_thread_reply() {
    let mut events = running_lead_events("main");
    events.extend(vec![
        DomainEvent::AgentCreated {
            id: "fork-1".into(),
            name: "fork-abc".into(),
            kind: AgentKind::Fork,
            agent_type: "midtown-channel-lead".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: None,
            bound_thread_id: Some("thread-abc".into()),
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "fork-1".into(),
            pid: 88,
            session_id: None,
        },
    ]);

    let proj = make_projections(&events);
    let commands = route_message(&proj, "main", "alice", "follow-up", Some("thread-abc"));

    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "fork-1")),
        "thread-bound agent should be nudged, got {:?}",
        commands
    );
}

#[test]
fn stopped_thread_bound_agent_still_nudged() {
    let mut events = running_lead_events("main");
    events.extend(vec![
        DomainEvent::AgentCreated {
            id: "fork-1".into(),
            name: "fork-abc".into(),
            kind: AgentKind::Fork,
            agent_type: "midtown-channel-lead".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: None,
            bound_thread_id: Some("thread-abc".into()),
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "fork-1".into(),
            pid: 88,
            session_id: None,
        },
        DomainEvent::AgentStopped {
            id: "fork-1".into(),
            reason: "done".into(),
        },
    ]);

    let proj = make_projections(&events);
    let commands = route_message(&proj, "main", "alice", "follow-up", Some("thread-abc"));

    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "fork-1")),
        "stopped thread-bound agent should still be nudged, got {:?}",
        commands
    );
}

#[test]
fn thread_without_bound_agent_falls_through_to_lead() {
    let proj = make_projections(&running_lead_events("main"));
    let commands = route_message(&proj, "main", "alice", "reply", Some("thread-xyz"));

    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "lead-1")),
        "lead should be nudged when no agent bound to thread, got {:?}",
        commands
    );
}

#[test]
fn thread_bound_agent_self_post_no_nudge() {
    let mut events = running_lead_events("main");
    events.push(DomainEvent::AgentCreated {
        id: "fork-1".into(),
        name: "fork-abc".into(),
        kind: AgentKind::Fork,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: Some("thread-abc".into()),
        icon: None,
        color: None,
    });
    events.push(DomainEvent::AgentStarted {
        id: "fork-1".into(),
        pid: 88,
        session_id: None,
    });

    let proj = make_projections(&events);
    let commands = route_message(&proj, "main", "fork-abc", "responding", Some("thread-abc"));

    assert!(
        !commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "fork-1")),
        "thread-bound agent should not self-nudge, got {:?}",
        commands
    );
}

// ── route_message: @mentions ────────────────────────────────────────

#[test]
fn at_mention_nudges_agent_by_name() {
    let mut events = running_lead_events("main");
    events.extend(worker_events("w1", "ghost-town", "main", Some("t1")));

    let proj = make_projections(&events);
    let commands = route_message(&proj, "main", "alice", "hey @ghost-town check this", None);

    let nudge_ids: Vec<&str> = commands
        .iter()
        .filter_map(|c| match c {
            Command::NudgeAgent { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert!(nudge_ids.contains(&"lead-1"), "lead nudged");
    assert!(nudge_ids.contains(&"w1"), "mentioned agent nudged");
}

#[test]
fn at_mention_nudges_stopped_agent() {
    let mut events = running_lead_events("main");
    events.extend(worker_events("w1", "ghost-town", "main", Some("t1")));
    events.push(DomainEvent::AgentStopped {
        id: "w1".into(),
        reason: "done".into(),
    });

    let proj = make_projections(&events);
    let commands = route_message(
        &proj,
        "main",
        "alice",
        "hey @ghost-town are you there?",
        None,
    );

    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "w1")),
        "stopped agent should still be nudged on @mention, got {:?}",
        commands
    );
}

#[test]
fn at_all_nudges_all_channel_agents() {
    let mut events = running_lead_events("main");
    events.extend(worker_events("w1", "ghost-town", "main", Some("t1")));
    events.extend(worker_events("w2", "swift-river", "main", Some("t2")));

    let proj = make_projections(&events);
    let commands = route_message(&proj, "main", "alice", "@all please rebase", None);

    let nudge_ids: Vec<&str> = commands
        .iter()
        .filter_map(|c| match c {
            Command::NudgeAgent { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert!(nudge_ids.contains(&"lead-1"));
    assert!(nudge_ids.contains(&"w1"));
    assert!(nudge_ids.contains(&"w2"));
}

#[test]
fn at_all_excludes_sender() {
    let mut events = running_lead_events("main");
    events.extend(worker_events("w1", "ghost-town", "main", Some("t1")));

    let proj = make_projections(&events);
    // Sender is "main" — same as lead's name
    let commands = route_message(&proj, "main", "main", "@all please rebase", None);

    assert!(
        !commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "lead-1")),
        "@all should exclude sender, got {:?}",
        commands
    );
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "w1")),
        "other agents still nudged",
    );
}

#[test]
fn at_lead_nudges_channel_lead() {
    let proj = make_projections(&running_lead_events("main"));
    let commands = route_message(&proj, "main", "alice", "hey @lead help", None);

    // Lead gets nudged (from both the implicit channel nudge and @lead,
    // but deduplication means only one NudgeAgent)
    let lead_nudges: Vec<_> = commands
        .iter()
        .filter(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "lead-1"))
        .collect();
    assert_eq!(
        lead_nudges.len(),
        1,
        "lead should be nudged exactly once (deduped), got {:?}",
        commands
    );
}

#[test]
fn self_mention_ignored() {
    let proj = make_projections(&running_lead_events("main"));
    let commands = route_message(&proj, "main", "ghost-town", "hey @ghost-town oops", None);

    assert!(
        !commands
            .iter()
            .any(|c| matches!(c, Command::NudgeAgent { id, message }
                if message.contains("@ghost-town mention"))),
        "self-mention should not produce a mention nudge, got {:?}",
        commands
    );
}

#[test]
fn unknown_mention_no_nudge() {
    let proj = make_projections(&running_lead_events("main"));
    let commands = route_message(&proj, "main", "alice", "hey @nobody-exists", None);

    // Should only have the implicit lead nudge, not one for @nobody-exists
    assert_eq!(
        commands.len(),
        1,
        "only lead nudge expected, got {:?}",
        commands
    );
}

// ── route_message: !N task references ───────────────────────────────

#[test]
fn task_ref_nudges_assigned_agent() {
    let mut events = running_lead_events("main");
    events.push(DomainEvent::TaskCreated {
        id: "42".into(),
        subject: "Fix bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    });
    events.extend(worker_events("w1", "ghost-town", "main", Some("42")));
    events.push(DomainEvent::TaskAssigned {
        task_id: "42".into(),
        agent_id: "w1".into(),
    });

    let proj = make_projections(&events);
    let commands = route_message(&proj, "main", "alice", "check !42 progress", None);

    let nudge_ids: Vec<&str> = commands
        .iter()
        .filter_map(|c| match c {
            Command::NudgeAgent { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert!(nudge_ids.contains(&"w1"), "task owner nudged");
    assert!(nudge_ids.contains(&"lead-1"), "lead also nudged");
}

#[test]
fn task_ref_no_nudge_for_unassigned() {
    let events = vec![DomainEvent::TaskCreated {
        id: "42".into(),
        subject: "Fix".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
    }];

    let proj = make_projections(&events);
    let commands = route_message(&proj, "main", "alice", "what about !42?", None);

    assert!(
        commands.is_empty(),
        "unassigned task + no lead = no nudges, got {:?}",
        commands
    );
}

// ── deduplication ───────────────────────────────────────────────────

#[test]
fn no_duplicate_nudges() {
    let proj = make_projections(&running_lead_events("main"));
    // @lead + @main both resolve to lead-1, plus implicit channel lead nudge
    let commands = route_message(&proj, "main", "alice", "@lead @main do something", None);

    let lead_nudges = commands
        .iter()
        .filter(|c| matches!(c, Command::NudgeAgent { id, .. } if id == "lead-1"))
        .count();
    assert_eq!(
        lead_nudges, 1,
        "lead nudged exactly once, got {:?}",
        commands
    );
}
