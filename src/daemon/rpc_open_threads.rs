//! RPC handlers for managing per-channel open thread sets.
//!
//! The `openThreads` set tracks which threads a user wants visible in
//! the sidebar. Persisted server-side so it syncs across all clients.

use tracing::error;

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

/// Handle `channel.open_threads` — get the open thread set for a channel.
pub(super) async fn handle_open_threads_get(
    id: RequestId,
    channel: &str,
    state: &DaemonState,
) -> Response {
    let ps = state.persistent_state.lock().await;
    let threads: Vec<&String> = ps
        .open_threads
        .get(channel)
        .map(|s| s.iter().collect())
        .unwrap_or_default();
    Response::success(id, serde_json::json!({ "threads": threads }))
}

/// Handle `channel.open_threads.set` — replace the open thread set for a channel.
pub(super) async fn handle_open_threads_set(
    id: RequestId,
    channel: &str,
    threads: Vec<String>,
    state: &DaemonState,
) -> Response {
    let thread_set: std::collections::HashSet<String> = threads.into_iter().collect();

    let mut ps = state.persistent_state.lock().await;
    ps.open_threads
        .insert(channel.to_string(), thread_set.clone());

    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
        error!("Failed to save daemon-state.json: {}", e);
        return Response::error(id, RpcError::new(-32603, format!("failed to persist: {e}")));
    }
    drop(ps);

    // Broadcast to all connected web clients
    state.broadcast_web_update(crate::web::WebUpdate::OpenThreadsChanged(
        crate::web::OpenThreadsChangedData {
            channel: channel.to_string(),
            threads: thread_set.into_iter().collect(),
        },
    ));

    Response::success(id, serde_json::json!({ "ok": true }))
}

#[path = "rpc_open_threads_tests.rs"]
#[cfg(test)]
mod tests;
