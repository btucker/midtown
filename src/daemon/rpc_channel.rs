//! Channel, status, and reminder RPC handlers.
//!
//! Handles `channel.post`, `channel.read`, `status`, and `reminder.*` methods.
//! Also contains the `handle_channel_post` function which routes @mentions,
//! deduplicates review notes, and sends notifications.

use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};

use crate::message::{Message, MessageType};
use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;
use super::constants::*;
use super::helpers::*;

// ============================================================================
// Channel RPC handlers
// ============================================================================

/// Handle channel.post RPC method.
///
/// Supports IRC-style `/me` actions. If the message starts with `/me `,
/// the prefix is stripped and the message is stored as an Action type.
/// For coworkers, the action text is also reflected in their tmux tab name.
///
/// Also detects feedback requests from coworkers and nudges the Lead.
///
/// Accepts an optional `channel` parameter to post to topic channels.
/// If not provided, defaults to the main channel.
pub(super) async fn handle_channel_post(
    id: RequestId,
    from: &str,
    message: &str,
    channel: Option<&str>,
    state: &DaemonState,
) -> Response {
    // Clean up shell escaping artifacts (e.g. "\!" from bash history expansion escaping)
    let message = unescape_shell_artifacts(message);

    // Check for /me prefix (IRC-style action)
    let (content, msg_type) = if let Some(action) = message.strip_prefix("/me ") {
        (action.to_string(), MessageType::Action)
    } else {
        (message.to_string(), MessageType::Text)
    };

    // Deduplicate [Review Note] messages: suppress rapid-fire notes from the same
    // reviewer for the same PR (within 60s cooldown). Notes after the cooldown
    // (e.g., corrections or follow-ups) are allowed through.
    if let Some(pr_num) = extract_review_note_pr(&content) {
        let key = (from.to_lowercase(), pr_num);
        let now = std::time::Instant::now();
        let cooldown = std::time::Duration::from_secs(60);
        let mut tracker = state.review_note_tracker.lock().unwrap();
        if tracker
            .get(&key)
            .is_some_and(|first_seen| now.duration_since(*first_seen) < cooldown)
        {
            debug!(
                "channel.post: suppressing duplicate [Review Note] from {} for PR #{} (within {}s cooldown)",
                from,
                pr_num,
                cooldown.as_secs()
            );
            return Response::success(
                id,
                serde_json::json!({
                    "posted": false,
                    "reason": "duplicate_review_note",
                }),
            );
        }
        // Record or refresh the timestamp
        tracker.insert(key, now);
    }

    // Use provided channel or default to main channel
    let channel_name = channel.unwrap_or_else(|| state.channel_router.default_channel_name());
    let msg = Message::for_channel(channel_name, from, content.clone(), msg_type.clone());

    // Use async version to avoid blocking the runtime during file lock acquisition
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        error!("Failed to write to channel: {}", e);
        return Response::error(id, RpcError::new(-32603, e.to_string()));
    }

    info!("Channel post from {}: {}", from, message);

    // Track last activity time for coworker (used for silent coworker detection)
    if is_coworker_sender(from) {
        let mut records = state.coworker_records.write().await;
        records
            .entry(from.to_string())
            .or_insert_with(crate::rules::CoworkerRecord::new_spawn)
            .last_activity = Some(Instant::now());
        drop(records); // Release write lock before acquiring read lock
    }

    // Update tmux tab for coworkers when they post /me actions
    if msg_type == MessageType::Action {
        update_coworker_tab_on_action(state, from, &content).await;
    }

    // Route user messages and @mentions
    route_user_messages(state, from, &content, &msg).await;

    // Handle @lead mentions from coworkers
    handle_lead_mentions(state, from, &content, &msg);

    // Handle @user mentions
    handle_user_mentions(state, from, &content);

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": "Message posted to channel",
        }),
    )
}

/// Handle channel.read RPC method.
pub(super) fn handle_channel_read(id: RequestId, all: bool, state: &DaemonState) -> Response {
    // Read from the default (main) channel
    let default_channel = match state.channel_router.default_channel() {
        Ok(ch) => ch,
        Err(e) => {
            error!("Failed to get default channel: {}", e);
            return Response::error(id, RpcError::new(-32603, e.to_string()));
        }
    };

    let messages = if all {
        match default_channel.read_all() {
            Ok(msgs) => msgs,
            Err(e) => {
                error!("Failed to read channel: {}", e);
                return Response::error(id, RpcError::new(-32603, e.to_string()));
            }
        }
    } else {
        // Read recent messages (last 20)
        match default_channel.read_all() {
            Ok(msgs) => {
                let total = msgs.len();
                if total > 20 {
                    msgs.into_iter().skip(total - 20).collect()
                } else {
                    msgs
                }
            }
            Err(e) => {
                error!("Failed to read channel: {}", e);
                return Response::error(id, RpcError::new(-32603, e.to_string()));
            }
        }
    };

    let messages_json: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "from": m.from,
                "message": m.content,
                "timestamp": m.timestamp.to_rfc3339(),
            })
        })
        .collect();

    Response::success(
        id,
        serde_json::json!({
            "messages": messages_json,
        }),
    )
}

