//! Headless execution and snapshot RPC handlers.
//!
//! Handles `headless.execute` (run a one-shot headless Claude session) and
//! `snapshot` (return the full WorldSnapshot for debugging).

use tracing::{error, info, warn};

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

// ============================================================================
// Handlers
// ============================================================================

/// Handle headless.execute RPC method.
pub(super) async fn handle_headless_execute(
    id: RequestId,
    prompt: &str,
    config: &crate::headless::HeadlessConfig,
) -> Response {
    info!(
        "Headless execute: model={}, prompt_len={}, has_schema={}",
        config.model,
        prompt.len(),
        config.json_schema.is_some()
    );

    let timeout = std::time::Duration::from_secs(300);

    match crate::headless::execute(config, prompt, timeout).await {
        Ok(result) => {
            info!(
                "Headless execute complete: cost=${:.4}, duration={}ms, error={}",
                result.cost_usd.unwrap_or(0.0),
                result.duration_ms.unwrap_or(0),
                result.is_error,
            );
            Response::success(
                id,
                serde_json::json!({
                    "success": !result.is_error,
                    "result": result.result,
                    "cost_usd": result.cost_usd,
                    "duration_ms": result.duration_ms,
                    "session_id": result.session_id,
                }),
            )
        }
        Err(e) => {
            warn!("Headless execute failed: {}", e);
            Response::error(
                id,
                RpcError::new(-32603, format!("Headless execution failed: {}", e)),
            )
        }
    }
}

/// Handle snapshot RPC method — collect and return the full WorldSnapshot.
pub(super) async fn handle_snapshot(id: RequestId, state: &DaemonState) -> Response {
    let default_channel = match state.channel_router.default_channel() {
        Ok(ch) => ch,
        Err(e) => {
            error!("Failed to get default channel for snapshot: {}", e);
            return Response::error(id, RpcError::new(-32603, e.to_string()));
        }
    };
    let snapshot = super::snapshot::collect_world_snapshot(state)
        .await
        .with_debug_context(&default_channel);
    match serde_json::to_value(&snapshot) {
        Ok(value) => Response::success(id, value),
        Err(e) => Response::error(
            id,
            RpcError::new(-32603, format!("Failed to serialize snapshot: {}", e)),
        ),
    }
}
