//! Behavioral tests for v2-spec.md Section 7: Event Store
//!
//! Each test maps to a specific SHALL requirement from the spec.

use super::*;
use crate::daemon_v2::events::{AgentKind, Provider};
use crate::daemon_v2::projections::Projections;
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn temp_store() -> (EventStore, TempDir) {
    let dir = TempDir::new().unwrap();
    let store = EventStore::new(dir.path().to_path_buf());
    (store, dir)
}

fn sample_event() -> DomainEvent {
    DomainEvent::AgentCreated {
        id: "agent-spec".into(),
        name: "amber-glow".into(),
        kind: AgentKind::Lead,
        agent_type: "midtown-project-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
        bound_thread_id: None,
        icon: None,
        color: None,
    }
}

fn sample_task_event() -> DomainEvent {
    DomainEvent::TaskCreated {
        id: "task-spec".into(),
        subject: "Spec test task".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        icon: None,
        parent: None,
    }
}

// ── Section 7: Event Store ────────────────────────────────────────────────────

/// Spec 7: WHEN EventStore is created THEN log-0000.jsonl SHALL be created
#[test]
fn event_store_creation_creates_log_file() {
    let dir = TempDir::new().unwrap();
    let _store = EventStore::new(dir.path().to_path_buf());

    let log_path = dir.path().join("log-0000.jsonl");
    assert!(
        log_path.exists(),
        "log-0000.jsonl should be created on EventStore::new"
    );
}

/// Spec 7: WHEN an event is appended THEN it SHALL be serialized as a JSON line
/// and the sequence counter incremented
#[test]
fn append_increments_sequence_counter() {
    let (mut store, _dir) = temp_store();

    assert_eq!(store.sequence(), 0, "initial sequence should be 0");

    store.append(&sample_event()).unwrap();
    assert_eq!(
        store.sequence(),
        1,
        "sequence should be 1 after first append"
    );

    store.append(&sample_task_event()).unwrap();
    assert_eq!(
        store.sequence(),
        2,
        "sequence should be 2 after second append"
    );
}

/// Spec 7: WHEN an event is appended THEN each event increments sequence by exactly 1
#[test]
fn each_append_increments_sequence_by_one() {
    let (mut store, _dir) = temp_store();

    for i in 1..=5u64 {
        store
            .append(&DomainEvent::TaskCreated {
                id: format!("task-{i}"),
                subject: format!("Task {i}"),
                channel: "main".into(),
                blocked_by: vec![],
                agent_type: None,
                icon: None,
                parent: None,
            })
            .unwrap();
        assert_eq!(
            store.sequence(),
            i,
            "sequence should be {i} after {i} appends"
        );
    }
}

/// Spec 7: WHEN recovery is performed THEN the latest snapshot SHALL be loaded
/// and remaining events replayed
#[test]
fn recovery_loads_snapshot_and_replays_events() {
    let dir = TempDir::new().unwrap();
    let mut store = EventStore::new(dir.path().to_path_buf());

    // Append events and take a snapshot
    store.append(&sample_event()).unwrap();
    store.append(&sample_task_event()).unwrap();
    assert_eq!(store.sequence(), 2);

    let projections = Projections::default();
    store.save_snapshot(&projections).unwrap();

    // Append more events after the snapshot
    store
        .append(&DomainEvent::AgentStarted {
            id: "agent-spec".into(),
            pid: 9999,
            session_id: Some("sess-recovery".into()),
        })
        .unwrap();
    assert_eq!(store.sequence(), 3);

    drop(store);

    // Recover: should find the snapshot (at seq 2) and replay 1 post-snapshot event
    let (recovered_store, snapshot, replay_events) =
        EventStore::recover(dir.path().to_path_buf()).unwrap();

    assert!(
        snapshot.is_some(),
        "recovery should load the saved snapshot"
    );
    assert_eq!(
        replay_events.len(),
        1,
        "recovery should replay 1 post-snapshot event"
    );
    assert_eq!(
        recovered_store.sequence(),
        3,
        "recovered store sequence should match pre-drop sequence"
    );
}

/// Spec 7: WHEN a snapshot is saved THEN all projections SHALL be serialized and
/// the log file advanced (new log file created at the snapshot sequence)
#[test]
fn snapshot_serializes_projections_and_advances_log() {
    let dir = TempDir::new().unwrap();
    let mut store = EventStore::new(dir.path().to_path_buf());

    store.append(&sample_event()).unwrap();
    store.append(&sample_task_event()).unwrap();
    assert_eq!(store.sequence(), 2);

    let projections = Projections::default();
    store.save_snapshot(&projections).unwrap();

    // Snapshot file should exist
    let snapshot_path = dir.path().join("snapshot-0002.json");
    assert!(
        snapshot_path.exists(),
        "snapshot-0002.json should be written after save_snapshot at sequence 2"
    );

    // Post-snapshot log file should exist
    let new_log = dir.path().join("log-0002.jsonl");
    assert!(
        new_log.exists(),
        "log-0002.jsonl should be created after snapshot to advance the log"
    );

    // The snapshot file should be valid JSON (deserializable as Projections)
    let content = std::fs::read_to_string(&snapshot_path).unwrap();
    let _parsed: Projections = serde_json::from_str(&content)
        .expect("snapshot should be valid JSON deserializable as Projections");
}

/// Spec 7: WHEN a log line is malformed during recovery THEN it SHALL be skipped
/// (recovery stops at the first malformed line and replays only valid events)
#[test]
fn recovery_skips_malformed_log_lines() {
    let dir = TempDir::new().unwrap();
    let mut store = EventStore::new(dir.path().to_path_buf());

    // Append one valid event
    store.append(&sample_event()).unwrap();
    assert_eq!(store.sequence(), 1);

    drop(store);

    // Simulate crash: inject a malformed line after the valid event
    let log_path = dir.path().join("log-0000.jsonl");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&log_path)
        .unwrap();
    writeln!(f, "{{\"broken_json\":").unwrap();
    drop(f);

    // Recovery should stop at the malformed line and only replay the valid event
    let (recovered_store, _snapshot, replay_events) =
        EventStore::recover(dir.path().to_path_buf()).unwrap();

    assert_eq!(
        replay_events.len(),
        1,
        "recovery should replay only the valid event before the malformed line"
    );
    assert_eq!(
        recovered_store.sequence(),
        1,
        "recovered sequence should reflect only the valid event"
    );
}

/// Spec 7: WHEN recovery is performed on a non-existent directory THEN it SHALL
/// return an empty store with no snapshot and no events
#[test]
fn recovery_from_nonexistent_dir_returns_empty_store() {
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("does-not-exist");

    let (store, snapshot, replay_events) = EventStore::recover(nonexistent).unwrap();

    assert_eq!(
        store.sequence(),
        0,
        "sequence should be 0 for empty recovery"
    );
    assert!(snapshot.is_none(), "no snapshot should exist");
    assert!(
        replay_events.is_empty(),
        "no events should be replayed from empty dir"
    );
}