// ============================================================================
// Status handler
// ============================================================================

/// Handle status RPC method.
///
/// This handler runs blocking operations (file I/O) in spawn_blocking
/// to avoid blocking the async runtime and causing RPC timeouts.
pub(super) async fn handle_status(id: RequestId, state: &DaemonState) -> Response {
    // Build a map of coworker name -> task subject from in_progress tasks
    let coworker_tasks: std::collections::HashMap<String, String> =
        crate::tasks::get_in_progress_tasks_with_subjects()
            .into_iter()
            .filter_map(|(_task_id, subject, owner)| {
                if owner.is_empty() {
                    None
                } else {
                    Some((owner.to_lowercase(), subject))
                }
            })
            .collect();

    // Get coworkers with their details, looking up current task from task storage
    let coworkers: Vec<serde_json::Value> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| {
            let current_task = coworker_tasks.get(&cw.name.to_lowercase()).cloned();
            serde_json::json!({
                "name": cw.name,
                "status": cw.status.to_string(),
                "current_task": current_task,
                "started_at": cw.started_at.to_rfc3339(),
            })
        })
        .collect();

    // Get cached PR data from the daemon's periodic polling
    let (pull_requests, merged_prs) = {
        let cache = state.pr_coworker_cache.read().unwrap();
        if cache.pr_poll_initialized {
            (cache.open_prs_data.clone(), cache.merged_prs_data.clone())
        } else {
            (Vec::new(), Vec::new())
        }
    };

    // Run blocking file I/O operations in spawn_blocking
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
// Internal helpers
// ============================================================================

/// Remove shell escaping artifacts from channel messages.
///
/// When Claude Code posts messages via its Bash tool, the LLM often escapes `!`
/// as `\!` (to avoid bash history expansion). Since the Bash tool runs in
/// non-interactive mode where history expansion is disabled, the backslash passes
/// through literally. This function cleans up such artifacts.
fn unescape_shell_artifacts(s: &str) -> String {
    s.replace("\\!", "!")
}

/// Extract PR number from a `[Review Note] PR #123: ...` message.
///
/// Returns `Some(pr_number)` if the message contains the review note pattern,
/// `None` otherwise. Used for per-reviewer per-PR deduplication.
fn extract_review_note_pr(message: &str) -> Option<u64> {
    let review_note_idx = message.find("[Review Note]")?;
    let after = &message[review_note_idx..];
    let pr_hash_idx = after.find("PR #").or_else(|| after.find("pr #"))?;
    let after_hash = &after[pr_hash_idx + 4..];
    let num_str: String = after_hash
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    num_str.parse().ok()
}

/// Update tmux tab for coworkers when they post /me actions.
///
/// Prefers structured state from daemon memory (reported via RPC) over
/// parsing the freeform /me message text with keyword matching.
async fn update_coworker_tab_on_action(state: &DaemonState, from: &str, content: &str) {
    let display_status = {
        let records = state.coworker_records.read().await;
        records.get(from).and_then(|record| record.display_status())
    };

    let coworkers = state.coworkers.clone();
    let from_clone = from.to_string();
    let content_clone = content.to_string();

    tokio::task::spawn_blocking(move || {
        if let Some(display) = display_status {
            if let Err(e) = coworkers.update_status_formatted(&from_clone, &display) {
                debug!("Failed to update tmux tab for {}: {}", from_clone, e);
            }
        } else {
            // Fallback: parse /me message text with keyword matching
            if let Err(e) = coworkers.update_status_display(&from_clone, Some(&content_clone)) {
                debug!("Failed to update tmux tab for {}: {}", from_clone, e);
            }
        }
    });
}

