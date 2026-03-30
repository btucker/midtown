use super::*;
use crate::daemon_v2::Projections;
use crate::daemon_v2::events::*;
use serde_json::json;
use std::path::Path;

fn test_channels_dir() -> &'static Path {
    Path::new("/tmp/midtown-rpc-test-nonexistent")
}

fn projections_with_agents() -> Projections {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::AgentCreated {
        id: "a1".into(),
        name: "ghost-town".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("task-1".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 1234,
        session_id: None,
    });
    proj.apply(&DomainEvent::AgentCreated {
        id: "a2".into(),
        name: "main-lead".into(),
        kind: AgentKind::Lead,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj
}

#[test]
fn status_returns_agent_counts() {
    let proj = projections_with_agents();
    let result = handlers::handle_status(&proj).unwrap();
    assert_eq!(result["agents"]["total"], 2);
    assert_eq!(result["agents"]["running"], 1);
}

#[test]
fn agent_list_returns_all_agents() {
    let proj = projections_with_agents();
    let result = handlers::handle_agent_list(&proj, None).unwrap();
    let agents = result.as_array().unwrap();
    assert_eq!(agents.len(), 2);
}

#[test]
fn agent_list_filters_by_kind() {
    let proj = projections_with_agents();
    let filter = Some(AgentFilter {
        kind: Some(AgentKind::Worker),
        running_only: false,
    });
    let result = handlers::handle_agent_list(&proj, filter).unwrap();
    let agents = result.as_array().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["name"], "ghost-town");
}

#[test]
fn agent_list_filters_running_only() {
    let proj = projections_with_agents();
    let filter = Some(AgentFilter {
        kind: None,
        running_only: true,
    });
    let result = handlers::handle_agent_list(&proj, filter).unwrap();
    let agents = result.as_array().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["name"], "ghost-town");
}

/// Spec 14: WHEN midtown status is called THEN the system SHALL return status
/// via the same RPC protocol
#[test]
fn dispatch_routes_status() {
    let proj = projections_with_agents();
    let request = json!({"jsonrpc": "2.0", "method": "status", "id": 1});
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert!(response["result"]["agents"]["total"].is_number());
    assert!(events.is_empty());
}

#[test]
fn dispatch_routes_agent_list() {
    let proj = projections_with_agents();
    let request = json!({"jsonrpc": "2.0", "method": "agent.list", "id": 2});
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert!(response["result"].is_array());
    assert!(events.is_empty());
}

#[test]
fn dispatch_unknown_method_returns_error() {
    let proj = Projections::default();
    let request = json!({"jsonrpc": "2.0", "method": "nonexistent", "id": 3});
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(response["error"]["code"], -32601);
    assert!(events.is_empty());
}

/// Spec 14: WHEN midtown task create is called THEN the system SHALL accept
/// the same parameters
#[test]
fn task_create_returns_events() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "task.create",
        "id": 10,
        "params": {
            "id": "task-42",
            "subject": "Fix the bug",
            "channel": "main",
            "blocked_by": ["task-41"]
        }
    });
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert_eq!(events.len(), 1);

    match &events[0] {
        DomainEvent::TaskCreated {
            id,
            subject,
            channel,
            blocked_by,
            ..
        } => {
            assert_eq!(id, "task-42");
            assert_eq!(subject, "Fix the bug");
            assert_eq!(channel, "main");
            assert_eq!(blocked_by, &vec!["task-41".to_string()]);
        }
        other => panic!("expected TaskCreated, got {:?}", other),
    }
}

#[test]
fn task_create_missing_params_returns_error() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "task.create",
        "id": 11
    });
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(response["error"]["code"], -32602);
    assert!(events.is_empty());
}

#[test]
fn task_create_missing_required_field_returns_error() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "task.create",
        "id": 12,
        "params": {
            "id": "task-42",
            "subject": "Fix it"
            // missing "channel"
        }
    });
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(response["error"]["code"], -32602);
    assert!(events.is_empty());
}

#[test]
fn channel_update_sets_lead_driven() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.update",
        "id": 20,
        "params": {
            "channel": "manual",
            "lead_driven": true
        }
    });
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert_eq!(events.len(), 1);

    match &events[0] {
        DomainEvent::ChannelLeadDrivenSet {
            channel,
            lead_driven,
        } => {
            assert_eq!(channel, "manual");
            assert!(*lead_driven);
        }
        other => panic!("expected ChannelLeadDrivenSet, got {:?}", other),
    }
}

#[test]
fn channel_update_missing_params_returns_error() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.update",
        "id": 21
    });
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(response["error"]["code"], -32602);
    assert!(events.is_empty());
}

#[test]
fn channel_update_no_known_fields_returns_empty_events() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.update",
        "id": 22,
        "params": {
            "channel": "manual"
        }
    });
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert!(events.is_empty());
}

