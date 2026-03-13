//! Tests for workflow state RPC handlers.

use std::process::Command;

use super::super::DaemonState;
use super::{
    handle_workflow_assign, handle_workflow_get_state, handle_workflow_list,
    handle_workflow_set_lead_driven, handle_workflow_set_state, handle_workflow_unassign,
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

    let response = handle_workflow_get_state(RequestId::Number(1), "test-channel", &state).await;

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

    let get_resp = handle_workflow_get_state(RequestId::Number(2), "test-channel", &state).await;
    let result = get_resp.result.expect("get_state should succeed");
    assert_eq!(result["state"], value);
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

    let get_a = handle_workflow_get_state(RequestId::Number(3), "channel-a", &state).await;
    let get_b = handle_workflow_get_state(RequestId::Number(4), "channel-b", &state).await;

    assert_eq!(get_a.result.unwrap()["state"]["ch"], "a");
    assert_eq!(get_b.result.unwrap()["state"]["ch"], "b");
}

/// Concurrent set_state calls for the same channel — last write wins.
#[tokio::test]
async fn test_concurrent_set_state_last_write_wins() {
    let (state, _temp_dir, _guard) = make_test_state("wf-concurrent");
    let state = std::sync::Arc::new(state);

    let mut handles = Vec::new();
    for i in 0..10 {
        let s = state.clone();
        handles.push(tokio::spawn(async move {
            handle_workflow_set_state(
                RequestId::Number(i as i64),
                "test-channel",
                None,
                serde_json::json!({"index": i}),
                &s,
            )
            .await;
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    // One of the writes should have won — state should be a valid object with "index".
    let resp = handle_workflow_get_state(RequestId::Number(100), "test-channel", &state).await;
    let result = resp.result.expect("should succeed");
    assert!(
        result["state"]["index"].is_number(),
        "state should have an index field"
    );
}

/// set_state replaces entire state.
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

    let resp = handle_workflow_get_state(RequestId::Number(3), "test-channel", &state).await;
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

// ── Nested key path tests ────────────────────────────────────────────────

/// set_state with a key sets a nested value without overwriting other keys.
#[tokio::test]
async fn test_set_state_with_key_preserves_existing() {
    let (state, _temp_dir, _guard) = make_test_state("wf-nested-key");

    // Set initial state with some data
    handle_workflow_set_state(
        RequestId::Number(1),
        "test-channel",
        None,
        serde_json::json!({"existing": "data", "tasks": {"100": {"status": "open"}}}),
        &state,
    )
    .await;

    // Set a nested key — should NOT wipe existing state
    handle_workflow_set_state(
        RequestId::Number(2),
        "test-channel",
        Some("tasks.42.excluded"),
        serde_json::json!(true),
        &state,
    )
    .await;

    let resp = handle_workflow_get_state(RequestId::Number(3), "test-channel", &state).await;
    let result = resp.result.unwrap();

    // Original data preserved
    assert_eq!(result["state"]["existing"], "data");
    assert_eq!(result["state"]["tasks"]["100"]["status"], "open");
    // New nested key set
    assert_eq!(result["state"]["tasks"]["42"]["excluded"], true);
}

/// set_state with a key and null value removes the nested key.
#[tokio::test]
async fn test_set_state_with_key_null_removes() {
    let (state, _temp_dir, _guard) = make_test_state("wf-nested-remove");

    // Set initial state
    handle_workflow_set_state(
        RequestId::Number(1),
        "test-channel",
        None,
        serde_json::json!({"tasks": {"42": {"excluded": true, "note": "test"}}}),
        &state,
    )
    .await;

    // Remove the "excluded" key
    handle_workflow_set_state(
        RequestId::Number(2),
        "test-channel",
        Some("tasks.42.excluded"),
        serde_json::Value::Null,
        &state,
    )
    .await;

    let resp = handle_workflow_get_state(RequestId::Number(3), "test-channel", &state).await;
    let result = resp.result.unwrap();

    // "excluded" removed, "note" preserved
    assert!(result["state"]["tasks"]["42"]["excluded"].is_null());
    assert_eq!(result["state"]["tasks"]["42"]["note"], "test");
}

/// set_state with a key creates intermediate objects as needed.
#[tokio::test]
async fn test_set_state_with_key_creates_intermediates() {
    let (state, _temp_dir, _guard) = make_test_state("wf-nested-create");

    // No state exists yet — set a deeply nested key
    handle_workflow_set_state(
        RequestId::Number(1),
        "test-channel",
        Some("tasks.99.excluded"),
        serde_json::json!(true),
        &state,
    )
    .await;

    let resp = handle_workflow_get_state(RequestId::Number(2), "test-channel", &state).await;
    let result = resp.result.unwrap();

    assert_eq!(result["state"]["tasks"]["99"]["excluded"], true);
}

// ── workflow.set-lead-driven ─────────────────────────────────────────────

/// Enabling lead-driven mode inserts channel into the set.
#[tokio::test]
async fn test_set_lead_driven_enable() {
    let (state, _temp_dir, _guard) = make_test_state("wf-lead-driven-enable");

    let resp =
        handle_workflow_set_lead_driven(RequestId::Number(1), "proj-auth", true, &state).await;
    let result = resp.result.expect("should succeed");
    assert_eq!(result["ok"], true);
    assert_eq!(result["enabled"], true);

    // Verify persistent state
    let ps = state.persistent_state.lock().await;
    assert!(ps.lead_driven_channels.contains("proj-auth"));
}

/// Disabling lead-driven mode removes channel from the set.
#[tokio::test]
async fn test_set_lead_driven_disable() {
    let (state, _temp_dir, _guard) = make_test_state("wf-lead-driven-disable");

    // Enable first
    handle_workflow_set_lead_driven(RequestId::Number(1), "proj-auth", true, &state).await;

    // Then disable
    let resp =
        handle_workflow_set_lead_driven(RequestId::Number(2), "proj-auth", false, &state).await;
    let result = resp.result.expect("should succeed");
    assert_eq!(result["ok"], true);
    assert_eq!(result["enabled"], false);

    // Verify removed
    let ps = state.persistent_state.lock().await;
    assert!(!ps.lead_driven_channels.contains("proj-auth"));
}

/// Lead-driven state persists to daemon-state.json and survives reload.
#[tokio::test]
async fn test_lead_driven_persists_to_disk() {
    let (state, _temp_dir, _guard) = make_test_state("wf-lead-driven-persist");

    handle_workflow_set_lead_driven(RequestId::Number(1), "proj-writing", true, &state).await;

    // Reload from disk
    let reloaded =
        crate::daemon::state::DaemonPersistentState::load_for_repo("wf-lead-driven-persist")
            .expect("reload should succeed");
    assert!(reloaded.lead_driven_channels.contains("proj-writing"));
}

/// Multiple channels can be lead-driven independently.
#[tokio::test]
async fn test_lead_driven_multiple_channels() {
    let (state, _temp_dir, _guard) = make_test_state("wf-lead-driven-multi");

    handle_workflow_set_lead_driven(RequestId::Number(1), "proj-auth", true, &state).await;
    handle_workflow_set_lead_driven(RequestId::Number(2), "proj-writing", true, &state).await;

    let ps = state.persistent_state.lock().await;
    assert!(ps.lead_driven_channels.contains("proj-auth"));
    assert!(ps.lead_driven_channels.contains("proj-writing"));
    assert!(!ps.lead_driven_channels.contains("proj-other"));
    drop(ps);

    // Disable one, other stays
    handle_workflow_set_lead_driven(RequestId::Number(3), "proj-auth", false, &state).await;

    let ps = state.persistent_state.lock().await;
    assert!(!ps.lead_driven_channels.contains("proj-auth"));
    assert!(ps.lead_driven_channels.contains("proj-writing"));
}

/// Disabling an already-disabled channel is a no-op (idempotent).
#[tokio::test]
async fn test_lead_driven_disable_idempotent() {
    let (state, _temp_dir, _guard) = make_test_state("wf-lead-driven-idempotent");

    let resp =
        handle_workflow_set_lead_driven(RequestId::Number(1), "proj-auth", false, &state).await;
    assert!(
        resp.result.is_some(),
        "disabling non-existent should succeed"
    );

    let ps = state.persistent_state.lock().await;
    assert!(!ps.lead_driven_channels.contains("proj-auth"));
}
