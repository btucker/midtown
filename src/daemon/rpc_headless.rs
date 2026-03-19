//! One-shot execution and snapshot RPC handlers.
//!
//! Handles `oneshot.execute` (run a one-shot Claude session) and
//! `snapshot` (return daemon persistent state for debugging).

use tracing::{info, warn};

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

// ============================================================================
// Handlers
// ============================================================================

/// Handle one-shot execute RPC method.
pub(super) async fn handle_headless_execute(
    id: RequestId,
    prompt: &str,
    config: &crate::headless::HeadlessConfig,
) -> Response {
    info!(
        "One-shot execute: model={}, prompt_len={}, has_schema={}",
        config.model,
        prompt.len(),
        config.json_schema.is_some()
    );

    let timeout = std::time::Duration::from_secs(300);

    match crate::headless::execute(config, prompt, timeout).await {
        Ok(result) => {
            info!(
                "One-shot execute complete: cost=${:.4}, duration={}ms, error={}",
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
            warn!("One-shot execute failed: {}", e);
            Response::error(
                id,
                RpcError::new(-32603, format!("One-shot execution failed: {}", e)),
            )
        }
    }
}

/// Handle snapshot RPC method — return DaemonPersistentState with tick fields populated.
pub(super) async fn handle_snapshot(id: RequestId, state: &DaemonState) -> Response {
    // Populate tick fields so the snapshot reflects current ephemeral state
    let _tasks = super::tick::prepare_tick(state).await;
    let ps = state.persistent_state.lock().await;
    match serde_json::to_value(&*ps) {
        Ok(value) => Response::success(id, value),
        Err(e) => Response::error(
            id,
            RpcError::new(-32603, format!("Failed to serialize snapshot: {}", e)),
        ),
    }
}