/// Spec 10.1: channel.update sets directory
#[test]
fn channel_update_sets_directory() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.update",
        "id": 23,
        "params": {
            "channel": "docs",
            "directory": "packages/docs"
        }
    });
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert!(events.iter().any(|e| matches!(
        e,
        DomainEvent::ChannelDirectorySet {
            channel,
            directory: Some(dir),
        } if channel == "docs" && dir == "packages/docs"
    )));
}

#[test]
fn session_fork_returns_spawn_command() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "session.fork",
        "id": 30,
        "params": {
            "thread_parent_id": "thread-abc123",
            "channel": "web",
            "name": "investigate-bug",
            "message": "Look into the auth issue"
        }
    });
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert_eq!(response["result"]["forking"], true);
    assert!(events.is_empty());
    assert_eq!(commands.len(), 1);

    match &commands[0] {
        crate::daemon_v2::decisions::Command::SpawnAgent(cfg) => {
            assert_eq!(cfg.name, "investigate-bug");
            assert_eq!(cfg.kind, AgentKind::Fork);
            assert_eq!(cfg.agent_type, "midtown-channel-lead");
            assert_eq!(cfg.channel.as_deref(), Some("web"));
            assert_eq!(cfg.bound_thread_id.as_deref(), Some("thread-abc123"));
            assert_eq!(
                cfg.initial_prompt.as_deref(),
                Some("Look into the auth issue")
            );
        }
        other => panic!("expected SpawnAgent, got {:?}", other),
    }
}

#[test]
fn session_fork_returns_existing_running_fork() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::AgentCreated {
        id: "f1".into(),
        name: "fork-abc".into(),
        kind: AgentKind::Fork,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("web".into()),
        task_id: None,
        bound_thread_id: Some("thread-abc123".into()),
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "f1".into(),
        pid: 999,
        session_id: None,
    });

    let request = json!({
        "jsonrpc": "2.0",
        "method": "session.fork",
        "id": 31,
        "params": {
            "thread_parent_id": "thread-abc123",
            "channel": "web"
        }
    });
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert_eq!(response["result"]["existing"], true);
    assert_eq!(response["result"]["fork_id"], "f1");
    assert!(events.is_empty());
    assert!(commands.is_empty());
}

#[test]
fn session_fork_generates_name_from_thread_id() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "session.fork",
        "id": 32,
        "params": {
            "thread_parent_id": "thread-abc123def456",
            "channel": "web"
        }
    });
    let (response, _events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(commands.len(), 1);

    match &commands[0] {
        crate::daemon_v2::decisions::Command::SpawnAgent(cfg) => {
            assert_eq!(cfg.name, "fork-thread-a");
        }
        other => panic!("expected SpawnAgent, got {:?}", other),
    }
}

#[test]
fn session_fork_missing_params_returns_error() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "session.fork",
        "id": 33
    });
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(response["error"]["code"], -32602);
    assert!(events.is_empty());
    assert!(commands.is_empty());
}

// ── Spec 14: v1 compatibility alias tests ───────────────────────────────

/// Spec 14: WHEN v1 RPC methods are called THEN the system SHALL handle them
/// via compatibility aliases
#[test]
fn v1_ping_returns_pong() {
    let proj = Projections::default();
    let request = json!({"jsonrpc": "2.0", "method": "ping", "id": 100});
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"], "pong");
    assert!(events.is_empty());
}

/// Spec 14: v1 version alias
#[test]
fn v1_version_returns_info() {
    let proj = Projections::default();
    let request = json!({"jsonrpc": "2.0", "method": "version", "id": 101});
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["name"], "midtown");
    assert!(response["result"]["version"].is_string());
    assert_eq!(response["result"]["daemon"], "v2");
    assert!(events.is_empty());
}

#[test]
fn v1_snapshot_aliases_to_status() {
    let proj = projections_with_agents();
    let request = json!({"jsonrpc": "2.0", "method": "snapshot", "id": 102});
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert!(response["result"]["agents"]["total"].is_number());
    assert!(events.is_empty());
}

#[test]
fn v1_coworker_list_aliases_to_agent_list() {
    let proj = projections_with_agents();
    let request = json!({"jsonrpc": "2.0", "method": "coworker.list", "id": 103});
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert!(response["result"].is_array());
    assert_eq!(response["result"].as_array().unwrap().len(), 2);
    assert!(events.is_empty());
}

#[test]
fn v1_coworkers_status_aliases_to_agent_list() {
    let proj = projections_with_agents();
    let request = json!({"jsonrpc": "2.0", "method": "coworkers.status", "id": 104});
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert!(response["result"].is_array());
    assert!(events.is_empty());
}

#[test]
fn v1_lead_spawn_returns_ok() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "lead.spawn",
        "id": 105,
        "params": {"provider": "claude"}
    });
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert!(events.is_empty());
    assert!(commands.is_empty());
}

#[test]
fn v1_coworker_break_stops_agent() {
    let proj = projections_with_agents();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "coworker.break",
        "id": 106,
        "params": {"name": "ghost-town"}
    });
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert!(events.is_empty());
    assert_eq!(commands.len(), 1);
    match &commands[0] {
        crate::daemon_v2::decisions::Command::StopAgent { id, .. } => {
            assert_eq!(id, "a1");
        }
        other => panic!("expected StopAgent, got {:?}", other),
    }
}