/// Route user messages: nudge lead and/or route @mentions to coworkers.
async fn route_user_messages(state: &DaemonState, from: &str, content: &str, msg: &Message) {
    if !state.is_user_sender(from) {
        return;
    }

    // Check if user is @mentioning specific coworkers or @all
    let has_coworker_mentions = !extract_mentions(content).is_empty() || contains_at_all(content);
    let has_lead_mention = content.to_lowercase().contains("@lead");

    // Route @mentions in user messages directly to coworkers
    super::chat::route_mentions(state, msg).await;

    // Only nudge lead if there are no coworker @mentions (regular
    // message for the lead) or if the user also @mentioned the lead.
    if !has_coworker_mentions || has_lead_mention {
        let nudge_msg = format!("user: {}", content);
        info!("Nudging Lead about user message");
        let coworkers = state.coworkers.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = coworkers.nudge_lead(&nudge_msg) {
                warn!("Failed to nudge Lead about user message: {}", e);
            }
        });
    } else {
        info!("Skipping Lead nudge — user message routed directly to mentioned coworker(s)");
    }
}

/// Handle @lead mentions from coworkers — nudge the lead and send push notification.
fn handle_lead_mentions(state: &DaemonState, from: &str, content: &str, msg: &Message) {
    let content_lower = content.to_lowercase();
    if !is_coworker_sender(from) || !content_lower.contains("@lead") {
        return;
    }

    // Use CooldownTracker to avoid duplicate nudges (expires after 1 hour)
    let should_nudge = {
        let cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.check("lead_mention", &msg.id, Duration::from_secs(3600))
    };

    if !should_nudge {
        return;
    }

    // Record that we're nudging for this message
    {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.record("lead_mention", &msg.id);
    }

    // Truncate message for nudge (max 100 chars)
    let summary = truncate_str(content, 100);
    let nudge_msg = format!("{} mentioned @lead: {}", from, summary);
    info!("Nudging Lead about @lead mention from {}", from);

    // Nudge the Lead window (spawn_blocking to avoid blocking async runtime)
    let coworkers = state.coworkers.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = coworkers.nudge_lead(&nudge_msg) {
            warn!("Failed to nudge Lead about @lead mention: {}", e);
        }
    });

    // Send push notification to mobile PWA
    state.send_push_notification(&format!("@lead from {}", from), &summary, "mention");
}

/// Handle @user mentions — send bell notification and push notification.
fn handle_user_mentions(state: &DaemonState, from: &str, content: &str) {
    let content_lower = content.to_lowercase();
    let has_user_mention = content_lower.contains("@user")
        || state
            .user_display_name
            .as_ref()
            .is_some_and(|dn| content_lower.contains(&format!("@{}", dn.to_lowercase())));

    if !has_user_mention || state.is_user_sender(from) {
        return;
    }

    info!("Bell notification: @user mentioned by {}", from);
    let coworkers = state.coworkers.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = coworkers.notify_user() {
            warn!("Failed to send bell notification for @user mention: {}", e);
        }
    });

    let display = state.user_display_name.as_deref().unwrap_or("user");
    let summary = truncate_str(content, 100);
    state.send_push_notification(&format!("@{} from {}", display, from), &summary, "mention");
}

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
    let channel_file = crate::paths::channel_file_for_repo("default");

    if !channel_file.exists() {
        return Vec::new();
    }

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
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unescape_shell_artifacts_exclamation() {
        assert_eq!(
            unescape_shell_artifacts("Game time\\! Let's go"),
            "Game time! Let's go"
        );
    }

    #[test]
    fn test_unescape_shell_artifacts_multiple_exclamations() {
        assert_eq!(
            unescape_shell_artifacts("Wow\\! Amazing\\! Done\\!"),
            "Wow! Amazing! Done!"
        );
    }

    #[test]
    fn test_unescape_shell_artifacts_no_escapes() {
        assert_eq!(
            unescape_shell_artifacts("Normal message with ! marks"),
            "Normal message with ! marks"
        );
    }

    #[test]
    fn test_unescape_shell_artifacts_preserves_other_backslashes() {
        assert_eq!(
            unescape_shell_artifacts("path\\to\\file and \\!"),
            "path\\to\\file and !"
        );
    }

    #[test]
    fn test_extract_review_note_pr_standard_format() {
        let msg = "@lead [Review Note] PR #708: The new is_ui_chrome() pattern.";
        assert_eq!(extract_review_note_pr(msg), Some(708));
    }

    #[test]
    fn test_extract_review_note_pr_no_match() {
        assert_eq!(extract_review_note_pr("@lead some regular message"), None);
        assert_eq!(extract_review_note_pr("fixed PR #42"), None);
        assert_eq!(extract_review_note_pr("[Review Note] no PR ref"), None);
    }

    #[test]
    fn test_extract_review_note_pr_various_numbers() {
        assert_eq!(
            extract_review_note_pr("@lead [Review Note] PR #1: minor issue"),
            Some(1)
        );
        assert_eq!(
            extract_review_note_pr("@lead [Review Note] PR #9999: edge case"),
            Some(9999)
        );
    }
}
