use std::path::Path;

use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::Projections;
use crate::daemon_v2::rpc;
use serde_json::json;
use tempfile::TempDir;

fn test_channels_dir() -> &'static Path {
    Path::new("/tmp/midtown-integration-test-nonexistent")
}

#[test]
fn full_lifecycle_through_store_and_projections() {
    let dir = TempDir::new().unwrap();
    let mut store = EventStore::new(dir.path().join("events"));
    let mut proj = Projections::default();

    // 1. Create a task
    let e1 = DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix auth bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    };
    store.append(&e1).unwrap();
    proj.apply(&e1);

    // 2. Create an agent
    let e2 = DomainEvent::AgentCreated {
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
    };
    store.append(&e2).unwrap();
    proj.apply(&e2);

    // 3. Start the agent
    let e3 = DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 5678,
        session_id: None,
    };
    store.append(&e3).unwrap();
    proj.apply(&e3);

    // Verify state via RPC
    let (status, _, _) = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 1}),
        &proj,
        test_channels_dir(),
    );
    assert_eq!(status["result"]["agents"]["running"], 1);
    // tasks is now an array for the kanban board
    let tasks = status["result"]["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["status"], "pending");

    // 4. Assign task
    let e4 = DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    };
    store.append(&e4).unwrap();
    proj.apply(&e4);

    let (status, _, _) = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 2}),
        &proj,
        test_channels_dir(),
    );
    let tasks = status["result"]["tasks"].as_array().unwrap();
    assert!(tasks.iter().all(|t| t["status"] != "pending"));
    assert!(tasks.iter().any(|t| t["status"] == "in_progress"));

    // 5. Snapshot and recover
    store.save_snapshot(&proj).unwrap();

    let e5 = DomainEvent::TaskCompleted {
        task_id: "t1".into(),
    };
    store.append(&e5).unwrap();

    let (recovered_store, snapshot, replay_events) =
        EventStore::recover(dir.path().join("events")).unwrap();
    assert_eq!(recovered_store.sequence(), 5);

    let mut recovered_proj = snapshot.unwrap();
    recovered_proj.apply_all(&replay_events);

    let (status, _, _) = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 3}),
        &recovered_proj,
        test_channels_dir(),
    );
    let tasks = status["result"]["tasks"].as_array().unwrap();
    assert!(tasks.iter().all(|t| t["status"] != "in_progress"));
    assert_eq!(status["result"]["agents"]["running"], 1);
}

/// Verify the TaskAssigned auto-emit logic works when an AgentCreated event
/// with task_id is produced (simulates daemon's run_due_decisions behavior).
#[test]
fn task_assigned_auto_emitted_after_agent_created_with_task() {
    let mut proj = Projections::default();

    // Create a task
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Test task".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
    });
    assert_eq!(
        proj.work.tasks.get("t1").unwrap().status,
        TaskStatus::Pending
    );

    // Simulate what execute_spawn returns
    let spawn_events = vec![
        DomainEvent::AgentCreated {
            id: "a1".into(),
            name: "swift-river".into(),
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
            pid: 5000,
            session_id: Some("sess-1".into()),
        },
    ];

    // Apply spawn events (what apply_events does)
    for event in &spawn_events {
        proj.apply(event);
    }

    // Simulate the daemon's auto-emit: check for AgentCreated with task_id
    let mut assign_events = Vec::new();
    for event in &spawn_events {
        if let DomainEvent::AgentCreated {
            id,
            task_id: Some(tid),
            ..
        } = event
        {
            assign_events.push(DomainEvent::TaskAssigned {
                task_id: tid.clone(),
                agent_id: id.clone(),
            });
        }
    }

    assert_eq!(assign_events.len(), 1, "should auto-emit 1 TaskAssigned");

    // Apply the auto-emitted TaskAssigned
    for event in &assign_events {
        proj.apply(event);
    }

    // Task should now be InProgress
    assert_eq!(
        proj.work.tasks.get("t1").unwrap().status,
        TaskStatus::InProgress,
        "task should be InProgress after TaskAssigned auto-emit"
    );
}

#[test]
fn recover_from_empty_directory() {
    let dir = TempDir::new().unwrap();
    let (store, snapshot, events) = EventStore::recover(dir.path().join("events")).unwrap();

    assert_eq!(store.sequence(), 0);
    assert!(snapshot.is_none());
    assert!(events.is_empty());

    let proj = Projections::default();
    let (status, _, _) = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 1}),
        &proj,
        test_channels_dir(),
    );
    assert_eq!(status["result"]["agents"]["total"], 0);
}
