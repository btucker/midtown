//! Channel-related RPC handlers.
//!
//! Handles `channel.post` and `channel.read` methods, including IRC-style `/me`
//! actions, review note deduplication, @mention routing, and notification delivery.

use std::time::{Duration, Instant};

use tracing::{debug, error, info};

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

/// Parse a duration string like "5m", "1h", "30s" into a Duration.
///
/// Returns None if the format is invalid.
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Extract number and unit
    let unit_pos = s
        .chars()
        .position(|c| !c.is_ascii_digit())
        .unwrap_or(s.len());
    let (num_str, unit) = s.split_at(unit_pos);

    let num: u64 = num_str.parse().ok()?;

    match unit {
        "s" | "sec" | "second" | "seconds" => Some(Duration::from_secs(num)),
        "m" | "min" | "minute" | "minutes" => Some(Duration::from_secs(num * 60)),
        "h" | "hr" | "hour" | "hours" => Some(Duration::from_secs(num * 3600)),
        "d" | "day" | "days" => Some(Duration::from_secs(num * 86400)),
        _ => None,
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// Handle channel.post RPC method.
///
/// Supports IRC-style `/me` actions. If the message starts with `/me `,
/// the prefix is stripped and the message is stored as an Action type.
/// For coworkers, the action text is also reflected in the web UI status.
///
/// Also detects feedback requests from coworkers and nudges the Lead.
/// For topic channels, nudges the active channel lead session (if any)
/// instead of the main Lead. @mentions are not routed in topic channels
/// since the channel lead owns all routing in its domain.
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

    // Nudge lead when user messages arrive (from web UI or TUI input)
    if state.is_user_sender(from) {
        let default_channel = state.channel_router.default_channel_name();
        let is_topic_channel = channel_name != default_channel;

        if is_topic_channel {
            // Topic channel: nudge the channel lead for this channel (if one is active).
            // Channel leads are registered in the session manager under the channel name
            // (see task !1465 for channel lead session lifecycle management).
            // If no channel lead is active, the message is already in the channel log
            // and will be read when the lead next starts up.
            //
            // Note: @mentions are intentionally NOT routed here. In topic channels,
            // the channel lead is the single point of entry and owns all routing
            // decisions within its domain. This avoids competing routing paths.
            if state.session_manager.is_alive(channel_name).await {
                let nudge_msg = format!("user: {}", content);
                info!(
                    "Nudging channel lead '{}' about user message in #{}",
                    channel_name, channel_name
                );
                if let Err(e) = state
                    .session_manager
                    .send_message(channel_name, &nudge_msg)
                    .await
                {
                    error!("Failed to nudge channel lead '{}': {}", channel_name, e);
                }
            } else {
                info!(
                    "No active channel lead for #{} — user message not forwarded",
                    channel_name
                );
            }
        } else {
            // Main channel: check if user is @mentioning specific coworkers or @all
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
                state.nudge_lead(&nudge_msg).await;
            } else {
                info!(
                    "Skipping Lead nudge — user message routed directly to mentioned coworker(s)"
                );
            }
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
            state.nudge_lead(&nudge_msg).await;

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

/// Handle channel.list RPC method.
///
/// Returns the list of available channels, optionally including archived channels.
/// This ensures the TUI and web UI show the same channel list.
pub(super) fn handle_channel_list(
    id: RequestId,
    include_archived: bool,
    state: &DaemonState,
) -> Response {
    let base_dir = state.channel_router.base_dir();
    let channels = match crate::Channel::list(base_dir, include_archived, Some(&state.repo_name)) {
        Ok(ch) => ch,
        Err(e) => {
            error!("Failed to list channels: {}", e);
            return Response::error(id, RpcError::new(-32603, e.to_string()));
        }
    };

    Response::success(
        id,
        serde_json::json!({
            "channels": channels,
        }),
    )
}

/// Handle channel.read RPC method.
pub(super) fn handle_channel_read(
    id: RequestId,
    all: bool,
    last: Option<usize>,
    since: Option<&str>,
    state: &DaemonState,
) -> Response {
    info!(
        "channel.read called with all={}, last={:?}, since={:?}",
        all, last, since
    );

    // Read from the default (main) channel
    let default_channel = match state.channel_router.default_channel() {
        Ok(ch) => ch,
        Err(e) => {
            error!("Failed to get default channel: {}", e);
            return Response::error(id, RpcError::new(-32603, e.to_string()));
        }
    };

    let messages = if let Some(n) = last {
        // Use --last flag: read last N messages
        info!("Reading last {} messages", n);
        match default_channel.read_last_n_messages(n) {
            Ok((msgs, _)) => {
                info!(
                    "read_last_n_messages({}) returned {} messages",
                    n,
                    msgs.len()
                );
                msgs
            }
            Err(e) => {
                error!("Failed to read last {} messages: {}", n, e);
                return Response::error(id, RpcError::new(-32603, e.to_string()));
            }
        }
    } else if let Some(duration_str) = since {
        // Use --since flag: filter messages by timestamp
        let duration = match parse_duration(duration_str) {
            Some(d) => d,
            None => {
                return Response::error(
                    id,
                    RpcError::new(
                        -32602,
                        format!(
                            "Invalid duration format: '{}'. Use format like '5m', '1h', '30s'",
                            duration_str
                        ),
                    ),
                );
            }
        };

        let cutoff = chrono::Utc::now() - chrono::Duration::from_std(duration).unwrap();

        match default_channel.read_all() {
            Ok(msgs) => msgs.into_iter().filter(|m| m.timestamp >= cutoff).collect(),
            Err(e) => {
                error!("Failed to read channel: {}", e);
                return Response::error(id, RpcError::new(-32603, e.to_string()));
            }
        }
    } else if all {
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
    use std::process::Command;

    fn make_test_state(repo_name: &str) -> DaemonState {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config");
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git commit");

        let wm = crate::worktree::WorktreeManager::new(temp_dir.path().to_path_buf())
            .expect("worktree manager");
        let cm = crate::coworker::CoworkerManager::new(wm);

        // Leak temp_dir so it survives the test
        let base_dir = temp_dir.path().to_path_buf();
        std::mem::forget(temp_dir);

        let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");
        let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
        DaemonState::new(
            "/tmp/test.sock".into(),
            cm,
            repo_name.to_string(),
            vec![base_dir],
            channel_router,
            None,
            10,
            None,
            "main".to_string(),
            shutdown_tx,
        )
        .expect("daemon state")
    }

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

    #[tokio::test]
    async fn test_user_message_queues_headed_lead_nudge() {
        let state = make_test_state("midtown-test-rpc-channel-queue-user");
        let adapter_id = "test-adapter-user";
        state
            .headed_register("lead", adapter_id, crate::auth::AuthProvider::Claude)
            .await
            .expect("register headed adapter");

        let response =
            handle_channel_post(1_i64.into(), "user", "please check this", None, &state).await;
        assert!(response.error.is_none(), "channel.post should succeed");

        let (messages, _capture) = state
            .headed_poll("lead", adapter_id, 0, 10)
            .await
            .expect("poll headed queue");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].kind, "nudge_text");
        assert_eq!(messages[0].text, "user: please check this");
        assert!(messages[0].submit);
    }

    #[tokio::test]
    async fn test_coworker_at_lead_queues_headed_lead_nudge() {
        let state = make_test_state("midtown-test-rpc-channel-queue-coworker");
        let adapter_id = "test-adapter-coworker";
        state
            .headed_register("lead", adapter_id, crate::auth::AuthProvider::Claude)
            .await
            .expect("register headed adapter");

        let response =
            handle_channel_post(2_i64.into(), "york", "@lead need a review", None, &state).await;
        assert!(response.error.is_none(), "channel.post should succeed");

        let (messages, _capture) = state
            .headed_poll("lead", adapter_id, 0, 10)
            .await
            .expect("poll headed queue");
        assert_eq!(messages.len(), 1);
        assert!(
            messages[0].text.contains("york mentioned @lead"),
            "queue entry should summarize coworker @lead mention"
        );
    }

    #[test]
    fn test_parse_duration_seconds() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("5sec"), Some(Duration::from_secs(5)));
        assert_eq!(parse_duration("10second"), Some(Duration::from_secs(10)));
        assert_eq!(parse_duration("15seconds"), Some(Duration::from_secs(15)));
    }

    #[test]
    fn test_parse_duration_minutes() {
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("10min"), Some(Duration::from_secs(600)));
        assert_eq!(parse_duration("2minute"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("3minutes"), Some(Duration::from_secs(180)));
    }

    #[test]
    fn test_parse_duration_hours() {
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("2hr"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("3hour"), Some(Duration::from_secs(10800)));
        assert_eq!(parse_duration("4hours"), Some(Duration::from_secs(14400)));
    }

    #[test]
    fn test_parse_duration_days() {
        assert_eq!(parse_duration("1d"), Some(Duration::from_secs(86400)));
        assert_eq!(parse_duration("2day"), Some(Duration::from_secs(172800)));
        assert_eq!(parse_duration("3days"), Some(Duration::from_secs(259200)));
    }

    #[test]
    fn test_parse_duration_invalid() {
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("5x"), None);
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("5.5m"), None); // floats not supported
    }

    /// Verify that a user message to a topic channel with no active channel lead
    /// succeeds without error and does NOT nudge the main lead.
    #[tokio::test]
    async fn test_user_message_to_topic_channel_no_lead_no_main_nudge() {
        let state = make_test_state("midtown-test-topic-no-lead");
        let adapter_id = "test-adapter-topic-no-lead";
        state
            .headed_register("lead", adapter_id, crate::auth::AuthProvider::Claude)
            .await
            .expect("register headed adapter");

        // Post to a topic channel with no active channel lead
        let response = handle_channel_post(
            1_i64.into(),
            "user",
            "hello topic",
            Some("auth-refactor"),
            &state,
        )
        .await;
        assert!(response.error.is_none(), "channel.post should succeed");

        // Main lead should NOT be nudged for topic channel user messages
        let (messages, _capture) = state
            .headed_poll("lead", adapter_id, 0, 10)
            .await
            .expect("poll headed queue");
        assert!(
            messages.is_empty(),
            "Main lead should not be nudged when user posts to a topic channel without a channel lead"
        );
    }

    /// Verify that a user message to the main channel still nudges the main lead.
    #[tokio::test]
    async fn test_user_message_to_main_channel_nudges_lead() {
        let state = make_test_state("midtown-test-main-channel-nudge");
        let adapter_id = "test-adapter-main-nudge";
        state
            .headed_register("lead", adapter_id, crate::auth::AuthProvider::Claude)
            .await
            .expect("register headed adapter");

        // Post to main channel (None = default channel)
        let response = handle_channel_post(2_i64.into(), "user", "hello main", None, &state).await;
        assert!(response.error.is_none(), "channel.post should succeed");

        let (messages, _capture) = state
            .headed_poll("lead", adapter_id, 0, 10)
            .await
            .expect("poll headed queue");
        assert_eq!(
            messages.len(),
            1,
            "Main lead should be nudged for main channel user messages"
        );
        assert_eq!(messages[0].text, "user: hello main");
    }

    #[tokio::test]
    async fn test_channel_read_with_last_parameter() {
        let state = make_test_state("midtown-test-channel-read-last");

        // Post 10 messages to the channel
        for i in 1..=10 {
            let msg = format!("Test message {}", i);
            let _response = handle_channel_post(i.into(), "test", &msg, None, &state).await;
        }

        // Request last 3 messages
        let response = handle_channel_read(999.into(), false, Some(3), None, &state);

        // Verify we got exactly 3 messages
        assert!(response.error.is_none(), "channel.read should succeed");
        let result = response.result.unwrap();
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(
            messages.len(),
            3,
            "Expected 3 messages, got {}",
            messages.len()
        );

        // Verify they are the last 3 messages
        assert!(
            messages[0]["message"]
                .as_str()
                .unwrap()
                .contains("message 8")
        );
        assert!(
            messages[1]["message"]
                .as_str()
                .unwrap()
                .contains("message 9")
        );
        assert!(
            messages[2]["message"]
                .as_str()
                .unwrap()
                .contains("message 10")
        );
    }
}