#[test]
fn v1_coworker_break_unknown_agent_returns_error() {
    let proj = projections_with_agents();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "coworker.break",
        "id": 107,
        "params": {"name": "nonexistent"}
    });
    let (response, _events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(response["error"]["code"], -32001);
    assert!(commands.is_empty());
}

#[test]
fn v1_coworker_nudge_nudges_agent() {
    let proj = projections_with_agents();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "coworker.nudge",
        "id": 108,
        "params": {"name": "ghost-town", "message": "hurry up"}
    });
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert!(events.is_empty());
    assert_eq!(commands.len(), 1);
    match &commands[0] {
        crate::daemon_v2::decisions::Command::NudgeAgent { id, message } => {
            assert_eq!(id, "a1");
            assert_eq!(message, "hurry up");
        }
        other => panic!("expected NudgeAgent, got {:?}", other),
    }
}

#[test]
fn v1_task_done_completes_task() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "task.done",
        "id": 109,
        "params": {"id": "task-42"}
    });
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert_eq!(events.len(), 1);
    match &events[0] {
        DomainEvent::TaskCompleted { task_id } => {
            assert_eq!(task_id, "task-42");
        }
        other => panic!("expected TaskCompleted, got {:?}", other),
    }
}

/// Spec 10.1: task.done accepts numeric id
#[test]
fn task_done_accepts_numeric_id() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "task.done",
        "id": 120,
        "params": {"id": 42}
    });
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert_eq!(events.len(), 1);
    match &events[0] {
        DomainEvent::TaskCompleted { task_id } => {
            assert_eq!(task_id, "42", "numeric id should be converted to string");
        }
        other => panic!("expected TaskCompleted, got {:?}", other),
    }
}

/// Spec 10.1: channel.post generates routing commands
#[test]
fn channel_post_generates_routing_commands() {
    let mut proj = Projections::default();
    // Create a lead so routing can nudge it
    proj.apply(&DomainEvent::AgentCreated {
        id: "lead-1".into(),
        name: "main-lead".into(),
        kind: AgentKind::Lead,
        agent_type: "midtown-project-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "lead-1".into(),
        pid: 1000,
        session_id: Some("sess-lead".into()),
    });

    let dir = tempfile::TempDir::new().unwrap();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.post",
        "id": 121,
        "params": {
            "channel": "main",
            "sender": "user",
            "content": "hello lead"
        }
    });
    let (response, events, commands) = dispatch_request(request, &proj, dir.path());
    assert!(response["error"].is_null());
    // Should produce a MessagePosted event
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DomainEvent::MessagePosted { .. })),
        "channel.post should produce MessagePosted event, got {:?}",
        events
    );
    // Should produce routing commands (nudge the lead)
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, crate::daemon_v2::decisions::Command::NudgeAgent { .. })),
        "channel.post should generate NudgeAgent routing command, got {:?}",
        commands
    );
}

// Spec 10.2: ping → "pong" (tested in v1_ping_returns_pong)
// Spec 10.2: version → name, version, daemon: "v2" (tested in v1_version_returns_info)
// Spec 10.2: snapshot → aliases to status (tested in v1_snapshot_aliases_to_status)
// Spec 10.4: unknown method → -32601 (tested in dispatch_unknown_method_returns_error)
// Spec 10.4: missing params → -32602 (tested in task_create_missing_params_returns_error, etc.)
// Spec 10.4: resource not found → -32000/-32001 (tested in task_update_returns_error_for_missing_task, etc.)

#[test]
fn v1_prs_status_returns_prs() {
    let proj = Projections::default();
    let request = json!({"jsonrpc": "2.0", "method": "prs.status", "id": 110});
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert!(response["result"]["prs"].is_array());
    assert!(events.is_empty());
}

/// Spec 13: coworker.spawn without name generates adjective-noun name
#[test]
fn coworker_spawn_generates_adjective_noun_name() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "coworker.spawn",
        "id": 112,
        "params": { "channel": "main" }
    });
    let (response, _, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(commands.len(), 1);
    match &commands[0] {
        crate::daemon_v2::decisions::Command::SpawnAgent(cfg) => {
            assert!(
                cfg.name.contains('-'),
                "generated name should be adjective-noun: {}",
                cfg.name
            );
            // Should not start with "worker-" (old UUID format)
            assert!(
                !cfg.name.starts_with("worker-"),
                "name should use adjective-noun, not UUID: {}",
                cfg.name
            );
        }
        other => panic!("expected SpawnAgent, got {:?}", other),
    }
}

