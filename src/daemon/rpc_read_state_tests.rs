//! Tests for read state RPC handlers and persistent state.

use super::*;
use crate::daemon::state::{DaemonPersistentState, ReadState};
use std::process::Command;

// ============================================================================
// Persistent state tests (no DaemonState needed)
// ============================================================================

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

// ============================================================================
// Test fixture
// ============================================================================

fn make_test_state_with_web_tx(
    repo_name: &str,
    web_updates_tx: Option<tokio::sync::broadcast::Sender<crate::web::WebUpdate>>,
) -> (
    DaemonState,
    tempfile::TempDir,
    crate::paths::TestMidtownBaseDirGuard,
) {
    let midtown_dir = tempfile::tempdir().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

    let temp_dir = tempfile::tempdir().expect("temp dir");
    Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git commit");

    let wm = crate::worktree::WorktreeManager::new(temp_dir.path().to_path_buf())
        .expect("worktree manager");
    let cm = crate::coworker::CoworkerManager::new(wm);

    let base_dir = temp_dir.path().to_path_buf();

    let channel_router = crate::ChannelRouter::new(&base_dir, repo_name);
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let (session_agg_tx, _session_agg_rx) = crate::daemon::session_events::channel();
    let state = DaemonState::new(
        "/tmp/test.sock".into(),
        cm,
        crate::paths::ProjectPaths::with_project_name(repo_name, repo_name),
        vec![base_dir],
        channel_router,
        web_updates_tx,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
        session_agg_tx,
    )
    .expect("daemon state");
    (state, temp_dir, _guard)
}

fn make_test_state(
    repo_name: &str,
) -> (
    DaemonState,
    tempfile::TempDir,
    crate::paths::TestMidtownBaseDirGuard,
) {
    make_test_state_with_web_tx(repo_name, None)
}

// ============================================================================
// RPC handler tests
// ============================================================================

#[tokio::test]
async fn test_read_state_get_empty_returns_empty() {
    let (state, _temp_dir, _guard) = make_test_state("test-read-state-empty");

    let response = handle_read_state_get(1_i64.into(), &state).await;

    assert!(response.error.is_none(), "should not error");
    let result = response.result.expect("should have result");

    let threads = result.get("threads").expect("should have threads key");
    assert!(
        threads
            .as_object()
            .expect("threads should be an object")
            .is_empty(),
        "threads should be empty on fresh state"
    );

    let channels = result.get("channels").expect("should have channels key");
    assert!(
        channels
            .as_object()
            .expect("channels should be an object")
            .is_empty(),
        "channels should be empty on fresh state"
    );
}

#[tokio::test]
async fn test_mark_thread_read_then_get() {
    let (state, _temp_dir, _guard) = make_test_state("test-mark-thread-read");

    let mark_response = handle_read_state_mark_read(
        1_i64.into(),
        "thread",
        "thread-abc",
        "2026-03-27T10:00:00Z",
        &state,
    )
    .await;

    assert!(mark_response.error.is_none(), "mark_read should not error");
    assert_eq!(
        mark_response
            .result
            .as_ref()
            .and_then(|r| r.get("ok"))
            .and_then(|v| v.as_bool()),
        Some(true),
        "mark_read should return ok:true"
    );

    let get_response = handle_read_state_get(2_i64.into(), &state).await;
    assert!(get_response.error.is_none(), "get should not error");
    let result = get_response.result.expect("should have result");

    let threads = result
        .get("threads")
        .and_then(|v| v.as_object())
        .expect("threads should be an object");
    assert_eq!(
        threads.get("thread-abc").and_then(|v| v.as_str()),
        Some("2026-03-27T10:00:00Z"),
        "thread timestamp should match"
    );
}

#[tokio::test]
async fn test_mark_channel_read_then_get() {
    let (state, _temp_dir, _guard) = make_test_state("test-mark-channel-read");

    handle_read_state_mark_read(
        1_i64.into(),
        "channel",
        "auth-refactor",
        "2026-03-27T09:00:00Z",
        &state,
    )
    .await;

    let get_response = handle_read_state_get(2_i64.into(), &state).await;
    let result = get_response.result.expect("should have result");

    let channels = result
        .get("channels")
        .and_then(|v| v.as_object())
        .expect("channels should be an object");
    assert_eq!(
        channels.get("auth-refactor").and_then(|v| v.as_str()),
        Some("2026-03-27T09:00:00Z"),
        "channel timestamp should match"
    );
}

#[tokio::test]
async fn test_mark_read_broadcasts_web_update() {
    let (updates_tx, mut rx) = crate::web::create_updates_channel();
    let (state, _temp_dir, _guard) =
        make_test_state_with_web_tx("test-read-state-broadcast", Some(updates_tx));

    handle_read_state_mark_read(
        1_i64.into(),
        "thread",
        "thread-xyz",
        "2026-03-27T11:00:00Z",
        &state,
    )
    .await;

    let update = rx.try_recv().expect("should have received a web update");
    match update {
        crate::web::WebUpdate::ReadStateChanged(data) => {
            assert_eq!(data.item_type, "thread");
            assert_eq!(data.id, "thread-xyz");
            assert_eq!(data.timestamp, "2026-03-27T11:00:00Z");
        }
        other => panic!("unexpected update variant: {other:?}"),
    }
}

#[tokio::test]
async fn test_mark_read_invalid_type_returns_error() {
    let (state, _temp_dir, _guard) = make_test_state("test-read-state-invalid-type");

    let response = handle_read_state_mark_read(
        1_i64.into(),
        "message",
        "some-id",
        "2026-03-27T10:00:00Z",
        &state,
    )
    .await;

    assert!(response.error.is_some(), "should return an error");
    assert!(response.result.is_none(), "should not have a result");
    let error = response.error.unwrap();
    assert_eq!(error.code, -32602, "should be invalid params error code");
    assert!(
        error.message.contains("message"),
        "error message should mention the invalid type"
    );
}
