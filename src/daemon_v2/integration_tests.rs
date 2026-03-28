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
    };
    store.append(&e2).unwrap();
    proj.apply(&e2);

    // 3. Start the agent
    let e3 = DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 5678,
    };
    store.append(&e3).unwrap();
    proj.apply(&e3);

    // Verify state via RPC
    let (status, _) = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 1}),
        &proj,
        test_channels_dir(),
    );
    assert_eq!(status["result"]["agents"]["running"], 1);
    assert_eq!(status["result"]["tasks"]["pending"], 1);

    // 4. Assign task
    let e4 = DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    };
    store.append(&e4).unwrap();
    proj.apply(&e4);

    let (status, _) = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 2}),
        &proj,
        test_channels_dir(),
    );
    assert_eq!(status["result"]["tasks"]["pending"], 0);
    assert_eq!(status["result"]["tasks"]["in_progress"], 1);

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

    let (status, _) = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 3}),
        &recovered_proj,
        test_channels_dir(),
    );
    assert_eq!(status["result"]["tasks"]["in_progress"], 0);
    assert_eq!(status["result"]["agents"]["running"], 1);
}

#[test]
fn recover_from_empty_directory() {
    let dir = TempDir::new().unwrap();
    let (store, snapshot, events) = EventStore::recover(dir.path().join("events")).unwrap();

    assert_eq!(store.sequence(), 0);
    assert!(snapshot.is_none());
    assert!(events.is_empty());

    let proj = Projections::default();
    let (status, _) = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 1}),
        &proj,
        test_channels_dir(),
    );
    assert_eq!(status["result"]["agents"]["total"], 0);
}