#[test]
fn v1_coworker_spawn_returns_spawn_command() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "coworker.spawn",
        "id": 111,
        "params": {
            "channel": "main",
            "prompt": "do the thing"
        }
    });
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert!(events.is_empty());
    assert_eq!(commands.len(), 1);
    match &commands[0] {
        crate::daemon_v2::decisions::Command::SpawnAgent(cfg) => {
            assert_eq!(cfg.kind, AgentKind::Worker);
            assert_eq!(cfg.channel.as_deref(), Some("main"));
            assert_eq!(cfg.initial_prompt.as_deref(), Some("do the thing"));
        }
        other => panic!("expected SpawnAgent, got {:?}", other),
    }
}

// ── task.list / task.update / pr.list / pr.action tests ─────────────────

fn projections_with_tasks_and_prs() -> Projections {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::TaskCreated {
        id: "task-1".into(),
        subject: "Fix the thing".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: Some("midtown-code-author".into()),
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    proj.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix-the-thing".into(),
        author: "ghost-town".into(),
    });
    proj
}

#[test]
fn task_list_returns_tasks() {
    let proj = projections_with_tasks_and_prs();
    let request = json!({"jsonrpc": "2.0", "method": "task.list", "id": 200});
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    let tasks = response["result"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"], "task-1");
    assert_eq!(tasks[0]["subject"], "Fix the thing");
    assert_eq!(tasks[0]["channel"], "main");
    assert!(events.is_empty());
    assert!(commands.is_empty());
}

#[test]
fn task_list_empty_when_no_tasks() {
    let proj = Projections::default();
    let request = json!({"jsonrpc": "2.0", "method": "task.list", "id": 201});
    let (response, events, _) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    let tasks = response["result"].as_array().unwrap();
    assert!(tasks.is_empty());
    assert!(events.is_empty());
}

#[test]
fn task_update_returns_ok_for_existing_task() {
    let proj = projections_with_tasks_and_prs();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "task.update",
        "id": 202,
        "params": {"id": "task-1", "subject": "Updated subject"}
    });
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert!(events.is_empty());
    assert!(commands.is_empty());
}

#[test]
fn task_update_returns_error_for_missing_task() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "task.update",
        "id": 203,
        "params": {"id": "nonexistent"}
    });
    let (response, events, _) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(response["error"]["code"], -32000);
    assert!(events.is_empty());
}

#[test]
fn task_update_missing_params_returns_error() {
    let proj = Projections::default();
    let request = json!({"jsonrpc": "2.0", "method": "task.update", "id": 204});
    let (response, events, _) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(response["error"]["code"], -32602);
    assert!(events.is_empty());
}

#[test]
fn pr_list_returns_prs() {
    let proj = projections_with_tasks_and_prs();
    let request = json!({"jsonrpc": "2.0", "method": "pr.list", "id": 210});
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    let prs = response["result"].as_array().unwrap();
    assert_eq!(prs.len(), 1);
    assert_eq!(prs[0]["number"], 42);
    assert_eq!(prs[0]["branch"], "fix-the-thing");
    assert_eq!(prs[0]["author"], "ghost-town");
    assert!(events.is_empty());
    assert!(commands.is_empty());
}

#[test]
fn pr_list_empty_when_no_prs() {
    let proj = Projections::default();
    let request = json!({"jsonrpc": "2.0", "method": "pr.list", "id": 211});
    let (response, events, _) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    let prs = response["result"].as_array().unwrap();
    assert!(prs.is_empty());
    assert!(events.is_empty());
}

#[test]
fn pr_action_merge_returns_merge_command() {
    let proj = projections_with_tasks_and_prs();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "pr.action",
        "id": 220,
        "params": {"action": "merge", "number": 42}
    });
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert!(events.is_empty());
    assert_eq!(commands.len(), 1);
    match &commands[0] {
        crate::daemon_v2::decisions::Command::MergePr { number } => {
            assert_eq!(*number, 42);
        }
        other => panic!("expected MergePr, got {:?}", other),
    }
}

#[test]
fn pr_action_comment_returns_post_comment_command() {
    let proj = projections_with_tasks_and_prs();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "pr.action",
        "id": 221,
        "params": {"action": "comment", "number": 42, "body": "LGTM!"}
    });
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert!(events.is_empty());
    assert_eq!(commands.len(), 1);
    match &commands[0] {
        crate::daemon_v2::decisions::Command::PostPrComment { number, body } => {
            assert_eq!(*number, 42);
            assert_eq!(body, "LGTM!");
        }
        other => panic!("expected PostPrComment, got {:?}", other),
    }
}

#[test]
fn pr_action_rerun_returns_rerun_ci_command() {
    let proj = projections_with_tasks_and_prs();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "pr.action",
        "id": 222,
        "params": {"action": "rerun", "number": 42, "run_id": 99}
    });
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert!(events.is_empty());
    assert_eq!(commands.len(), 1);
    match &commands[0] {
        crate::daemon_v2::decisions::Command::RerunCi { run_id } => {
            assert_eq!(*run_id, 99);
        }
        other => panic!("expected RerunCi, got {:?}", other),
    }
}

#[test]
fn pr_action_unknown_action_returns_error() {
    let proj = projections_with_tasks_and_prs();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "pr.action",
        "id": 223,
        "params": {"action": "explode", "number": 42}
    });
    let (response, _events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(response["error"]["code"], -32602);
    assert!(commands.is_empty());
}

