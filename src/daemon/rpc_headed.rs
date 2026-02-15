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
        Ok(messages) => Response::success(
            id,
            serde_json::json!({
                "session": session,
                "adapter_id": adapter_id,
                "after_id": after_id,
                "messages": messages
            }),
        ),
        Err(e) => Response::error(id, RpcError::new(-32602, e)),
    }
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
