//! Workflow RPC handlers.
//!
//! Handles `workflow.get_state`, `workflow.set_state`, `workflow.list`,
//! `workflow.assign`, `workflow.unassign`, and `workflow.set-lead-driven` methods.
//!
//! State is owned by the daemon in `DaemonPersistentState::workflow_state`
//! and persisted to `daemon-state.json` alongside other daemon state.
//! State is flat per-channel — no plugin sub-namespace.

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
///
/// Returns the full channel state JSON, or `null` when no state exists.
pub(super) async fn handle_workflow_get_state(
    id: RequestId,
    channel: &str,
    state: &DaemonState,
) -> Response {
    let ps = state.persistent_state.lock().await;

    let value = ps
        .workflow_state
        .get(channel)
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    Response::success(id, serde_json::json!({ "state": value }))
}

/// Handle `workflow.set_state` RPC method.
///
/// Params:
/// - `channel` (required): channel name
/// - `key` (optional): dot-separated path for nested access (e.g. "tasks.42.excluded")
/// - `state` (required): JSON value to store
///
/// When `key` is provided, sets/removes the value at the nested path within the
/// channel's state object. A `null` value removes the key. When `key` is absent,
/// replaces the entire channel state.
pub(super) async fn handle_workflow_set_state(
    id: RequestId,
    channel: &str,
    key: Option<&str>,
    new_state: serde_json::Value,
    daemon_state: &DaemonState,
) -> Response {
    let mut ps = daemon_state.persistent_state.lock().await;

    if let Some(key) = key {
        // Nested key path: navigate/create intermediate objects, set leaf value.
        let root = ps
            .workflow_state
            .entry(channel.to_string())
            .or_insert_with(|| serde_json::json!({}));

        let parts: Vec<&str> = key.split('.').collect();
        let mut current = root;
        for part in &parts[..parts.len() - 1] {
            if !current.is_object() {
                *current = serde_json::json!({});
            }
            current = current
                .as_object_mut()
                .unwrap()
                .entry(*part)
                .or_insert_with(|| serde_json::json!({}));
        }

        let leaf = parts[parts.len() - 1];
        if new_state.is_null() {
            if let Some(obj) = current.as_object_mut() {
                obj.remove(leaf);
            }
        } else {
            if !current.is_object() {
                *current = serde_json::json!({});
            }
            current
                .as_object_mut()
                .unwrap()
                .insert(leaf.to_string(), new_state);
        }
    } else {
        ps.workflow_state.insert(channel.to_string(), new_state);
    };

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

/// Handle `workflow.set-lead-driven` RPC method.
///
/// Params:
/// - `channel` (required): channel name
/// - `enabled` (required): boolean — `true` to enable, `false` to disable
///
/// When enabled, the daemon relays workflow events as human-readable @mentions
/// to the channel lead instead of executing its built-in state machine.
pub(super) async fn handle_workflow_set_lead_driven(
    id: RequestId,
    channel: &str,
    enabled: bool,
    daemon_state: &DaemonState,
) -> Response {
    let mut ps = daemon_state.persistent_state.lock().await;

    if enabled {
        ps.lead_driven_channels.insert(channel.to_string());
    } else {
        ps.lead_driven_channels.remove(channel);
    }

    if let Err(e) = ps.save_for_repo(daemon_state.paths.dir_key()) {
        error!(
            channel = %channel,
            enabled = %enabled,
            "workflow.set-lead-driven: failed to save daemon state: {}",
            e
        );
        return Response::error(
            id,
            RpcError::new(-32603, format!("failed to persist state: {e}")),
        );
    }

    debug!(
        channel = %channel,
        enabled = %enabled,
        "workflow.set-lead-driven: lead-driven mode {}",
        if enabled { "enabled" } else { "disabled" }
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
