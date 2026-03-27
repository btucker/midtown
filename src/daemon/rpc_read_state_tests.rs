//! Tests for read state RPC handlers and persistent state.

use crate::daemon::state::{DaemonPersistentState, ReadState};

#[test]
fn read_state_default_empty() {
    let ps = DaemonPersistentState::default();
    assert!(ps.read_state.is_empty());
}

#[test]
fn read_state_struct_default_empty() {
    let rs = ReadState::default();
    assert!(rs.threads.is_empty());
    assert!(rs.channels.is_empty());
}

#[test]
fn read_state_roundtrip_serde() {
    let mut ps = DaemonPersistentState::default();
    let mut rs = ReadState::default();
    rs.threads
        .insert("thread-1".to_string(), "2026-03-27T10:00:00Z".to_string());
    rs.channels.insert(
        "auth-refactor".to_string(),
        "2026-03-27T09:00:00Z".to_string(),
    );
    ps.read_state.insert("default".to_string(), rs);

    let json = serde_json::to_string(&ps).unwrap();
    let ps2: DaemonPersistentState = serde_json::from_str(&json).unwrap();

    let rs2 = ps2.read_state.get("default").unwrap();
    assert_eq!(rs2.threads.get("thread-1").unwrap(), "2026-03-27T10:00:00Z");
    assert_eq!(
        rs2.channels.get("auth-refactor").unwrap(),
        "2026-03-27T09:00:00Z"
    );
}

#[test]
fn read_state_deserialize_missing_field() {
    let json = r#"{}"#;
    let ps: DaemonPersistentState = serde_json::from_str(json).unwrap();
    assert!(ps.read_state.is_empty());
}
