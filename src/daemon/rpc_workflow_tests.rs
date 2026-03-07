//! Tests for workflow state RPC handlers.

use std::process::Command;

use super::super::DaemonState;
use super::{
    handle_workflow_assign, handle_workflow_get_state, handle_workflow_list,
    handle_workflow_set_state, handle_workflow_unassign,
};
use crate::rpc::RequestId;

fn make_test_state(
    repo_name: &str,
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
    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let state = DaemonState::new(
        "/tmp/test-workflow-state.sock".into(),
        cm,
        crate::paths::ProjectPaths::with_project_name(repo_name, repo_name),
        vec![base_dir],
        channel_router,
        None,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state");
    (state, temp_dir, _guard)
}

/// get_state returns null when no state exists for the channel.
#[tokio::test]
async fn test_get_state_returns_null_when_empty() {
    let (state, _temp_dir, _guard) = make_test_state("wf-get-empty");

    let response =
        handle_workflow_get_state(RequestId::Number(1), "test-channel", None, &state).await;

    let result = response.result.expect("should succeed");
    assert!(result["state"].is_null());
}

/// set_state then get_state round-trips a JSON value.
#[tokio::test]
async fn test_set_then_get_roundtrip() {
    let (state, _temp_dir, _guard) = make_test_state("wf-roundtrip");

    let value = serde_json::json!({"counter": 42, "active": true});
    let set_resp = handle_workflow_set_state(
        RequestId::Number(1),
        "test-channel",
        None,
        value.clone(),
        &state,
    )
    .await;
    assert!(set_resp.result.is_some(), "set_state should succeed");

    let get_resp =
        handle_workflow_get_state(RequestId::Number(2), "test-channel", None, &state).await;
    let result = get_resp.result.expect("get_state should succeed");
    assert_eq!(result["state"], value);
}

/// set_state with plugin key merges into existing state.
#[tokio::test]
async fn test_set_state_with_plugin_key_merges() {
    let (state, _temp_dir, _guard) = make_test_state("wf-plugin-merge");

    // Set plugin-a state
    let val_a = serde_json::json!({"step": 1});
    handle_workflow_set_state(
        RequestId::Number(1),
        "test-channel",
        Some("plugin-a"),
        val_a.clone(),
        &state,
    )
    .await;

    // Set plugin-b state
    let val_b = serde_json::json!({"step": 2});
    handle_workflow_set_state(
        RequestId::Number(2),
        "test-channel",
        Some("plugin-b"),
        val_b.clone(),
        &state,
    )
    .await;

    // get_state without plugin returns entire object
    let get_all =
        handle_workflow_get_state(RequestId::Number(3), "test-channel", None, &state).await;
    let result = get_all.result.expect("should succeed");
    assert_eq!(result["state"]["plugin-a"], val_a);
    assert_eq!(result["state"]["plugin-b"], val_b);

    // get_state with plugin returns only that key's value
    let get_a = handle_workflow_get_state(
        RequestId::Number(4),
        "test-channel",
        Some("plugin-a"),
        &state,
    )
    .await;
    let result_a = get_a.result.expect("should succeed");
    assert_eq!(result_a["state"], val_a);
}

/// get_state with plugin key returns null when key is absent.
#[tokio::test]
async fn test_get_state_plugin_key_absent() {
    let (state, _temp_dir, _guard) = make_test_state("wf-plugin-absent");

    // Set some state first
    handle_workflow_set_state(
        RequestId::Number(1),
        "test-channel",
        Some("existing-plugin"),
        serde_json::json!({"data": true}),
        &state,
    )
    .await;

    // Query a non-existent plugin key
    let resp = handle_workflow_get_state(
        RequestId::Number(2),
        "test-channel",
        Some("nonexistent"),
        &state,
    )
    .await;
    let result = resp.result.expect("should succeed");
    assert!(result["state"].is_null());
}

/// Different channels have isolated state.
#[tokio::test]
async fn test_channels_have_isolated_state() {
    let (state, _temp_dir, _guard) = make_test_state("wf-isolation");

    handle_workflow_set_state(
        RequestId::Number(1),
        "channel-a",
        None,
        serde_json::json!({"ch": "a"}),
        &state,
    )
    .await;

    handle_workflow_set_state(
        RequestId::Number(2),
        "channel-b",
        None,
        serde_json::json!({"ch": "b"}),
        &state,
    )
    .await;

    let get_a = handle_workflow_get_state(RequestId::Number(3), "channel-a", None, &state).await;
    let get_b = handle_workflow_get_state(RequestId::Number(4), "channel-b", None, &state).await;

    assert_eq!(get_a.result.unwrap()["state"]["ch"], "a");
    assert_eq!(get_b.result.unwrap()["state"]["ch"], "b");
}

/// Concurrent set_state calls for different plugin keys on the same channel
/// both persist (no data loss).
#[tokio::test]
async fn test_concurrent_set_state_plugin_keys_no_data_loss() {
    let (state, _temp_dir, _guard) = make_test_state("wf-concurrent");
    let state = std::sync::Arc::new(state);

    let mut handles = Vec::new();
    for i in 0..10 {
        let s = state.clone();
        handles.push(tokio::spawn(async move {
            let key = format!("plugin-{i}");
            handle_workflow_set_state(
                RequestId::Number(i as i64),
                "test-channel",
                Some(&key),
                serde_json::json!({"index": i}),
                &s,
            )
            .await;
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    let resp =
        handle_workflow_get_state(RequestId::Number(100), "test-channel", None, &state).await;
    let result = resp.result.expect("should succeed");
    let obj = result["state"].as_object().expect("state should be object");

    // All 10 plugin keys should be present — no writes lost.
    for i in 0..10 {
        let key = format!("plugin-{i}");
        assert!(
            obj.contains_key(&key),
            "missing key {key} — concurrent write lost data"
        );
        assert_eq!(obj[&key]["index"], i);
    }
}

/// set_state without plugin key replaces entire state.
#[tokio::test]
async fn test_set_state_replaces_entire_state() {
    let (state, _temp_dir, _guard) = make_test_state("wf-replace");

    // Set initial state
    handle_workflow_set_state(
        RequestId::Number(1),
        "test-channel",
        None,
        serde_json::json!({"old_key": "old_value"}),
        &state,
    )
    .await;

    // Replace with new state
    let new = serde_json::json!({"new_key": "new_value"});
    handle_workflow_set_state(
        RequestId::Number(2),
        "test-channel",
        None,
        new.clone(),
        &state,
    )
    .await;

    let resp = handle_workflow_get_state(RequestId::Number(3), "test-channel", None, &state).await;
    let result = resp.result.unwrap();
    assert_eq!(result["state"], new);
    assert!(result["state"]["old_key"].is_null());
}

/// State persists to daemon-state.json and survives reload.
#[tokio::test]
async fn test_state_persists_to_daemon_state_json() {
    let (state, _temp_dir, _guard) = make_test_state("wf-persist");

    let value = serde_json::json!({"persistent": true, "count": 7});
    handle_workflow_set_state(
        RequestId::Number(1),
        "test-channel",
        None,
        value.clone(),
        &state,
    )
    .await;

    // Reload from disk and verify the workflow state was persisted.
    let reloaded = crate::daemon::state::DaemonPersistentState::load_for_repo("wf-persist")
        .expect("reload should succeed");
    let channel_state = reloaded
        .workflow_state
        .get("test-channel")
        .expect("channel should exist in reloaded state");
    assert_eq!(*channel_state, value);
}

// ── workflow.assign / workflow.unassign / workflow.list ───────────────────

/// workflow.assign stores channel→workflow mapping.
#[tokio::test]
async fn test_workflow_assign_and_list() {
    let (state, _temp_dir, _guard) = make_test_state("wf-assign");

    // Create a workflow directory so workflow.list can find it
    let workflows_dir = state.paths.workflows_dir();
    std::fs::create_dir_all(workflows_dir.join("tdw")).unwrap();
    std::fs::write(workflows_dir.join("tdw/workflow.py"), "# hooks").unwrap();
    std::fs::write(workflows_dir.join("tdw/AGENTS.md"), "# TDW workflow").unwrap();

    // Assign "tdw" to "proj-auth"
    let resp = handle_workflow_assign(RequestId::Number(1), "proj-auth", "tdw", &state).await;
    assert!(resp.result.is_some(), "assign should succeed");

    // Verify via persistent state
    let ps = state.persistent_state.lock().await;
    assert_eq!(ps.channel_workflows.get("proj-auth").unwrap(), "tdw");
    drop(ps);

    // workflow.list should return the workflow
    let list_resp = handle_workflow_list(RequestId::Number(2), &state).await;
    let result = list_resp.result.expect("list should succeed");
    let workflows = result["workflows"].as_array().expect("should be array");
    assert_eq!(workflows.len(), 1);
    assert_eq!(workflows[0]["name"], "tdw");
    assert!(workflows[0]["has_agents_md"].as_bool().unwrap());

    // Also check assignments in list response
    let assignments = result["assignments"].as_object().expect("should be object");
    assert_eq!(assignments["proj-auth"], "tdw");
}

/// workflow.unassign removes the mapping.
#[tokio::test]
async fn test_workflow_unassign() {
    let (state, _temp_dir, _guard) = make_test_state("wf-unassign");

    // Assign first
    {
        let mut ps = state.persistent_state.lock().await;
        ps.channel_workflows
            .insert("proj-auth".to_string(), "tdw".to_string());
    }

    // Unassign
    let resp = handle_workflow_unassign(RequestId::Number(1), "proj-auth", &state).await;
    assert!(resp.result.is_some(), "unassign should succeed");

    // Verify removed
    let ps = state.persistent_state.lock().await;
    assert!(!ps.channel_workflows.contains_key("proj-auth"));
}

/// workflow.list returns empty when no workflows exist.
#[tokio::test]
async fn test_workflow_list_empty() {
    let (state, _temp_dir, _guard) = make_test_state("wf-list-empty");

    let resp = handle_workflow_list(RequestId::Number(1), &state).await;
    let result = resp.result.expect("list should succeed");
    let workflows = result["workflows"].as_array().expect("should be array");
    assert!(workflows.is_empty());
    let assignments = result["assignments"].as_object().expect("should be object");
    assert!(assignments.is_empty());
}
