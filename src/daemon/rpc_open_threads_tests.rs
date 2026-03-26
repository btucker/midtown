//! Tests for open_threads persistent state.

use crate::daemon::state::DaemonPersistentState;
use std::collections::HashSet;

#[test]
fn open_threads_default_empty() {
    let ps = DaemonPersistentState::default();
    assert!(ps.open_threads.is_empty());
}

#[test]
fn open_threads_roundtrip_serde() {
    let mut ps = DaemonPersistentState::default();
    let mut threads = HashSet::new();
    threads.insert("thread-1".to_string());
    threads.insert("thread-2".to_string());
    ps.open_threads.insert("my-channel".to_string(), threads);

    let json = serde_json::to_string(&ps).unwrap();
    let ps2: DaemonPersistentState = serde_json::from_str(&json).unwrap();

    assert_eq!(ps2.open_threads.get("my-channel").unwrap().len(), 2);
    assert!(
        ps2.open_threads
            .get("my-channel")
            .unwrap()
            .contains("thread-1")
    );
}

#[test]
fn open_threads_deserialize_missing_field() {
    // Old state files won't have open_threads — should default to empty
    let json = r#"{}"#;
    let ps: DaemonPersistentState = serde_json::from_str(json).unwrap();
    assert!(ps.open_threads.is_empty());
}
