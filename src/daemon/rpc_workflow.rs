//! Workflow state RPC handlers.
//!
//! Handles `workflow.get_state` and `workflow.set_state` methods. These let
//! the Python workflow daemon read/write per-channel persistent state through
//! the daemon's Unix socket instead of directly accessing the filesystem.
//!
//! State is stored in the existing `workflow-state.json` files under
//! `~/.midtown/projects/<repo>/channels/<channel>/`. The `plugin` parameter
//! is an optional key for namespacing state by plugin within a channel.

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
/// Returns `null` when the state file doesn't exist or the plugin key is absent.
pub(super) async fn handle_workflow_get_state(
    id: RequestId,
    channel: &str,
    plugin: Option<&str>,
    state: &DaemonState,
) -> Response {
    let state_file = crate::paths::workflow_state_file(channel, state.paths.dir_key());

    let content = match tokio::fs::read_to_string(&state_file).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!(
                channel = %channel,
                "workflow.get_state: no state file, returning null"
            );
            return Response::success(id, serde_json::json!({ "state": null }));
        }
        Err(e) => {
            error!(
                channel = %channel,
                "workflow.get_state: failed to read state file: {}",
                e
            );
            return Response::error(
                id,
                RpcError::new(-32603, format!("failed to read state: {e}")),
            );
        }
    };

    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            error!(
                channel = %channel,
                "workflow.get_state: invalid JSON in state file: {}",
                e
            );
            return Response::error(
                id,
                RpcError::new(-32603, format!("corrupt state file: {e}")),
            );
        }
    };

    let value = match plugin {
        Some(key) => parsed.get(key).cloned().unwrap_or(serde_json::Value::Null),
        None => parsed,
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
/// is updated (or inserted), and the result is written back. When `plugin`
/// is absent, the entire state file is replaced.
pub(super) async fn handle_workflow_set_state(
    id: RequestId,
    channel: &str,
    plugin: Option<&str>,
    new_state: serde_json::Value,
    daemon_state: &DaemonState,
) -> Response {
    let state_file = crate::paths::workflow_state_file(channel, daemon_state.paths.dir_key());

    let final_value = match plugin {
        Some(key) => {
            // Merge into existing state at the plugin key.
            let mut existing = load_state_or_empty(&state_file).await;
            if let Some(obj) = existing.as_object_mut() {
                obj.insert(key.to_string(), new_state);
            } else {
                // State file wasn't an object — replace with a fresh object.
                let mut map = serde_json::Map::new();
                map.insert(key.to_string(), new_state);
                existing = serde_json::Value::Object(map);
            }
            existing
        }
        None => new_state,
    };

    // Ensure parent directory exists.
    if let Some(parent) = state_file.parent()
        && let Err(e) = tokio::fs::create_dir_all(parent).await
    {
        error!(
            channel = %channel,
            "workflow.set_state: failed to create directory: {}",
            e
        );
        return Response::error(
            id,
            RpcError::new(-32603, format!("failed to create state dir: {e}")),
        );
    }

    let json = match serde_json::to_string_pretty(&final_value) {
        Ok(j) => j,
        Err(e) => {
            error!(
                channel = %channel,
                "workflow.set_state: failed to serialize state: {}",
                e
            );
            return Response::error(
                id,
                RpcError::new(-32603, format!("serialization error: {e}")),
            );
        }
    };

    // Atomic write: write to a temp file in the same directory, then rename.
    let tmp_path = state_file.with_extension("json.tmp");
    if let Err(e) = tokio::fs::write(&tmp_path, json.as_bytes()).await {
        error!(
            channel = %channel,
            "workflow.set_state: failed to write temp file: {}",
            e
        );
        return Response::error(
            id,
            RpcError::new(-32603, format!("failed to write state: {e}")),
        );
    }

    if let Err(e) = tokio::fs::rename(&tmp_path, &state_file).await {
        error!(
            channel = %channel,
            "workflow.set_state: failed to rename temp file: {}",
            e
        );
        // Clean up the temp file on failure.
        let _ = tokio::fs::remove_file(&tmp_path).await;
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

/// Load existing state from disk, returning an empty JSON object if the file
/// doesn't exist or contains invalid JSON.
async fn load_state_or_empty(path: &std::path::Path) -> serde_json::Value {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => serde_json::from_str(&content).unwrap_or(serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    }
}

#[path = "rpc_workflow_tests.rs"]
#[cfg(test)]
mod tests;
