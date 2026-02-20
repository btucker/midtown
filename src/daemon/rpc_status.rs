//! Status and monitoring RPC handlers.
//!
//! Handles the `status` method, providing a comprehensive view of daemon state
//! including coworkers, tasks, PRs, and channel activity.

use tracing::error;

use super::DaemonState;
use super::constants::*;
use super::helpers::*;
use crate::rpc::{RequestId, Response, RpcError};

// ============================================================================
// Handlers
// ============================================================================

/// Handle status RPC method.
///
/// This handler runs blocking operations (gh CLI, file I/O) in spawn_blocking
/// to avoid blocking the async runtime and causing RPC timeouts.
pub(super) async fn handle_status(id: RequestId, state: &DaemonState) -> Response {
    // Build a map of coworker name -> task display string from in_progress tasks
    // This is the source of truth for what each coworker is working on
    // Format: "!1234 Task subject" (task ID + subject)
    let coworker_tasks: std::collections::HashMap<String, String> =
        crate::tasks::get_in_progress_tasks_with_subjects()
            .into_iter()
            .filter_map(|(task_id, subject, owner)| {
                if owner.is_empty() {
                    None
                } else {
                    // Include both task ID and subject in the display string
                    let task_display = format!("!{} {}", task_id, subject);
                    Some((owner.to_lowercase(), task_display))
                }
            })
            .collect();

    // Get coworkers with their details, looking up current task from task storage
    let coworkers: Vec<serde_json::Value> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| {
            // Look up current task from task storage (case-insensitive)
            let current_task = coworker_tasks.get(&cw.name.to_lowercase()).cloned();
            serde_json::json!({
                "name": cw.name,
                "status": cw.status.to_string(),
                "current_task": current_task,
                "started_at": cw.started_at.to_rfc3339(),
                "provider": cw.provider.as_str(),
                "profile": cw.profile,
            })
        })
        .collect();

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

    // Get GitHub API rate limit state and channel lead names together to avoid
    // locking persistent_state twice.
    let (rate_limit, channel_lead_names) = {
        let ps = state.persistent_state.lock().await;
        let names: std::collections::HashSet<String> =
            ps.channel_lead_sessions.keys().cloned().collect();
        (ps.github.rate_limit.clone(), names)
    };

    let (coworkers, active_coworker_count) =
        tag_channel_leads_and_count(coworkers, &channel_lead_names);

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "daemon_running": true,
            "active_coworkers": active_coworker_count,
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

// ============================================================================
// Helper functions
// ============================================================================

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

/// Tag each coworker JSON value with `is_channel_lead` and return the count
/// of non-lead coworkers. Channel leads are persistent domain experts and
/// do not consume coworker slots.
fn tag_channel_leads_and_count(
    coworkers: Vec<serde_json::Value>,
    channel_lead_names: &std::collections::HashSet<String>,
) -> (Vec<serde_json::Value>, usize) {
    let coworkers: Vec<serde_json::Value> = coworkers
        .into_iter()
        .map(|mut cw| {
            let name = cw
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let is_lead = channel_lead_names.contains(&name);
            if let Some(obj) = cw.as_object_mut() {
                obj.insert("is_channel_lead".to_string(), is_lead.into());
            }
            cw
        })
        .collect();
    let active_count = coworkers
        .iter()
        .filter(|cw| {
            !cw.get("is_channel_lead")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .count();
    (coworkers, active_count)
}

#[path = "rpc_status_tests.rs"]
#[cfg(test)]
mod tests;