#[test]
fn pr_action_unknown_pr_returns_error() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "pr.action",
        "id": 224,
        "params": {"action": "merge", "number": 999}
    });
    let (response, _events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(response["error"]["code"], -32000);
    assert!(commands.is_empty());
}

// ── original tests continued ────────────────────────────────────────────

#[test]
fn session_fork_missing_channel_returns_error() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "session.fork",
        "id": 34,
        "params": {
            "thread_parent_id": "thread-abc123"
        }
    });
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(response["error"]["code"], -32602);
    assert!(events.is_empty());
    assert!(commands.is_empty());
}

#[test]
fn channel_post_accepts_v1_params() {
    // v1 CLI sends "from" and "message", v2 expects "sender" and "content"
    let proj = Projections::default();
    let dir = tempfile::tempdir().unwrap();
    let channels_dir = dir.path();

    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.post",
        "id": 99,
        "params": {
            "from": "user",
            "message": "hello from v1 CLI",
            "channel": "main"
        }
    });

    let (response, events, _commands) = dispatch_request(request, &proj, channels_dir);
    assert!(
        response.get("error").is_none(),
        "v1-style params should not produce an error, got: {:?}",
        response
    );
    assert_eq!(events.len(), 1, "should produce MessagePosted event");
}

#[test]
fn channel_post_defaults_channel_when_missing() {
    let proj = Projections::default();
    let dir = tempfile::tempdir().unwrap();
    let channels_dir = dir.path();

    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.post",
        "id": 100,
        "params": {
            "from": "user",
            "message": "hello"
        }
    });

    let (response, _events, _commands) = dispatch_request(request, &proj, channels_dir);
    assert!(
        response.get("error").is_none(),
        "missing channel should use default, not error, got: {:?}",
        response
    );
}

/// Spec 4.5: WHEN a fork is spawned THEN it SHALL inherit the parent lead's
/// session context by using fork_from_session
#[test]
fn session_fork_inherits_parent_lead_session() {
    let mut proj = Projections::default();
    // Create a running lead with a session_id
    proj.apply(&DomainEvent::AgentCreated {
        id: "lead-1".into(),
        name: "web-lead".into(),
        kind: AgentKind::Lead,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("web".into()),
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "lead-1".into(),
        pid: 9000,
        session_id: Some("parent-sess-abc".into()),
    });

    let request = json!({
        "jsonrpc": "2.0",
        "method": "session.fork",
        "id": 40,
        "params": {
            "thread_parent_id": "thread-fork-test",
            "channel": "web",
            "message": "investigate this"
        }
    });
    let (_response, _events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(commands.len(), 1);

    match &commands[0] {
        crate::daemon_v2::decisions::Command::SpawnAgent(cfg) => {
            assert_eq!(
                cfg.fork_from_session.as_deref(),
                Some("parent-sess-abc"),
                "fork should inherit parent lead's session_id via fork_from_session"
            );
        }
        other => panic!("expected SpawnAgent, got {:?}", other),
    }
}

// Spec 4.5: fork returns existing fork (tested in session_fork_returns_existing_running_fork)
// Spec 11.3: message→content transformation (tested in daemon-v2-live.spec.js E2E)

/// Spec 5.3: channel.read with thread_parent_id returns only thread messages
#[test]
fn channel_read_with_thread_parent_id() {
    let proj = Projections::default();
    let dir = tempfile::TempDir::new().unwrap();

    // Post top-level messages
    crate::daemon_v2::executor::channel_io::post_message(
        dir.path(),
        "test",
        "alice",
        "top msg",
        None,
    )
    .unwrap();

    // Get the message ID
    let msgs =
        crate::daemon_v2::executor::channel_io::read_messages(dir.path(), "test", None).unwrap();
    let parent_id = msgs[0]["id"].as_str().unwrap().to_string();

    // Post a thread reply
    crate::daemon_v2::executor::channel_io::post_message(
        dir.path(),
        "test",
        "bob",
        "thread reply",
        Some(&parent_id),
    )
    .unwrap();

    // Read with thread_parent_id should return only thread messages
    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.read",
        "id": 300,
        "params": {
            "channel": "test",
            "thread_parent_id": parent_id,
        }
    });
    let (response, _, _) = dispatch_request(request, &proj, dir.path());
    assert!(response["error"].is_null());
    let results = response["result"].as_array().unwrap();
    assert_eq!(
        results.len(),
        2,
        "should return parent + reply, got {:?}",
        results
    );
}

