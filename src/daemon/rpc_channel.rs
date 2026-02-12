//! Channel-related RPC handlers.
//!
//! Handles `channel.post` and `channel.read` methods, including IRC-style `/me`
//! actions, review note deduplication, @mention routing, and notification delivery.

use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};

use crate::message::{Message, MessageType};
use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;
use super::helpers::*;

// ============================================================================
// Helper functions
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
    // Match "[Review Note]" followed by "PR #" and a number
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

// ============================================================================
// Handlers
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

    // Update tmux tab for coworkers when they post /me actions.
    // Prefer structured state from daemon memory (reported via RPC) over
    // parsing the freeform /me message text with keyword matching.
    //
    // Run tmux operations in spawn_blocking to avoid blocking the async
    // runtime. This prevents RPC timeouts when tmux commands are slow.
    if msg_type == MessageType::Action {
        let display_status = {
            let records = state.coworker_records.read().await;
            records.get(from).and_then(|record| record.display_status())
        };

        let coworkers = state.coworkers.clone();
        let from_clone = from.to_string();
        let content_clone = content.clone();

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

    // Nudge lead when user messages arrive (from web UI or TUI input)
    if state.is_user_sender(from) {
        // Check if user is @mentioning specific coworkers or @all
        let has_coworker_mentions =
            !extract_mentions(&content).is_empty() || contains_at_all(&content);
        let has_lead_mention = content.to_lowercase().contains("@lead");

        // Route @mentions in user messages directly to coworkers
        super::chat::route_mentions(state, &msg).await;

        // Only nudge lead if there are no coworker @mentions (regular
        // message for the lead) or if the user also @mentioned the lead.
        // This lets users talk directly to coworkers without the lead
        // acting as a middleman.
        if !has_coworker_mentions || has_lead_mention {
            let nudge_msg = format!("user: {}", content);
            info!("Nudging Lead about user message");
            // Run in spawn_blocking to avoid blocking the async runtime
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

    // Nudge the Lead when a coworker explicitly mentions @lead
    let content_lower = content.to_lowercase();
    if is_coworker_sender(from) && content_lower.contains("@lead") {
        // Use CooldownTracker to avoid duplicate nudges (expires after 1 hour)
        let should_nudge = {
            let cooldowns = state.cooldowns.lock().unwrap();
            cooldowns.check("lead_mention", &msg.id, Duration::from_secs(3600))
        };

        if should_nudge {
            // Record that we're nudging for this message
            {
                let mut cooldowns = state.cooldowns.lock().unwrap();
                cooldowns.record("lead_mention", &msg.id);
            }

            // Truncate message for nudge (max 100 chars)
            let summary = truncate_str(&content, 100);

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
    }

    // Send bell notification and push notification for @user mentions
    // Also recognize @<display_name> if configured (e.g., @Ben)
    let has_user_mention = content_lower.contains("@user")
        || state
            .user_display_name
            .as_ref()
            .is_some_and(|dn| content_lower.contains(&format!("@{}", dn.to_lowercase())));
    if has_user_mention && !state.is_user_sender(from) {
        info!("Bell notification: @user mentioned by {}", from);
        // Run in spawn_blocking to avoid blocking the async runtime
        let coworkers = state.coworkers.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = coworkers.notify_user() {
                warn!("Failed to send bell notification for @user mention: {}", e);
            }
        });
        let display = state.user_display_name.as_deref().unwrap_or("user");
        let summary = truncate_str(&content, 100);
        state.send_push_notification(&format!("@{} from {}", display, from), &summary, "mention");
    }

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
        // Read all messages
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
        let msg = "@lead [Review Note] PR #708: The new is_ui_chrome() pattern for ctrl+ key hints is heuristic. Please determine if this warrants a follow-up task.";
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
