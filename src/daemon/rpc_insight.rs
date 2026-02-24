//! Insight reporting RPC handler.
//!
//! Handles the `insight.report` method: deduplicates via in-memory hashing
//! and posts insights to the channel.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use tracing::{debug, info, warn};

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

// ============================================================================
// Handler
// ============================================================================

/// Handle insight.report RPC method.
///
/// Deduplicates via in-memory hash set, posts the insight to the channel,
/// and nudges the channel lead.
pub(super) async fn handle_insight_report(
    id: RequestId,
    agent: &str,
    insight: &str,
    channel: Option<&str>,
    state: &DaemonState,
) -> Response {
    // Deduplicate: normalize and hash the insight content
    let hash = hash_insight(insight);
    {
        let mut hashes = state.insight_hashes.lock().unwrap();
        if !hashes.insert(hash) {
            debug!("insight.report: duplicate insight from {}, skipping", agent);
            return Response::success(
                id,
                serde_json::json!({
                    "posted": false,
                    "reason": "duplicate",
                }),
            );
        }
    }

    // Resolve channel: explicit > coworker's task channel > main.
    // When a coworker reports an insight without --channel, auto-route to their
    // assigned task's channel so insights don't flood the main channel.
    let resolved_channel: Option<String> = if channel.is_none() {
        let ps = state.persistent_state.lock().await;
        ps.headless_sessions
            .get(agent)
            .and_then(|s| s.task_id)
            .map(|tid| tid.to_string())
            .and_then(|tid| ps.task_channel.get(&tid).cloned())
    } else {
        None
    };

    // Post insight to specified channel (or main if None)
    let channel_name: &str = channel
        .or(resolved_channel.as_deref())
        .unwrap_or_else(|| state.channel_router.default_channel_name());
    let msg = crate::message::Message::for_channel(
        channel_name,
        agent,
        format!("💡 {}", insight),
        crate::message::MessageType::Text,
    );
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        warn!("insight.report: failed to post to channel: {}", e);
        return Response::error(
            id,
            RpcError::new(-32603, format!("Failed to post insight: {}", e)),
        );
    }

    info!(
        "insight.report: posted insight from {} to channel '{}'",
        agent, channel_name
    );

    // Nudge channel lead about the insight (works for both topic and main channels).
    // For topic channels: spawns-if-dead, resumes-if-idle, nudges-if-alive.
    // For main channel: nudges the project lead.
    let task_id = state.get_task_id_for_coworker(agent);
    let nudge_effect = crate::daemon::effects::Effect::NudgeChannelLead {
        channel_name: channel_name.to_string(),
        reason: crate::daemon::wake_reason::WakeReason::InsightPosted {
            insight: insight.to_string(),
            agent: agent.to_string(),
            msg_id: msg.id.clone(),
            task_id,
            channel_name: channel_name.to_string(),
        },
    };
    crate::daemon::effects::execute_effects(vec![nudge_effect], state).await;

    Response::success(
        id,
        serde_json::json!({
            "posted": true,
        }),
    )
}

// ============================================================================
// Helper functions
// ============================================================================

/// Hash insight content for deduplication.
fn hash_insight(insight: &str) -> u64 {
    let normalized: String = insight
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    hasher.finish()
}

#[path = "rpc_insight_tests.rs"]
#[cfg(test)]
mod tests;