/// Spec 5.3: channel.read without thread_parent_id excludes thread replies
#[test]
fn channel_read_excludes_thread_replies() {
    let proj = Projections::default();
    let dir = tempfile::TempDir::new().unwrap();

    crate::daemon_v2::executor::channel_io::post_message(
        dir.path(),
        "test",
        "alice",
        "top 1",
        None,
    )
    .unwrap();
    crate::daemon_v2::executor::channel_io::post_message(
        dir.path(),
        "test",
        "alice",
        "top 2",
        None,
    )
    .unwrap();

    let msgs =
        crate::daemon_v2::executor::channel_io::read_messages(dir.path(), "test", None).unwrap();
    let parent_id = msgs[0]["id"].as_str().unwrap().to_string();

    crate::daemon_v2::executor::channel_io::post_message(
        dir.path(),
        "test",
        "bob",
        "reply",
        Some(&parent_id),
    )
    .unwrap();

    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.read",
        "id": 301,
        "params": { "channel": "test" }
    });
    let (response, _, _) = dispatch_request(request, &proj, dir.path());
    assert!(response["error"].is_null());
    let results = response["result"].as_array().unwrap();
    assert_eq!(
        results.len(),
        2,
        "thread replies should be excluded, got {:?}",
        results
    );
}

/// Spec 2.2: coworker.report-state emits AgentStateReported event
#[test]
fn report_state_emits_event() {
    let proj = projections_with_agents();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "coworker.report-state",
        "id": 400,
        "params": {"name": "ghost-town", "state": "idle"}
    });
    let (response, events, _commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(events.len(), 1);
    match &events[0] {
        DomainEvent::AgentStateReported { id, state } => {
            assert_eq!(id, "a1");
            assert_eq!(state, "idle");
        }
        other => panic!("expected AgentStateReported, got {:?}", other),
    }
}

/// Spec 2.2: coworker.report-state with unknown agent returns error
#[test]
fn report_state_unknown_agent_returns_error() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "coworker.report-state",
        "id": 401,
        "params": {"name": "nonexistent", "state": "idle"}
    });
    let (response, events, _) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(response["error"]["code"], -32001);
    assert!(events.is_empty());
}

// ── Integration: channel.post → channel.read roundtrip ──────────────────

/// Spec 5.3 + 10.1: message posted via channel.post is readable via channel.read
#[test]
fn channel_post_then_read_roundtrip() {
    let proj = Projections::default();
    let dir = tempfile::TempDir::new().unwrap();

    // Post a message
    let post_request = json!({
        "jsonrpc": "2.0",
        "method": "channel.post",
        "id": 500,
        "params": {
            "channel": "roundtrip",
            "sender": "alice",
            "content": "hello roundtrip",
        }
    });
    let (post_response, events, _) = dispatch_request(post_request, &proj, dir.path());
    assert!(post_response["error"].is_null(), "post should succeed");
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], DomainEvent::MessagePosted { .. }));

    // Read it back
    let read_request = json!({
        "jsonrpc": "2.0",
        "method": "channel.read",
        "id": 501,
        "params": { "channel": "roundtrip" }
    });
    let (read_response, _, _) = dispatch_request(read_request, &proj, dir.path());
    assert!(read_response["error"].is_null(), "read should succeed");
    let messages = read_response["result"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "should have 1 message");
    assert_eq!(messages[0]["from"], "alice");
    assert_eq!(messages[0]["message"], "hello roundtrip");
}

/// Spec 5.3: channel.read with limit returns last N messages
#[test]
fn channel_read_with_limit() {
    let proj = Projections::default();
    let dir = tempfile::TempDir::new().unwrap();

    // Post 5 messages
    for i in 1..=5 {
        let request = json!({
            "jsonrpc": "2.0",
            "method": "channel.post",
            "id": i,
            "params": {
                "channel": "limit-test",
                "sender": "user",
                "content": format!("msg {i}"),
            }
        });
        let (response, _, _) = dispatch_request(request, &proj, dir.path());
        assert!(response["error"].is_null());
    }

    // Read with limit 2 — should get last 2
    let read_request = json!({
        "jsonrpc": "2.0",
        "method": "channel.read",
        "id": 100,
        "params": { "channel": "limit-test", "limit": 2 }
    });
    let (response, _, _) = dispatch_request(read_request, &proj, dir.path());
    assert!(response["error"].is_null());
    let messages = response["result"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "should return last 2 messages");
    assert_eq!(messages[0]["message"], "msg 4");
    assert_eq!(messages[1]["message"], "msg 5");
}

// ── task.prompt / task.handoff ───────────────────────────────────────────

/// Section 15: task.prompt sends a nudge to the task's assigned agent
#[test]
fn task_prompt_nudges_assigned_agent() {
    let proj = projections_with_agents();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "task.prompt",
        "id": 700,
        "params": { "id": "task-1", "message": "please check the tests" }
    });
    let (response, _, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            crate::daemon_v2::decisions::Command::NudgeAgent { id, message }
            if id == "a1" && message.contains("please check the tests")
        ),
        "should nudge assigned agent, got {:?}",
        commands[0]
    );
}

/// Section 15: task.prompt returns error when no agent assigned
#[test]
fn task_prompt_no_agent_returns_error() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::TaskCreated {
        id: "orphan".into(),
        subject: "No agent".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    let request = json!({
        "jsonrpc": "2.0",
        "method": "task.prompt",
        "id": 701,
        "params": { "id": "orphan", "message": "hello?" }
    });
    let (response, _, _) = dispatch_request(request, &proj, test_channels_dir());
    assert_eq!(response["error"]["code"], -32000);
}

