use super::*;
use crate::daemon_v2::events::{AgentKind, Provider};
use tempfile::TempDir;

fn temp_store() -> (EventStore, TempDir) {
    let dir = TempDir::new().unwrap();
    let store = EventStore::new(dir.path().to_path_buf());
    (store, dir)
}

fn sample_agent_created() -> DomainEvent {
    DomainEvent::AgentCreated {
        id: "agent-1".into(),
        name: "ghost-town".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("task-1".into()),
        bound_thread_id: None,
        icon: None,
        color: None,
    }
}

fn sample_task_created() -> DomainEvent {
    DomainEvent::TaskCreated {
        id: "task-1".into(),
        subject: "Fix auth bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: None,
        icon: None,
        color: None,
        parent: None,
        thread_id: None,
        message_id: None,
    }
}

#[test]
fn append_and_read_back() {
    let (mut store, _dir) = temp_store();
    let event = sample_agent_created();

    store.append(&event).unwrap();
    store.append(&sample_task_created()).unwrap();

    let events = store.events_since(0).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(store.sequence(), 2);
}

#[test]
fn snapshot_and_recover() {
    let (mut store, dir) = temp_store();

    // Append 3 events
    store.append(&sample_agent_created()).unwrap();
    store.append(&sample_task_created()).unwrap();
    store
        .append(&DomainEvent::AgentStarted {
            id: "agent-1".into(),
            pid: 1234,
            session_id: None,
        })
        .unwrap();

    assert_eq!(store.sequence(), 3);

    // Take snapshot at current sequence
    let projections = Projections::default();
    store.save_snapshot(&projections).unwrap();

    // Append one more event after snapshot
    store
        .append(&DomainEvent::TaskCompleted {
            task_id: "task-1".into(),
        })
        .unwrap();

    assert_eq!(store.sequence(), 4);

    // Recover from disk — should load snapshot + replay 1 event
    let (recovered_store, snapshot, replay_events) =
        EventStore::recover(dir.path().to_path_buf()).unwrap();

    assert_eq!(recovered_store.sequence(), 4);
    assert!(snapshot.is_some());
    assert_eq!(replay_events.len(), 1);
}

#[test]
fn recover_empty_directory() {
    let dir = TempDir::new().unwrap();
    let (store, snapshot, replay_events) = EventStore::recover(dir.path().to_path_buf()).unwrap();

    assert_eq!(store.sequence(), 0);
    assert!(snapshot.is_none());
    assert_eq!(replay_events.len(), 0);
}

#[test]
fn truncates_partial_line_on_recovery() {
    let (mut store, dir) = temp_store();
    store.append(&sample_agent_created()).unwrap();
    drop(store);

    // Simulate crash: append partial JSON to the log file
    let log_path = dir.path().join("log-0000.jsonl");
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&log_path)
        .unwrap();
    write!(f, "{{\"broken\":").unwrap();

    // Recovery should ignore the partial line
    let (recovered_store, _, replay_events) =
        EventStore::recover(dir.path().to_path_buf()).unwrap();

    assert_eq!(recovered_store.sequence(), 1);
    assert_eq!(replay_events.len(), 1);
}
