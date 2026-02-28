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
    // Deduplicate: normalize and hash the insight content.
    // Must happen before the channel-lead check so that channel-lead insights
    // still enter the hash set. This prevents a non-lead coworker from
    // re-posting the same insight text after a channel lead already reported it.
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

    // Channel leads auto-post all output to their channel already.
    // Suppress the explicit insight.report to avoid double-posting.
    {
        let ps = state.persistent_state.lock().await;
        let is_channel_lead = ps
            .sessions
            .values()
            .any(|s| s.current_name.as_deref() == Some(agent) && s.coworker_type == "channel-lead");
        if is_channel_lead {
            debug!(
                "insight.report: suppressing insight from channel lead {}, already auto-posted",
                agent
            );
            return Response::success(
                id,
                serde_json::json!({
                    "posted": false,
                    "reason": "channel_lead",
                }),
            );
        }
    }

    // Resolve channel and thread: explicit > coworker's task channel/thread > main.
    // When a coworker reports an insight without --channel, auto-route to their
    // assigned task's channel so insights don't flood the main channel.
    // Also resolve the task's thread binding so insights thread under the task
    // announcement (parallels fork_bound_threads for channel.post).
    let (resolved_channel, resolved_thread_id): (Option<String>, Option<String>) =
        if channel.is_none() {
            let ps = state.persistent_state.lock().await;
            let task_id = ps
                .sessions
                .values()
                .find(|r| r.current_name.as_deref() == Some(agent))
                .and_then(|r| r.task_id.as_deref());
            let ch = task_id.and_then(|tid| ps.task_channel.get(tid).cloned());
            let thread = task_id.and_then(|tid| ps.task_thread_id.get(tid).cloned());
            (ch, thread)
        } else {
            (None, None)
        };

    // Post insight to specified channel (or main if None)
    let channel_name: &str = channel
        .or(resolved_channel.as_deref())
        .unwrap_or_else(|| state.channel_router.default_channel_name());
    let insight_content = format!("💡 {}", insight);
    let msg = if let Some(ref thread_id) = resolved_thread_id {
        crate::message::Message::thread_reply(
            channel_name,
            agent,
            insight_content,
            thread_id,
            crate::message::MessageType::Text,
        )
    } else {
        crate::message::Message::for_channel(
            channel_name,
            agent,
            insight_content,
            crate::message::MessageType::Text,
        )
    };
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
