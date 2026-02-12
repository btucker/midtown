//! RPC handlers for miscellaneous operations (status, reminders, insight, headless).
//!
//! Extracted from `rpc.rs` to keep the main dispatch module focused on routing.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use tracing::{debug, error, info, warn};

use crate::message::Message;
use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;
use super::constants::*;
use super::helpers::*;

use super::rpc_coworker::collect_coworker_list;

// ============================================================================
// Status handler
// ============================================================================

/// Handle status RPC method.
///
/// This handler runs blocking operations (gh CLI, file I/O) in spawn_blocking
/// to avoid blocking the async runtime and causing RPC timeouts.
pub(super) async fn handle_status(id: RequestId, state: &DaemonState) -> Response {
    let coworkers = collect_coworker_list(state);

    // Get cached PR data from the daemon's periodic polling (every 30s for open PRs,
    // every 5 minutes for merged PRs). This avoids synchronous gh CLI calls that can
    // timeout under GitHub API rate limiting.
    //
    // During daemon startup (before the first PR poll completes), return empty arrays
    // rather than stale data. The first open PR poll completes within ~5 seconds, so
    // this window is brief.
    let (pull_requests, merged_prs) = {
        let cache = state.pr_coworker_cache.read().unwrap();
        if cache.pr_poll_initialized {
            (cache.open_prs_data.clone(), cache.merged_prs_data.clone())
        } else {
            // PR poll hasn't completed yet - return empty arrays during startup
            (Vec::new(), Vec::new())
        }
    };

    // Run blocking file I/O operations in spawn_blocking.
    // Note: get_all_tasks reads from Claude Code task storage (local filesystem),
    // not GitHub API, so it's fast and doesn't cause rate limit timeouts.
    let (tasks, recent_activity) = match tokio::task::spawn_blocking(move || {
        let tasks = get_all_tasks();
        let recent_activity = get_recent_channel_activity();
        (tasks, recent_activity)
    })
    .await
    {
        Ok(result) => result,
        Err(e) => {
            error!("spawn_blocking panic in status handler: {}", e);
            return Response::error(id, RpcError::new(-32603, "Internal error".to_string()));
        }
    };

    let pending_count = tasks
        .iter()
        .filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("pending"))
        .count();

    // Get GitHub API rate limit state
    let rate_limit = {
        let ps = state.persistent_state.lock().await;
        ps.github.rate_limit.clone()
    };

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "daemon_running": true,
            "active_coworkers": state.coworkers.count(),
            "max_coworkers": state.max_coworkers,
            "max_dev_coworkers": state.max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1),
            "pending_tasks": pending_count,
            "socket_path": state.socket_path.to_string_lossy(),
            "coworkers": coworkers,
            "tasks": tasks,
            "pull_requests": pull_requests,
            "merged_prs": merged_prs,
            "recent_activity": recent_activity,
            "github_rate_limit": {
                "graphql": {
                    "remaining": rate_limit.graphql.remaining,
                    "limit": rate_limit.graphql.limit,
                    "used": rate_limit.graphql.used,
                    "reset": rate_limit.graphql.reset,
                    "remaining_pct": (rate_limit.graphql.remaining_pct() * 100.0) as u32,
                },
                "rest": {
                    "remaining": rate_limit.core.remaining,
                    "limit": rate_limit.core.limit,
                    "used": rate_limit.core.used,
                    "reset": rate_limit.core.reset,
                    "remaining_pct": (rate_limit.core.remaining_pct() * 100.0) as u32,
                },
                "summary": rate_limit.summary(),
            },
        }),
    )
}

// REMOVED: get_open_prs() and format_pr_status()
// These functions made synchronous gh CLI calls on every RPC, causing timeouts under
// GitHub API rate limiting. Now handle_status uses cached PR data from the daemon's
// periodic polling (see pr_coworker_cache in daemon/mod.rs and poll_prs_for_issues in
// daemon/pr.rs). The formatting logic moved to format_pr_status_for_rpc() in pr.rs.

/// Get all tasks from Claude Code task storage with their status.
fn get_all_tasks() -> Vec<serde_json::Value> {
    crate::tasks::read_tasks()
        .into_iter()
        .map(|task| {
            let status = match task.status {
                crate::tasks::TaskStatus::Pending => "pending",
                crate::tasks::TaskStatus::InProgress => "in_progress",
                crate::tasks::TaskStatus::Completed => "completed",
            };
            serde_json::json!({
                "id": task.id,
                "subject": task.subject,
                "status": status,
                "assignee": task.owner,
            })
        })
        .collect()
}

