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

    // Post insight to specified channel (or main if None)
    let channel_name = channel.unwrap_or_else(|| state.channel_router.default_channel_name());
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

    // Determine working directory for the architect session.
    let cwd = if is_coworker_sender(agent) {
        let worktree = crate::paths::coworkers_dir_for_repo(&state.repo_name).join(agent);
        if worktree.exists() {
            worktree
        } else {
            state.all_repo_paths.first().cloned().unwrap_or_default()
        }
    } else {
        state.all_repo_paths.first().cloned().unwrap_or_default()
    };

    // Spawn the architect task asynchronously
    let repo_name = state.repo_name.clone();
    let insight_owned = insight.to_string();
    let channel_owned = channel.map(|s| s.to_string());
    tokio::spawn(async move {
        super::architect::generate_insight_diagram(insight_owned, cwd, repo_name, channel_owned)
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_insight_deterministic() {
        let hash1 = hash_insight("Test insight content");
        let hash2 = hash_insight("Test insight content");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_insight_different_content() {
        let hash1 = hash_insight("Insight one");
        let hash2 = hash_insight("Insight two");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_hash_insight_normalizes_whitespace() {
        let hash1 = hash_insight("This is an insight");
        let hash2 = hash_insight("  This  is   an   insight  ");
        let hash3 = hash_insight("This\n  is\nan\ninsight");
        let hash4 = hash_insight("THIS IS AN INSIGHT");

        assert_eq!(hash1, hash2, "extra whitespace should be normalized");
        assert_eq!(hash1, hash3, "newlines should be normalized");
        assert_eq!(hash1, hash4, "case should be normalized");
    }
}
