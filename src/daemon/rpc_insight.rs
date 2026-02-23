//! Insight reporting RPC handler.
//!
//! Handles the `insight.report` method: deduplicates via in-memory hashing,
//! posts insights to the channel, and spawns architect sessions for optional
//! diagram generation.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use tracing::{debug, info, warn};

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;
use super::helpers::*;

// ============================================================================
// Handler
// ============================================================================

/// Handle insight.report RPC method.
///
/// Deduplicates via in-memory hash set, posts the insight to the channel,
/// and spawns a headless architect session to optionally generate a diagram.
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
    let nudge_effect = crate::daemon::effects::Effect::NudgeChannelLead {
        channel_name: channel_name.to_string(),
        reason: crate::daemon::wake_reason::WakeReason::InsightPosted {
            insight: insight.to_string(),
            agent: agent.to_string(),
            msg_id: msg.id.clone(),
        },
    };
    crate::daemon::effects::execute_effects(vec![nudge_effect], state).await;

    // Determine working directory for the architect session.
    let cwd = if is_coworker_sender(agent, &state.repo_name) {
        let worktree = crate::paths::coworkers_dir_for_repo(&state.repo_name).join(agent);
        if worktree.exists() {
            worktree
        } else {
            state.all_repo_paths.first().cloned().unwrap_or_default()
        }
    } else {
        state.all_repo_paths.first().cloned().unwrap_or_default()
    };

    // Resolve auth for the architect session
    let auth_provider = crate::config::get_execution_provider_for_role(
        &state.repo_name,
        crate::config::ExecutionRole::Architect,
    );
    let auth_profile_dir =
        crate::auth::active_profile_dir_for_project_with_provider(&state.repo_name, auth_provider);

    // Spawn the architect task asynchronously
    let repo_name = state.repo_name.clone();
    let insight_owned = insight.to_string();
    // Pass None when posting to the main channel so the architect skips diagram
    // generation there (noise guard). For topic channels — whether explicitly
    // provided or auto-resolved from the coworker's task — pass Some so diagrams
    // are posted to the correct channel.
    let channel_owned = if channel_name != state.channel_router.default_channel_name() {
        Some(channel_name.to_string())
    } else {
        None
    };
    tokio::spawn(async move {
        super::architect::generate_insight_diagram(
            insight_owned,
            cwd,
            repo_name,
            channel_owned,
            auth_provider,
            auth_profile_dir,
        )
        .await;
    });

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