/// Get recent channel activity.
fn get_recent_channel_activity() -> Vec<serde_json::Value> {
    // Try to read from the default channel location
    let channel_file = crate::paths::channel_file_for_repo("default");

    if !channel_file.exists() {
        return Vec::new();
    }

    // Read the last few messages from the channel
    match std::fs::read_to_string(&channel_file) {
        Ok(content) => {
            let messages: Vec<serde_json::Value> = content
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();

            // Get the last 5 messages, most recent last
            messages
                .into_iter()
                .rev()
                .take(5)
                .map(|msg| {
                    serde_json::json!({
                        "timestamp": msg.get("timestamp")
                            .and_then(|t| t.as_str())
                            .map(|t| {
                                // Format timestamp for display (just time portion)
                                if t.len() > 11 {
                                    t[11..16].to_string()
                                } else {
                                    t.to_string()
                                }
                            })
                            .unwrap_or_default(),
                        "from": msg.get("from").and_then(|f| f.as_str()).unwrap_or("unknown"),
                        "summary": truncate_message(
                            msg.get("content").and_then(|c| c.as_str()).unwrap_or(""),
                            60
                        ),
                    })
                })
                .collect()
        }
        Err(_) => Vec::new(),
    }
}

// ============================================================================
// Reminder handlers
// ============================================================================

/// Handle reminder.create RPC method.
pub(super) async fn handle_reminder_create(
    id: RequestId,
    message: &str,
    state: &DaemonState,
) -> Response {
    let mut ps = state.persistent_state.lock().await;
    let reminder_id = ps.reminders.add(
        crate::reminders::ReminderTrigger::AllWorkMerged,
        message.to_string(),
    );

    if let Err(e) = ps.save_for_repo(&state.repo_name) {
        error!("Failed to save daemon-state.json: {}", e);
    }

    let confirmation = format!(
        "Reminder set (id: {}): I'll notify you when all tasks are completed and all PRs are merged. Message: \"{}\"",
        reminder_id, message
    );
    info!("{}", confirmation);
    Response::success(id, serde_json::json!({ "message": confirmation }))
}

/// Handle reminder.list RPC method.
pub(super) async fn handle_reminder_list(id: RequestId, state: &DaemonState) -> Response {
    let ps = state.persistent_state.lock().await;
    let active = ps.reminders.active();

    if active.is_empty() {
        return Response::success(id, serde_json::json!({ "message": "No active reminders." }));
    }

    let lines: Vec<String> = active
        .iter()
        .map(|r| {
            format!(
                "  {} [{}] \"{}\" (created {})",
                r.id,
                r.trigger,
                r.message,
                r.created_at.format("%Y-%m-%d %H:%M UTC")
            )
        })
        .collect();

    let output = format!("Active reminders:\n{}", lines.join("\n"));
    Response::success(id, serde_json::json!({ "message": output }))
}

/// Handle reminder.cancel RPC method.
pub(super) async fn handle_reminder_cancel(
    id: RequestId,
    reminder_id: &str,
    state: &DaemonState,
) -> Response {
    let mut ps = state.persistent_state.lock().await;
    if ps.reminders.cancel(reminder_id) {
        if let Err(e) = ps.save_for_repo(&state.repo_name) {
            error!("Failed to save daemon-state.json: {}", e);
        }
        let msg = format!("Reminder {} cancelled.", reminder_id);
        info!("{}", msg);
        Response::success(id, serde_json::json!({ "message": msg }))
    } else {
        Response::error(
            id,
            RpcError::new(-32602, format!("Reminder '{}' not found", reminder_id)),
        )
    }
}

// ============================================================================
// Insight handler
// ============================================================================

/// Handle insight.report RPC method.
///
/// Called by the insight PostToolUse hook when a coworker or lead generates
/// an insight block. Deduplicates via in-memory hash set, posts the insight
/// to the channel, and spawns a headless architect session to optionally
/// generate a Mermaid diagram.
///
/// The optional `channel` parameter specifies which channel to post the insight to.
/// If None, defaults to the main channel. Architect diagrams are only posted when
/// `channel` is Some (topic channel) — diagrams are skipped for the main channel
/// to avoid noise.
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
    let msg = Message::for_channel(
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
    // For coworkers, use their worktree; for lead, use the main repo dir.
    let cwd = if is_coworker_sender(agent) {
        let worktree = crate::paths::coworkers_dir_for_repo(&state.repo_name).join(agent);
        if worktree.exists() {
            worktree
        } else {
            // Worktree gone — fall back to main repo dir
            state.all_repo_paths.first().cloned().unwrap_or_default()
        }
    } else {
        state.all_repo_paths.first().cloned().unwrap_or_default()
    };

    // Spawn the architect task asynchronously - pass channel so diagram routes to same channel as insight
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

/// Hash insight content for deduplication.
///
/// Normalizes text (trim, collapse whitespace, lowercase) before hashing
/// to prevent duplicates from minor formatting variations.
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
// Headless handler
// ============================================================================

/// Handle headless.execute RPC method.
///
/// Spawns a headless Claude Code session and runs a one-shot prompt. Returns
/// the final result with cost and duration. The session uses JSON streaming
/// internally but this RPC endpoint blocks until the result is available.
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

    // Default timeout of 5 minutes for RPC-invoked headless sessions
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
