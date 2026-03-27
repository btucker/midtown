//! Tests for open_threads RPC handlers and persistent state.

use super::*;
use crate::daemon::state::DaemonPersistentState;
use std::collections::HashSet;
use std::process::Command;

// ============================================================================
// Persistent state tests (no DaemonState needed)
// ============================================================================

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
async fn test_open_threads_get_unknown_channel_returns_empty() {
    let (state, _temp_dir, _guard) = make_test_state("test-repo-get-unknown");

    let response = handle_open_threads_get(1_i64.into(), "nonexistent-channel", &state).await;

    assert!(response.error.is_none(), "should not error");
    let result = response.result.expect("should have result");
    let threads = result.get("threads").expect("should have threads key");
    assert!(
        threads
            .as_array()
            .expect("threads should be array")
            .is_empty(),
        "unknown channel should return empty threads"
    );
}

#[tokio::test]
async fn test_open_threads_set_then_get_returns_set_threads() {
    let (state, _temp_dir, _guard) = make_test_state("test-repo-set-get");

    let set_response = handle_open_threads_set(
        1_i64.into(),
        "my-channel",
        vec!["thread-a".to_string(), "thread-b".to_string()],
        &state,
    )
    .await;

    assert!(set_response.error.is_none(), "set should not error");
    let set_result = set_response.result.expect("set should have result");
    assert_eq!(
        set_result.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "set should return ok:true"
    );

    let get_response = handle_open_threads_get(2_i64.into(), "my-channel", &state).await;

    assert!(get_response.error.is_none(), "get should not error");
    let get_result = get_response.result.expect("get should have result");
    let threads = get_result
        .get("threads")
        .and_then(|v| v.as_array())
        .expect("should have threads array");

    assert_eq!(threads.len(), 2, "should return exactly 2 threads");

    let thread_strs: Vec<&str> = threads.iter().filter_map(|v| v.as_str()).collect();
    assert!(thread_strs.contains(&"thread-a"), "should contain thread-a");
    assert!(thread_strs.contains(&"thread-b"), "should contain thread-b");
}

#[tokio::test]
async fn test_open_threads_set_replaces_existing() {
    let (state, _temp_dir, _guard) = make_test_state("test-repo-replace");

    // Set initial threads
    handle_open_threads_set(
        1_i64.into(),
        "my-channel",
        vec!["old-thread".to_string()],
        &state,
    )
    .await;

    // Replace with new threads
    handle_open_threads_set(
        2_i64.into(),
        "my-channel",
        vec!["new-thread".to_string()],
        &state,
    )
    .await;

    let get_response = handle_open_threads_get(3_i64.into(), "my-channel", &state).await;

    let threads = get_response
        .result
        .expect("result")
        .get("threads")
        .and_then(|v| v.as_array())
        .expect("threads array")
        .clone();

    assert_eq!(
        threads.len(),
        1,
        "should have exactly 1 thread after replace"
    );
    assert_eq!(
        threads[0].as_str(),
        Some("new-thread"),
        "should contain only new-thread"
    );
}

#[tokio::test]
async fn test_open_threads_set_broadcasts_web_update() {
    let (updates_tx, mut rx) = crate::web::create_updates_channel();
    let (state, _temp_dir, _guard) =
        make_test_state_with_web_tx("test-repo-broadcast", Some(updates_tx));

    handle_open_threads_set(
        1_i64.into(),
        "broadcast-channel",
        vec!["t1".to_string()],
        &state,
    )
    .await;

    let update = rx.try_recv().expect("should have received a web update");
    match update {
        crate::web::WebUpdate::OpenThreadsChanged(data) => {
            assert_eq!(data.channel, "broadcast-channel");
            assert!(data.threads.contains(&"t1".to_string()));
        }
        other => panic!("unexpected update variant: {other:?}"),
    }
}
