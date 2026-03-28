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
