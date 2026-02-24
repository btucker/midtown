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
    // Build a map of coworker name -> task display string from in_progress tasks.
    // Format: "!1234 Task subject" (task ID + subject)
    let coworker_tasks: std::collections::HashMap<String, String> =
        crate::tasks::get_in_progress_tasks_with_subjects()
            .into_iter()
            .filter_map(|(task_id, subject, owner)| {
                if owner.is_empty() {
                    None
                } else {
                    let task_display = format!("!{} {}", task_id, subject);
                    Some((owner.to_lowercase(), task_display))
                }
            })
            .collect();

    // Snapshot live workflow state (phase, task_id) from coworker_records.
    // This reflects what coworkers are *actually* doing, not just task ownership.
    let coworker_records: std::collections::HashMap<String, crate::rules::CoworkerRecord> = {
        let records = state.coworker_records.read().await;
        records.clone()
    };

    // Read all persistent state in a single lock: reviewer assignments, worktree PR map,
    // rate limit, channel lead names, and task-message-id map. Avoids multiple lock acquires.
    let (reviewer_pr_map, worktree_pr_map, rate_limit, channel_lead_names, task_message_ids) = {
        let ps = state.persistent_state.lock().await;
        let rev_map: std::collections::HashMap<String, u64> = ps
            .github
            .active_assignments()
            .iter()
            .map(|(pr_number, assignment)| (assignment.reviewer.clone(), *pr_number))
            .collect();
        let wt_map: std::collections::HashMap<String, u64> = ps
            .worktree_registry
            .all_assignments()
            .iter()
            .filter_map(|(_, assignment)| {
                let coworker = assignment.current_coworker.as_ref()?;
                let pr_number = assignment.pr_number?;
                Some((coworker.clone(), pr_number))
            })
            .collect();
        let channel_leads: std::collections::HashSet<String> =
            ps.channel_lead_sessions.keys().cloned().collect();
        let msg_ids = ps.task_message_id.clone();
        (
            rev_map,
            wt_map,
            ps.github.rate_limit.clone(),
            channel_leads,
            msg_ids,
        )
    };

    // Get token usage from session manager (keyed by coworker name).
    let token_usage = state.session_manager.get_token_usage().await;

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
    // Note: get_all_tasks and read_tasks read from Claude Code task storage (local
    // filesystem), not GitHub API, so they're fast and don't cause rate limit timeouts.
    let (tasks, recent_activity, task_pr_map) = match tokio::task::spawn_blocking(move || {
        let tasks = get_all_tasks(&task_message_ids);
        let recent_activity = get_recent_channel_activity();
        // Build task -> PR number map from task files with explicit PR associations.
        let task_pr_map: std::collections::HashMap<u32, u64> = crate::tasks::read_tasks()
            .into_iter()
            .filter_map(|task| {
                let task_id: u32 = task.id.parse().ok()?;
                let pr = task.pr?;
                Some((task_id, pr))
            })
            .collect();
        (tasks, recent_activity, task_pr_map)
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

    // Get coworkers with their details, looking up current task from task storage.
    // Exclude the lead session (named after the repo) — it is the project Lead,
    // not a coworker, and must not appear in the coworkers status box.
    let coworkers: Vec<serde_json::Value> = state
        .coworkers
        .list()
        .iter()
        .filter(|cw| !cw.name.eq_ignore_ascii_case(&state.repo_name))
        .map(|cw| {
            // Look up current task from task storage (case-insensitive)
            let current_task = coworker_tasks.get(&cw.name.to_lowercase()).cloned();
            // Look up token usage from session manager
            let (input_tokens, output_tokens) =
                token_usage.get(&cw.name).copied().unwrap_or((0, 0));
            // Get live workflow phase and task_id from coworker_records
            let record = coworker_records.get(&cw.name);
            let workflow_phase = record.and_then(|r| r.workflow_phase);
            let record_task_id = record.and_then(|r| r.task_id);
            // Find PR number for this coworker (priority: task file > reviewer > worktree)
            let pr_number = resolve_pr_number(
                record_task_id,
                &cw.name,
                &task_pr_map,
                &reviewer_pr_map,
                &worktree_pr_map,
            );
            serde_json::json!({
                "name": cw.name,
                "status": cw.status.to_string(),
                "current_task": current_task,
                "started_at": cw.started_at.to_rfc3339(),
                "provider": cw.provider.as_str(),
                "profile": cw.profile,
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "phase": workflow_phase.map(|p| p.abbreviation()),
                "pr_number": pr_number,
            })
        })
        .collect();

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

/// Resolve the PR number for a coworker using a priority chain:
/// 1. Task file PR association (task_id → PR mapping)
/// 2. Active reviewer assignment (coworker reviewing a PR)
/// 3. Worktree registry (coworker's worktree has a PR)
fn resolve_pr_number(
    task_id: Option<u32>,
    coworker_name: &str,
    task_pr_map: &std::collections::HashMap<u32, u64>,
    reviewer_pr_map: &std::collections::HashMap<String, u64>,
    worktree_pr_map: &std::collections::HashMap<String, u64>,
) -> Option<u64> {
    task_id
        .and_then(|tid| task_pr_map.get(&tid).copied())
        .or_else(|| reviewer_pr_map.get(coworker_name).copied())
        .or_else(|| worktree_pr_map.get(coworker_name).copied())
}

/// Get all tasks from Claude Code task storage with their status.
fn get_all_tasks(
    task_message_ids: &std::collections::HashMap<String, String>,
) -> Vec<serde_json::Value> {
    crate::tasks::read_tasks()
        .into_iter()
        .map(|task| {
            let status = match task.status {
                crate::tasks::TaskStatus::Pending => "pending",
                crate::tasks::TaskStatus::InProgress => "in_progress",
                crate::tasks::TaskStatus::Completed => "completed",
            };
            let message_id = task_message_ids.get(&task.id).cloned();
            serde_json::json!({
                "id": task.id,
                "subject": task.subject,
                "status": status,
                "assignee": task.owner,
                "message_id": message_id,
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

/// Filter the project lead session from a coworker list.
///
/// The lead session is named after the repo (e.g., "midtown") and must not
/// appear in the coworkers list returned by the status command. This function
/// is extracted for testability.
#[cfg(test)]
pub(super) fn filter_lead_session(
    coworkers: Vec<serde_json::Value>,
    repo_name: &str,
) -> Vec<serde_json::Value> {
    coworkers
        .into_iter()
        .filter(|cw| {
            cw.get("name")
                .and_then(|v| v.as_str())
                .map(|name| !name.eq_ignore_ascii_case(repo_name))
                .unwrap_or(true)
        })
        .collect()
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