/// Section 15: task.handoff stops current agent and spawns replacement
#[test]
fn task_handoff_stops_and_respawns() {
    let mut proj = projections_with_agents();
    proj.apply(&DomainEvent::TaskCreated {
        id: "task-1".into(),
        subject: "Fix the thing".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    let request = json!({
        "jsonrpc": "2.0",
        "method": "task.handoff",
        "id": 710,
        "params": { "id": "task-1", "message": "continue this work" }
    });
    let (response, events, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(
        response["error"].is_null(),
        "task.handoff error: {response}"
    );

    // Should reset the task
    assert!(events.iter().any(|e| matches!(
        e,
        DomainEvent::TaskReset { task_id, .. } if task_id == "task-1"
    )));

    // Should stop the old agent and spawn a new one
    assert!(
        commands.iter().any(
            |c| matches!(c, crate::daemon_v2::decisions::Command::StopAgent { id, .. } if id == "a1")
        ),
        "should stop old agent"
    );
    assert!(
        commands.iter().any(
            |c| matches!(c, crate::daemon_v2::decisions::Command::SpawnAgent(cfg) if cfg.task_id.as_deref() == Some("task-1"))
        ),
        "should spawn new agent for same task"
    );
}

// ── channel.create / channel.archive / channel.unarchive ────────────────

/// Spec 5.2 + 10.1: channel.create creates a new channel via RPC
#[test]
fn channel_create_via_rpc() {
    let proj = Projections::default();
    let dir = tempfile::TempDir::new().unwrap();

    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.create",
        "id": 600,
        "params": { "name": "new-chan" }
    });
    let (response, _, _) = dispatch_request(request, &proj, dir.path());
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["ok"], true);
    assert_eq!(response["result"]["channel"], "new-chan");

    // Verify the channel directory exists
    let ch_dir = dir.path().join("channels").join("new-chan");
    assert!(ch_dir.exists(), "channel directory should be created");
}

/// Spec 5.2: channel.archive renames directory with .archived suffix
#[test]
fn channel_archive_and_unarchive_via_rpc() {
    let proj = Projections::default();
    let dir = tempfile::TempDir::new().unwrap();

    // Create a channel first
    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.create",
        "id": 601,
        "params": { "name": "archive-rpc" }
    });
    let (response, _, _) = dispatch_request(request, &proj, dir.path());
    assert!(response["error"].is_null());

    let ch_dir = dir.path().join("channels").join("archive-rpc");
    let archived_dir = dir.path().join("channels").join("archive-rpc.archived");

    assert!(ch_dir.exists());
    assert!(!archived_dir.exists());

    // Archive
    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.archive",
        "id": 602,
        "params": { "channel": "archive-rpc" }
    });
    let (response, _, _) = dispatch_request(request, &proj, dir.path());
    assert!(response["error"].is_null());
    assert!(!ch_dir.exists(), "original dir should be gone");
    assert!(archived_dir.exists(), "archived dir should exist");

    // Unarchive
    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.unarchive",
        "id": 603,
        "params": { "channel": "archive-rpc" }
    });
    let (response, _, _) = dispatch_request(request, &proj, dir.path());
    assert!(response["error"].is_null());
    assert!(ch_dir.exists(), "restored dir should exist");
    assert!(!archived_dir.exists(), "archived dir should be gone");
}

// ── session.detach ──────────────────────────────────────────────────────

/// Section 15: session.detach stops agent by name
#[test]
fn session_detach_stops_agent() {
    let proj = projections_with_agents();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "session.detach",
        "id": 800,
        "params": { "name": "ghost-town" }
    });
    let (response, _, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], crate::daemon_v2::decisions::Command::StopAgent { id, .. } if id == "a1"),
        "session.detach should stop the agent"
    );
}

// ── channel.rename ──────────────────────────────────────────────────────

/// Section 15: channel.rename renames a channel directory
#[test]
fn channel_rename_via_rpc() {
    let proj = Projections::default();
    let dir = tempfile::TempDir::new().unwrap();

    // Create the source channel
    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.create",
        "id": 700,
        "params": { "name": "old-name" }
    });
    let (response, _, _) = dispatch_request(request, &proj, dir.path());
    assert!(response["error"].is_null());

    // Rename it
    let request = json!({
        "jsonrpc": "2.0",
        "method": "channel.rename",
        "id": 701,
        "params": { "old": "old-name", "new": "new-name" }
    });
    let (response, _, _) = dispatch_request(request, &proj, dir.path());
    assert!(
        response["error"].is_null(),
        "channel.rename error: {response}"
    );

    // Old should be gone, new should exist
    let old_dir = dir.path().join("channels").join("old-name");
    let new_dir = dir.path().join("channels").join("new-name");
    assert!(!old_dir.exists(), "old channel dir should be gone");
    assert!(new_dir.exists(), "new channel dir should exist");
}

