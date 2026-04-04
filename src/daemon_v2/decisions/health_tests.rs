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
            thread_id: None,
            message_id: None,
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
            thread_id: None,
            message_id: None,
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
            thread_id: None,
            message_id: None,
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
            tool_data: None,
            auto_output: false,
        },
        DomainEvent::MessagePosted {
            id: "msg-2".into(),
            channel: "backend".into(),
            sender: "bob".into(),
            content: "hello".into(),
            thread_id: None,
            tool_data: None,
            auto_output: false,
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
            tool_data: None,
            auto_output: false,
        },
        DomainEvent::MessagePosted {
            id: "msg-2".into(),
            channel: "old".into(),
            sender: "alice".into(),
            content: "hi".into(),
            thread_id: None,
            tool_data: None,
            auto_output: false,
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
            tool_data: None,
            auto_output: false,
        },
        DomainEvent::MessagePosted {
            id: "msg-2".into(),
            channel: "backend".into(),
            sender: "bob".into(),
            content: "hi".into(),
            thread_id: None,
            tool_data: None,
            auto_output: false,
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
            tool_data: None,
            auto_output: false,
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
        tool_data: None,
        auto_output: false,
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

#[test]
fn ensure_channel_leads_resumes_stopped_lead_not_spawns_new() {
    // A stopped lead exists for "main" — should resume it, not spawn a new one
    let events = vec![
        DomainEvent::MessagePosted {
            id: "msg-1".into(),
            channel: "main".into(),
            sender: "alice".into(),
            content: "hi".into(),
            thread_id: None,
            tool_data: None,
            auto_output: false,
        },
        DomainEvent::AgentCreated {
            id: "lead-old".into(),
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
            id: "lead-old".into(),
            pid: 100,
            session_id: Some("sess-old".into()),
        },
        DomainEvent::AgentStopped {
            id: "lead-old".into(),
            reason: "crashed".into(),
        },
    ];

    let proj = make_projections(&events);
    let commands = ensure_channel_leads_alive(&proj, "main");

    // Should resume the existing lead, NOT spawn a new one
    assert_eq!(commands.len(), 1, "expected 1 command, got {:?}", commands);
    assert!(
        matches!(&commands[0], Command::ResumeAgent { id } if id == "lead-old"),
        "should ResumeAgent for existing stopped lead, not SpawnAgent; got {:?}",
        commands[0]
    );
}

#[test]
fn ensure_channel_leads_no_action_when_running() {
    // A running lead exists — should do nothing
    let events = vec![
        DomainEvent::AgentCreated {
            id: "lead-1".into(),
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
            id: "lead-1".into(),
            pid: 42,
            session_id: None,
        },
    ];

    let proj = make_projections(&events);
    let commands = ensure_channel_leads_alive(&proj, "main");
    assert!(
        commands.is_empty(),
        "running lead should need no action, got {:?}",
        commands
    );
}

#[test]
fn stop_idle_worker_after_60s() {
    let mut proj = make_projections(&[
        DomainEvent::AgentCreated {
            id: "idle-1".into(),
            name: "proving-ground".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: Some("task-99".into()),
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "idle-1".into(),
            pid: 1234,
            session_id: Some("sess-1".into()),
        },
        DomainEvent::AgentStateReported {
            id: "idle-1".into(),
            state: "idle".into(),
        },
    ]);

    // Backdate the idle report to 90s ago
    if let Some(agent) = proj.agents.by_id.get_mut("idle-1") {
        agent.state_reported_at = Some(chrono::Utc::now() - chrono::Duration::seconds(90));
    }

    let commands = stop_idle_reported_workers(&proj);
    assert_eq!(commands.len(), 1, "should stop worker idle for >60s");
}

#[test]
fn spare_idle_worker_within_60s() {
    let mut proj = make_projections(&[
        DomainEvent::AgentCreated {
            id: "idle-2".into(),
            name: "busy-bee".into(),
            kind: AgentKind::Worker,
            agent_type: "midtown-code-author".into(),
            provider: Provider::ClaudeCode,
            channel: Some("main".into()),
            task_id: Some("task-100".into()),
            bound_thread_id: None,
            icon: None,
            color: None,
        },
        DomainEvent::AgentStarted {
            id: "idle-2".into(),
            pid: 5678,
            session_id: Some("sess-2".into()),
        },
        DomainEvent::AgentStateReported {
            id: "idle-2".into(),
            state: "idle".into(),
        },
    ]);

    // Idle reported 30s ago — within the 60s window
    if let Some(agent) = proj.agents.by_id.get_mut("idle-2") {
        agent.state_reported_at = Some(chrono::Utc::now() - chrono::Duration::seconds(30));
    }

    let commands = stop_idle_reported_workers(&proj);
    assert!(
        commands.is_empty(),
        "should not stop worker idle for only 30s"
    );
}

/// Spec 4.4: worker spawn failure cooldown prevents respawn loop
#[test]
fn no_respawn_when_worker_spawn_cooldown_active() {
    use crate::daemon_v2::projections::cooldowns::CooldownCategory;

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
            thread_id: None,
            message_id: None,
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

    let mut proj = make_projections(&events);

    // Record a SpawnFailure cooldown for this task (simulating a worker that died quickly)
    proj.cooldowns
        .record(CooldownCategory::SpawnFailure, "task-1".to_string());

    let commands = check_dead_workers(&proj);

    // When cooldown is active, check_dead_workers should NOT respawn
    assert!(
        commands.is_empty(),
        "should not respawn worker when SpawnFailure cooldown is active, got: {:?}",
        commands
    );
}

/// Spec 2.2: WHEN a worker has failed 3 consecutive spawn attempts THEN stop retrying
/// and post to ops channel.
#[test]
fn worker_gives_up_after_max_spawn_failures() {
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
            session_id: Some("session-1".into()),
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
            thread_id: None,
            message_id: None,
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

    let mut proj = make_projections(&events);

    // Simulate 3 consecutive spawn failures (cooldown expired each time)
    for _ in 0..super::MAX_WORKER_RESTARTS {
        proj.cooldowns
            .record(CooldownCategory::SpawnFailure, "task-1".to_string());
    }

    // Manually expire the cooldown so check_dead_workers doesn't skip due to active cooldown
    proj.cooldowns
        .expire_for_test(CooldownCategory::SpawnFailure, "task-1");

    let commands = check_dead_workers(&proj);

    // Should NOT try to resume — max failures reached
    assert!(
        !commands
            .iter()
            .any(|c| matches!(c, Command::ResumeAgent { .. } | Command::SpawnAgent(_))),
        "should not respawn worker after {} failures, got: {:?}",
        super::MAX_WORKER_RESTARTS,
        commands,
    );

    // Should post to ops channel about the failure
    assert!(
        commands.iter().any(|c| matches!(c, Command::PostSystem {
            channel,
            ..
        } if channel == "ops")),
        "should escalate to ops after max failures, got: {:?}",
        commands,
    );
}

/// Spec 2.2: Workers below the max restart limit should still be respawned
/// after cooldown expires.
#[test]
fn worker_respawns_when_below_max_failures() {
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
            session_id: Some("session-1".into()),
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
            thread_id: None,
            message_id: None,
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

    let mut proj = make_projections(&events);

    // Only 2 failures (below the limit of 3)
    for _ in 0..2 {
        proj.cooldowns
            .record(CooldownCategory::SpawnFailure, "task-1".to_string());
    }

    // Expire the cooldown
    proj.cooldowns
        .expire_for_test(CooldownCategory::SpawnFailure, "task-1");

    let commands = check_dead_workers(&proj);

    // Should still try to resume since we're below max
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, Command::ResumeAgent { .. })),
        "should resume worker when below max failures, got: {:?}",
        commands,
    );
}

