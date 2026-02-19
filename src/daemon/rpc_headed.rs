//! Headed wrapper intercom RPC handlers.
//!
//! These endpoints provide an adapter-neutral transport between the daemon and
//! headed wrappers. Wrappers claim an exclusive lease per session and consume
//! queued messages via poll+ack.

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

const DEFAULT_POLL_LIMIT: usize = 50;

pub(super) async fn handle_register(
    id: RequestId,
    session: &str,
    adapter_id: &str,
    provider: crate::auth::AuthProvider,
    state: &DaemonState,
) -> Response {
    match state.headed_register(session, adapter_id, provider).await {
        Ok((acked_id, lease_provider)) => Response::success(
            id,
            serde_json::json!({
                "session": session,
                "adapter_id": adapter_id,
                "provider": lease_provider.as_str(),
                "acked_id": acked_id
            }),
        ),
        Err(e) => Response::error(id, RpcError::new(-32602, e)),
    }
}

pub(super) async fn handle_unregister(
    id: RequestId,
    session: &str,
    adapter_id: &str,
    state: &DaemonState,
) -> Response {
    match state.headed_unregister(session, adapter_id).await {
        Ok(()) => Response::success(
            id,
            serde_json::json!({
                "session": session,
                "adapter_id": adapter_id,
                "unregistered": true
            }),
        ),
        Err(e) => Response::error(id, RpcError::new(-32602, e)),
    }
}

pub(super) async fn handle_heartbeat(
    id: RequestId,
    session: &str,
    adapter_id: &str,
    state: &DaemonState,
) -> Response {
    match state.headed_heartbeat(session, adapter_id).await {
        Ok(()) => Response::success(
            id,
            serde_json::json!({
                "session": session,
                "adapter_id": adapter_id,
                "ok": true
            }),
        ),
        Err(e) => Response::error(id, RpcError::new(-32602, e)),
    }
}

pub(super) async fn handle_poll(
    id: RequestId,
    session: &str,
    adapter_id: &str,
    after_id: u64,
    limit: Option<usize>,
    state: &DaemonState,
) -> Response {
    let limit = limit.unwrap_or(DEFAULT_POLL_LIMIT);
    match state
        .headed_poll(session, adapter_id, after_id, limit)
        .await
    {
        Ok((messages, capture_output)) => Response::success(
            id,
            serde_json::json!({
                "session": session,
                "adapter_id": adapter_id,
                "after_id": after_id,
                "messages": messages,
                "capture_output": capture_output,
            }),
        ),
        Err(e) => Response::error(id, RpcError::new(-32602, e)),
    }
}

pub(super) async fn handle_output(
    id: RequestId,
    session: &str,
    output: &str,
    state: &DaemonState,
) -> Response {
    state
        .headed_deliver_output(session, output.to_string())
        .await;
    Response::success(
        id,
        serde_json::json!({
            "session": session,
            "ok": true,
        }),
    )
}

pub(super) async fn handle_ack(
    id: RequestId,
    session: &str,
    adapter_id: &str,
    msg_id: u64,
    state: &DaemonState,
) -> Response {
    match state.headed_ack(session, adapter_id, msg_id).await {
        Ok(acked_id) => Response::success(
            id,
            serde_json::json!({
                "session": session,
                "adapter_id": adapter_id,
                "acked_id": acked_id
            }),
        ),
        Err(e) => Response::error(id, RpcError::new(-32602, e)),
    }
}

/// Enqueue a raw text payload to a headed session's intercom queue.
///
/// Used by the TUI to inject control characters (e.g., \x16 for Ctrl+V)
/// into an interactive lead or channel lead PTY session.
pub(super) async fn handle_enqueue(
    id: RequestId,
    session: &str,
    text: &str,
    state: &DaemonState,
) -> Response {
    // Validate session name: non-empty, alphanumeric plus hyphens/underscores only.
    if session.is_empty()
        || !session
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Response::error(id, RpcError::invalid_params());
    }

    // Enqueue with submit=false so no Enter keystroke follows the payload.
    state.enqueue_headed_text(session, text, false).await;

    Response::success(
        id,
        serde_json::json!({
            "session": session,
            "ok": true,
        }),
    )
}
