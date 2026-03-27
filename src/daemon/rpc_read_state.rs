//! RPC handlers for per-user read state (threads and channels).
//!
//! Read state tracks when a user last read a thread or channel,
//! enabling unread indicators that sync across devices.

use tracing::error;

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

const DEFAULT_USER: &str = "default";

/// Handle `read_state.get` — returns all read timestamps for current user.
pub(super) async fn handle_read_state_get(id: RequestId, state: &DaemonState) -> Response {
    let ps = state.persistent_state.lock().await;
    let read_state = ps.read_state.get(DEFAULT_USER);

    let threads = read_state
        .map(|rs| &rs.threads)
        .cloned()
        .unwrap_or_default();
    let channels = read_state
        .map(|rs| &rs.channels)
        .cloned()
        .unwrap_or_default();

    Response::success(
        id,
        serde_json::json!({ "threads": threads, "channels": channels }),
    )
}

/// Handle `read_state.mark_read` — marks a thread or channel as read.
pub(super) async fn handle_read_state_mark_read(
    id: RequestId,
    item_type: &str,
    item_id: &str,
    timestamp: &str,
    state: &DaemonState,
) -> Response {
    if item_type != "thread" && item_type != "channel" {
        return Response::error(
            id,
            RpcError::new(
                -32602,
                format!("type must be 'thread' or 'channel', got '{item_type}'"),
            ),
        );
    }

    let mut ps = state.persistent_state.lock().await;
    let read_state = ps.read_state.entry(DEFAULT_USER.to_string()).or_default();

    match item_type {
        "thread" => {
            read_state
                .threads
                .insert(item_id.to_string(), timestamp.to_string());
        }
        "channel" => {
            read_state
                .channels
                .insert(item_id.to_string(), timestamp.to_string());
        }
        _ => unreachable!(),
    }

    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
        error!("Failed to save daemon-state.json: {}", e);
        return Response::error(id, RpcError::new(-32603, format!("failed to persist: {e}")));
    }
    drop(ps);

    state.broadcast_web_update(crate::web::WebUpdate::ReadStateChanged(
        crate::web::ReadStateChangedData {
            item_type: item_type.to_string(),
            id: item_id.to_string(),
            timestamp: timestamp.to_string(),
        },
    ));

    Response::success(id, serde_json::json!({ "ok": true }))
}

#[path = "rpc_read_state_tests.rs"]
#[cfg(test)]
mod tests;