// ── oneshot.execute ─────────────────────────────────────────────────────

/// Section 15: oneshot.execute spawns a one-off worker
#[test]
fn oneshot_execute_spawns_worker() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "oneshot.execute",
        "id": 900,
        "params": { "prompt": "echo hello" }
    });
    let (response, _, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null(), "oneshot error: {response}");
    assert!(response["result"]["ok"] == true);
    assert!(response["result"]["agent"].is_string());

    assert_eq!(commands.len(), 1);
    match &commands[0] {
        crate::daemon_v2::decisions::Command::SpawnAgent(cfg) => {
            assert_eq!(cfg.kind, AgentKind::Worker);
            assert_eq!(cfg.initial_prompt.as_deref(), Some("echo hello"));
            assert!(cfg.task_id.is_none(), "oneshot has no task");
            assert!(cfg.channel.is_none(), "oneshot gets DM channel");
        }
        other => panic!("expected SpawnAgent, got {:?}", other),
    }
}

// ── pr.merge shortcut ───────────────────────────────────────────────────

/// pr.merge is a shortcut for pr.action with action=merge
#[test]
fn pr_merge_shortcut() {
    let proj = projections_with_tasks_and_prs();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "pr.merge",
        "id": 950,
        "params": { "number": 42 }
    });
    let (response, _, commands) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null(), "pr.merge error: {response}");
    assert_eq!(commands.len(), 1);
    assert!(
        matches!(
            &commands[0],
            crate::daemon_v2::decisions::Command::MergePr { number } if *number == 42
        ),
        "should produce MergePr command"
    );
}

// ── reminder CRUD ───────────────────────────────────────────────────────

/// Section 15: reminder.create creates a reminder and returns its ID
#[test]
fn reminder_create_and_list() {
    let mut proj = Projections::default();
    let dir = tempfile::TempDir::new().unwrap();

    // Create a reminder
    let request = json!({
        "jsonrpc": "2.0",
        "method": "reminder.create",
        "id": 1000,
        "params": {
            "trigger": "all-work-merged",
            "message": "All PRs merged, time to release!"
        }
    });
    let (response, events, _) = dispatch_request(request, &proj, dir.path());
    assert!(response["error"].is_null(), "create error: {response}");
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], DomainEvent::ReminderCreated { .. }));

    // Apply the event
    for event in &events {
        proj.apply(event);
    }

    // List should show it
    let request = json!({
        "jsonrpc": "2.0",
        "method": "reminder.list",
        "id": 1001
    });
    let (response, _, _) = dispatch_request(request, &proj, dir.path());
    assert!(response["error"].is_null());
    let reminders = response["result"].as_array().unwrap();
    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0]["trigger"], "all-work-merged");
    assert_eq!(reminders[0]["message"], "All PRs merged, time to release!");
}

/// Section 15: reminder.cancel removes a reminder
#[test]
fn reminder_cancel_removes() {
    let mut proj = Projections::default();
    let dir = tempfile::TempDir::new().unwrap();

    // Create
    let request = json!({
        "jsonrpc": "2.0",
        "method": "reminder.create",
        "id": 1010,
        "params": { "trigger": "cron", "message": "Check status", "cron_expr": "0 * * * *" }
    });
    let (_, events, _) = dispatch_request(request, &proj, dir.path());
    for event in &events {
        proj.apply(event);
    }
    assert_eq!(proj.reminders.len(), 1);
    let reminder_id = proj.reminders[0].id.clone();

    // Cancel
    let request = json!({
        "jsonrpc": "2.0",
        "method": "reminder.cancel",
        "id": 1011,
        "params": { "id": reminder_id }
    });
    let (response, events, _) = dispatch_request(request, &proj, dir.path());
    assert!(response["error"].is_null());
    for event in &events {
        proj.apply(event);
    }
    assert!(proj.reminders.is_empty(), "reminder should be cancelled");
}

// ── workflow CRUD ───────────────────────────────────────────────────────

/// Section 15: workflow.set_state sets a key-value state on a channel
#[test]
fn workflow_set_state_and_list() {
    let mut proj = Projections::default();

    // Set workflow state
    let request = json!({
        "jsonrpc": "2.0",
        "method": "workflow.set_state",
        "id": 1100,
        "params": { "channel": "backend", "key": "phase", "state": "developing" }
    });
    let (response, events, _) = dispatch_request(request, &proj, test_channels_dir());
    assert!(
        response["error"].is_null(),
        "workflow.set_state error: {response}"
    );
    for event in &events {
        proj.apply(event);
    }

    // List should show it
    let request = json!({
        "jsonrpc": "2.0",
        "method": "workflow.list",
        "id": 1101
    });
    let (response, _, _) = dispatch_request(request, &proj, test_channels_dir());
    assert!(response["error"].is_null());
    let workflows = response["result"].as_array().unwrap();
    assert!(
        workflows.iter().any(|w| w["channel"] == "backend"
            && w["key"] == "phase"
            && w["state"] == "developing"),
        "workflow state should appear in list: {workflows:?}"
    );
}
