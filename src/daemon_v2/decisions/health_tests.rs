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
            subject: "Do something".into(),
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
        DomainEvent::AgentStopped {
            id: "a1".into(),
            reason: "process died".into(),
        },
    ];

    let proj = make_projections(&events);
    let commands = check_dead_workers(&proj);

    // Per spec 2.2: dead worker with no session_id → spawn replacement
    assert_eq!(commands.len(), 1, "expected 1 command, got {:?}", commands);
    assert!(
        matches!(&commands[0], Command::SpawnAgent(cfg) if cfg.task_id.as_deref() == Some("task-1")),
        "expected SpawnAgent replacement for task-1, got {:?}",
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
            subject: "Do something".into(),
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
fn check_idle_workers_stops_long_running_taskless_worker() {
    use chrono::{Duration, Utc};

    // Worker created and started — simulate a started_at that is >5 min ago
    // by applying AgentStarted and then manually back-dating started_at.
    let events = vec![
        DomainEvent::AgentCreated {
            id: "w1".into(),
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
            id: "w1".into(),
            pid: 200,
            session_id: None,
        },
    ];

    let mut proj = Projections::default();
    proj.apply_all(&events);

    // Back-date started_at to 10 minutes ago so the worker is past the cutoff.
    let agent = proj.agents.by_id.get_mut("w1").unwrap();
    agent.started_at = Some(Utc::now() - Duration::minutes(10));

    let commands = check_idle_workers(&proj);

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], Command::StopAgent { id, reason } if id == "w1" && reason == "idle worker"),
        "expected StopAgent for w1, got {:?}",
        commands[0]
    );
}

#[test]
fn check_idle_workers_ignores_recent_taskless_worker() {
    // Worker started < 5 min ago — should NOT be stopped.
    let events = vec![
        DomainEvent::AgentCreated {
            id: "w2".into(),
            name: "new-worker".into(),
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
            id: "w2".into(),
            pid: 201,
            session_id: None,
        },
    ];

    let proj = make_projections(&events);
    // started_at is set to Utc::now() by AgentStarted — within the 5-min window
    let commands = check_idle_workers(&proj);
    assert!(
        commands.is_empty(),
        "expected no commands for recently started worker, got {:?}",
        commands
    );
}

#[test]
fn check_idle_workers_ignores_worker_with_task() {
    use chrono::{Duration, Utc};

    let events = vec![
        DomainEvent::AgentCreated {
            id: "w3".into(),
            name: "busy-worker".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: Some("task-42".into()),
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "w3".into(),
            pid: 202,
            session_id: None,
        },
        DomainEvent::TaskCreated {
            id: "task-42".into(),
            subject: "Do work".into(),
            channel: "main".into(),
            blocked_by: vec![],
            agent_type: None,
            agent_name: None,
            icon: None,
            color: None,
            parent: None,
        },
        DomainEvent::TaskAssigned {
            task_id: "task-42".into(),
            agent_id: "w3".into(),
        },
    ];

    let mut proj = Projections::default();
    proj.apply_all(&events);

    // Back-date started_at so it would be stopped if taskless.
    let agent = proj.agents.by_id.get_mut("w3").unwrap();
    agent.started_at = Some(Utc::now() - Duration::minutes(10));

    let commands = check_idle_workers(&proj);
    assert!(
        commands.is_empty(),
        "expected no commands for worker with task, got {:?}",
        commands
    );
}

#[test]
fn ensure_channel_leads_alive_spawns_for_all_active_channels() {
    // Two channels exist (via messages), no leads running for either
    let events = vec![
        DomainEvent::MessagePosted {
            id: "msg-1".into(),
            channel: "main".into(),
            sender: "alice".into(),
            content: "hello".into(),
            thread_id: None,
        },
        DomainEvent::MessagePosted {
            id: "msg-2".into(),
            channel: "backend".into(),
            sender: "bob".into(),
            content: "hello".into(),
            thread_id: None,
        },
    ];

    let proj = make_projections(&events);
    let commands = ensure_channel_leads_alive(&proj, "main");

    // Should spawn leads for both "main" and "backend"
    assert_eq!(
        commands.len(),
        2,
        "expected 2 SpawnAgent, got {:?}",
        commands
    );
    let channels: Vec<Option<&str>> = commands
        .iter()
        .filter_map(|c| match c {
            Command::SpawnAgent(cfg) => Some(cfg.channel.as_deref()),
            _ => None,
        })
        .collect();
    assert!(channels.contains(&Some("main")));
    assert!(channels.contains(&Some("backend")));
}