/// Spec 2.2: WHEN a resumed agent's session_id is cleared (stale session)
/// THEN check_dead_workers should spawn a fresh replacement instead of resuming.
#[test]
fn cleared_session_id_causes_fresh_spawn() {
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
            session_id: Some("stale-session-id".into()),
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
            thread_id: None,
            message_id: None,
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

    let mut proj = make_projections(&events);

    // Simulate the daemon clearing the stale session_id
    // (This happens in daemon.rs when agent dies within 5s of start)
    proj.agents.by_id.get_mut("a1").unwrap().session_id = None;

    let commands = check_dead_workers(&proj);

    // Should spawn fresh (SpawnAgent) instead of resume
    assert_eq!(commands.len(), 1, "expected 1 command, got {:?}", commands);
    assert!(
        commands.iter().any(|c| matches!(c, Command::SpawnAgent(_))),
        "should spawn fresh after session_id cleared, got: {:?}",
        commands,
    );
}

/// Spec 4.4: WHEN a lead has failed 3 consecutive spawn attempts THEN stop
/// retrying AND post to ops channel (mirrors worker behavior from Spec 2.2).
#[test]
fn lead_gives_up_after_max_spawn_failures() {
    let events = vec![
        // Main channel lead (running — not the subject of this test)
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
            pid: 50,
            session_id: Some("session-main".into()),
        },
        // daemon-core lead (stopped — subject of this test)
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
            session_id: Some("session-1".into()),
        },
        DomainEvent::AgentStopped {
            id: "lead-1".into(),
            reason: "process died".into(),
        },
    ];

    let mut proj = make_projections(&events);

    // Ensure the channel exists in the projection
    proj.apply(&DomainEvent::MessagePosted {
        id: "msg-1".into(),
        channel: "daemon-core".into(),
        sender: "user".into(),
        content: "hello".into(),
        thread_id: None,
        tool_data: None,
        auto_output: false,
    });

    // Simulate 3 consecutive spawn failures
    for _ in 0..super::MAX_LEAD_RESTARTS {
        proj.cooldowns
            .record(CooldownCategory::SpawnFailure, "daemon-core".to_string());
    }

    // Expire the cooldown timer so ensure_channel_leads_alive doesn't skip
    proj.cooldowns
        .expire_for_test(CooldownCategory::SpawnFailure, "daemon-core");

    let commands = ensure_channel_leads_alive(&proj, "main");

    // Should NOT try to resume/spawn — max failures reached
    assert!(
        !commands
            .iter()
            .any(|c| matches!(c, Command::ResumeAgent { .. } | Command::SpawnAgent(_))),
        "should not respawn lead after {} failures, got: {:?}",
        super::MAX_LEAD_RESTARTS,
        commands,
    );

    // Should post to ops channel about the failure
    assert!(
        commands.iter().any(|c| matches!(c, Command::PostSystem {
            channel,
            ..
        } if channel == "ops")),
        "should escalate to ops after max lead failures, got: {:?}",
        commands,
    );
}
