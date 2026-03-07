//! Workflow RPC handlers.
//!
//! Handles `workflow.get_state`, `workflow.set_state`, `workflow.list`,
//! `workflow.assign`, and `workflow.unassign` methods.
//!
//! State is owned by the daemon in `DaemonPersistentState::workflow_state`
//! and persisted to `daemon-state.json` alongside other daemon state. The
//! `plugin` parameter provides namespacing within a channel's state object.

use tracing::{debug, error};

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

// ============================================================================
// Handlers
// ============================================================================

/// Handle `workflow.get_state` RPC method.
///
/// Params:
/// - `channel` (required): channel name
/// - `plugin` (optional): plugin key — if provided, returns only that key's value
///
/// Returns the full state JSON or the value at the plugin key.
/// Returns `null` when no state exists for the channel or the plugin key is absent.
pub(super) async fn handle_workflow_get_state(
    id: RequestId,
    channel: &str,
    plugin: Option<&str>,
    state: &DaemonState,
) -> Response {
    let ps = state.persistent_state.lock().await;

    let channel_state = ps.workflow_state.get(channel);

    let value = match (channel_state, plugin) {
        (None, _) => serde_json::Value::Null,
        (Some(s), None) => s.clone(),
        (Some(s), Some(key)) => s.get(key).cloned().unwrap_or(serde_json::Value::Null),
    };

    Response::success(id, serde_json::json!({ "state": value }))
}

/// Handle `workflow.set_state` RPC method.
///
/// Params:
/// - `channel` (required): channel name
/// - `state` (required): JSON value to store
/// - `plugin` (optional): plugin key — if provided, merges at that key
///
/// When `plugin` is provided, the existing state is loaded, the plugin key
/// is updated (or inserted), and the result is stored. When `plugin`
/// is absent, the entire channel state is replaced.
pub(super) async fn handle_workflow_set_state(
    id: RequestId,
    channel: &str,
    plugin: Option<&str>,
    new_state: serde_json::Value,
    daemon_state: &DaemonState,
) -> Response {
    let mut ps = daemon_state.persistent_state.lock().await;

    match plugin {
        Some(key) => {
            // Merge into existing state at the plugin key.
            let entry = ps
                .workflow_state
                .entry(channel.to_string())
                .or_insert_with(|| serde_json::json!({}));

            if let Some(obj) = entry.as_object_mut() {
                obj.insert(key.to_string(), new_state);
            } else {
                // State wasn't an object — replace with a fresh object.
                let mut map = serde_json::Map::new();
                map.insert(key.to_string(), new_state);
                *entry = serde_json::Value::Object(map);
            }
        }
        None => {
            ps.workflow_state.insert(channel.to_string(), new_state);
        }
    }

    // Persist to daemon-state.json.
    if let Err(e) = ps.save_for_repo(daemon_state.paths.dir_key()) {
        error!(
            channel = %channel,
            "workflow.set_state: failed to save daemon state: {}",
            e
        );
        return Response::error(
            id,
            RpcError::new(-32603, format!("failed to persist state: {e}")),
        );
    }

    debug!(
        channel = %channel,
        plugin = ?plugin,
        "workflow.set_state: state updated"
    );

    Response::success(id, serde_json::json!({ "ok": true }))
}

/// Handle `workflow.assign` RPC method.
///
/// Params:
/// - `channel` (required): channel name
/// - `workflow` (required): workflow name to assign
///
/// Validates that the workflow exists, then stores the channel→workflow mapping
/// in `DaemonPersistentState::channel_workflows`.
pub(super) async fn handle_workflow_assign(
    id: RequestId,
    channel: &str,
    workflow: &str,
    daemon_state: &DaemonState,
) -> Response {
    // Validate workflow exists
    let workflows_dir = daemon_state.paths.workflows_dir();
    let workflows = crate::paths::discover_workflows(&workflows_dir);
    if !workflows.iter().any(|w| w.name == workflow) {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Workflow '{}' not found", workflow)),
        );
    }

    let mut ps = daemon_state.persistent_state.lock().await;
    ps.channel_workflows
        .insert(channel.to_string(), workflow.to_string());

    if let Err(e) = ps.save_for_repo(daemon_state.paths.dir_key()) {
        error!(
            channel = %channel,
            workflow = %workflow,
            "workflow.assign: failed to save daemon state: {}",
            e
        );
        return Response::error(
            id,
            RpcError::new(-32603, format!("failed to persist state: {e}")),
        );
    }

    debug!(
        channel = %channel,
        workflow = %workflow,
        "workflow.assign: assigned workflow to channel"
    );

    Response::success(id, serde_json::json!({ "ok": true }))
}

/// Handle `workflow.unassign` RPC method.
///
/// Params:
/// - `channel` (required): channel name
///
/// Removes the channel's workflow assignment, reverting to daemon defaults.
pub(super) async fn handle_workflow_unassign(
    id: RequestId,
    channel: &str,
    daemon_state: &DaemonState,
) -> Response {
    let mut ps = daemon_state.persistent_state.lock().await;
    ps.channel_workflows.remove(channel);

    if let Err(e) = ps.save_for_repo(daemon_state.paths.dir_key()) {
        error!(
            channel = %channel,
            "workflow.unassign: failed to save daemon state: {}",
            e
        );
        return Response::error(
            id,
            RpcError::new(-32603, format!("failed to persist state: {e}")),
        );
    }

    debug!(
        channel = %channel,
        "workflow.unassign: removed workflow assignment"
    );

    Response::success(id, serde_json::json!({ "ok": true }))
}

/// Handle `workflow.list` RPC method.
///
/// Returns available workflows (from the workflows directory) and current
/// channel→workflow assignments.
pub(super) async fn handle_workflow_list(id: RequestId, daemon_state: &DaemonState) -> Response {
    let workflows_dir = daemon_state.paths.workflows_dir();
    let workflows = crate::paths::discover_workflows(&workflows_dir);

    let workflow_list: Vec<serde_json::Value> = workflows
        .iter()
        .map(|w| {
            serde_json::json!({
                "name": w.name,
                "dir": w.dir.to_string_lossy(),
                "has_agents_md": w.agents_md.is_some(),
            })
        })
        .collect();

    let ps = daemon_state.persistent_state.lock().await;
    let assignments: serde_json::Map<String, serde_json::Value> = ps
        .channel_workflows
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();

    Response::success(
        id,
        serde_json::json!({
            "workflows": workflow_list,
            "assignments": assignments,
        }),
    )
}

#[path = "rpc_workflow_tests.rs"]
#[cfg(test)]
mod tests;