#[test]
fn ensure_channel_leads_alive_skips_archived() {
    use crate::daemon_v2::events::DomainEvent;

    // Channel "old" is archived — should not spawn a lead for it
    let events = vec![
        DomainEvent::MessagePosted {
            id: "msg-1".into(),
            channel: "main".into(),
            sender: "alice".into(),
            content: "hi".into(),
            thread_id: None,
        },
        DomainEvent::MessagePosted {
            id: "msg-2".into(),
            channel: "old".into(),
            sender: "alice".into(),
            content: "hi".into(),
            thread_id: None,
        },
    ];

    let mut proj = make_projections(&events);
    // Mark "old" as archived
    proj.channels.channels.get_mut("old").unwrap().archived = true;

    let commands = ensure_channel_leads_alive(&proj, "main");

    // Only "main" should get a lead
    assert_eq!(
        commands.len(),
        1,
        "expected 1 SpawnAgent, got {:?}",
        commands
    );
    assert!(
        matches!(&commands[0], Command::SpawnAgent(cfg) if cfg.channel.as_deref() == Some("main")),
        "expected SpawnAgent for main, got {:?}",
        commands[0]
    );
}

#[test]
fn ensure_channel_leads_alive_skips_channels_with_running_lead() {
    // "main" has a running lead, "backend" does not
    let events = vec![
        DomainEvent::MessagePosted {
            id: "msg-1".into(),
            channel: "main".into(),
            sender: "alice".into(),
            content: "hi".into(),
            thread_id: None,
        },
        DomainEvent::MessagePosted {
            id: "msg-2".into(),
            channel: "backend".into(),
            sender: "bob".into(),
            content: "hi".into(),
            thread_id: None,
        },
        DomainEvent::AgentCreated {
            id: "lead-main".into(),
            name: "main".into(),
            kind: AgentKind::Lead,
            agent_type: "midtown-project-lead".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: None,
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "lead-main".into(),
            pid: 42,
            session_id: None,
        },
    ];

    let proj = make_projections(&events);
    let commands = ensure_channel_leads_alive(&proj, "main");

    // Only "backend" needs a new lead
    assert_eq!(
        commands.len(),
        1,
        "expected 1 SpawnAgent, got {:?}",
        commands
    );
    assert!(
        matches!(&commands[0], Command::SpawnAgent(cfg) if cfg.channel.as_deref() == Some("backend")),
        "expected SpawnAgent for backend, got {:?}",
        commands[0]
    );
}

#[test]
fn ensure_channel_leads_alive_uses_channel_directory() {
    // "frontend" channel has a directory setting — lead should get it as working_dir
    let events = vec![
        DomainEvent::MessagePosted {
            id: "msg-1".into(),
            channel: "frontend".into(),
            sender: "alice".into(),
            content: "hi".into(),
            thread_id: None,
        },
        DomainEvent::ChannelDirectorySet {
            channel: "frontend".into(),
            directory: Some("packages/web".into()),
        },
    ];

    let proj = make_projections(&events);
    let commands = ensure_channel_leads_alive(&proj, "main");

    // Default channel "main" + "frontend" channel = 2 commands
    assert_eq!(
        commands.len(),
        2,
        "expected 2 SpawnAgent (main + frontend), got {:?}",
        commands
    );
    // The frontend lead should have the directory as working_dir
    let frontend_cmd = commands
        .iter()
        .find(
            |c| matches!(c, Command::SpawnAgent(cfg) if cfg.channel.as_deref() == Some("frontend")),
        )
        .expect("should have SpawnAgent for frontend");
    assert!(
        matches!(frontend_cmd, Command::SpawnAgent(cfg)
            if cfg.working_dir.as_deref() == Some("packages/web")
            && cfg.agent_type == "midtown-channel-lead"),
        "expected SpawnAgent with working_dir packages/web, got {:?}",
        frontend_cmd
    );
}

#[test]
fn ensure_channel_leads_alive_uses_project_lead_for_default() {
    // Default channel should use "midtown-project-lead" agent type
    let events = vec![DomainEvent::MessagePosted {
        id: "msg-1".into(),
        channel: "main".into(),
        sender: "alice".into(),
        content: "hi".into(),
        thread_id: None,
    }];

    let proj = make_projections(&events);
    let commands = ensure_channel_leads_alive(&proj, "main");

    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], Command::SpawnAgent(cfg)
            if cfg.agent_type == "midtown-project-lead"),
        "default channel should use midtown-project-lead, got {:?}",
        commands[0]
    );
}
