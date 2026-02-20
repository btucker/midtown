//! Channel-related RPC handlers.
//!
//! Handles `channel.post`, `channel.read`, `channel.create`, `channel.archive`,
//! and `channel.list` methods, including IRC-style `/me` actions, review note
//! deduplication, @mention routing, and notification delivery.

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
    thread_parent_id: Option<&str>,
    state: &DaemonState,
) -> Response {
    // Clean up shell escaping artifacts (e.g. "\!" from bash history expansion escaping)
    // and trim leading/trailing whitespace so channel messages don't start with blank lines.
    let message = unescape_shell_artifacts(message.trim());

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
    let msg = if let Some(parent_id) = thread_parent_id {
        Message::thread_reply(
            channel_name,
            from,
            content.clone(),
            parent_id,
            msg_type.clone(),
        )
    } else {
        Message::for_channel(channel_name, from, content.clone(), msg_type.clone())
    };

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
            // Topic channel: nudge the channel lead for this channel.
            // If the session isn't running, resume it first.
            //
            // Note: @mentions are intentionally NOT routed here. In topic channels,
            // the channel lead is the single point of entry and owns all routing
            // decisions within its domain. This avoids competing routing paths.
            let session_name = crate::launch::channel_lead_session_name(channel_name);
            let session_ready = if state.session_manager.is_alive(&session_name).await {
                true
            } else {
                // Session isn't running — resume it so the user message is handled.
                let (session_id, headless_session_id_cleared) = {
                    let ps = state.persistent_state.lock().await;
                    let sid = ps.channel_lead_sessions.get(channel_name).cloned();
                    // Cross-check headless_sessions: if the death handler cleared the
                    // session_id after a failed resume, don't attempt to resume with a
                    // stale ID (same guard as startup.rs recovery).
                    let cleared = ps
                        .headless_sessions
                        .get(channel_name)
                        .is_some_and(|info| info.session_id.is_empty());
                    (sid, cleared)
                };

                let session_mode = match session_id {
                    Some(ref id) if !id.is_empty() && !headless_session_id_cleared => {
                        info!(
                            "Resuming channel lead session for '{}' (session {}) after user message",
                            channel_name, id
                        );
                        crate::launch::SessionMode::ResumeSession(id.clone())
                    }
                    _ => {
                        if headless_session_id_cleared {
                            info!(
                                "Skipping stale session ID for channel lead '{}': headless_sessions entry was cleared after failed resume",
                                channel_name
                            );
                        } else {
                            info!(
                                "No saved session for channel lead '{}', spawning fresh after user message",
                                channel_name
                            );
                        }
                        crate::launch::SessionMode::Fresh
                    }
                };

                // Register placeholder in channel_lead_sessions before spawning
                // so NudgeChannelLead effects aren't silently dropped and daemon
                // restart recovery can find this channel's session.
                let is_fresh = matches!(session_mode, crate::launch::SessionMode::Fresh);
                if is_fresh {
                    let mut ps = state.persistent_state.lock().await;
                    ps.channel_lead_sessions
                        .entry(channel_name.to_string())
                        .or_insert_with(String::new);
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
                        error!(
                            "Failed to save daemon state before spawning channel lead: {}",
                            e
                        );
                    }
                }

                let config = crate::launch::LaunchConfig::channel_lead(
                    channel_name,
                    &state.repo_name,
                    session_mode,
                    "", // domain_context: accumulates via session persistence
                );

                match state.spawn_coworker(&config).await {
                    Ok(()) => true,
                    Err(e) => {
                        error!("Failed to spawn channel lead '{}': {}", channel_name, e);
                        // Keep the placeholder in channel_lead_sessions even on spawn failure.
                        // An empty-string placeholder is harmless (startup recovery handles it
                        // with SessionMode::Fresh), and preserving it means the channel is
                        // registered for restart recovery even if this spawn attempt failed.
                        false
                    }
                }
            };

            if session_ready {
                let nudge_msg = format!("user ({}): {}", msg.id, content);
                info!(
                    "Nudging channel lead '{}' about user message in #{}",
                    channel_name, channel_name
                );
                if let Err(e) = state
                    .session_manager
                    .send_message(&session_name, &nudge_msg)
                    .await
                {
                    error!("Failed to nudge channel lead '{}': {}", channel_name, e);
                }
            }
        } else {
            // Main channel: always nudge the lead on user messages.
            // Also route any @mentions directly to the mentioned coworkers.
            // The lead stays informed of all user messages regardless of @mentions,
            // so it can provide context, coordinate, or respond if needed.

            // Route @mentions in user messages directly to coworkers
            super::chat::route_mentions(state, &msg).await;

            // When the lead is dead, expedite its respawn and wake the ops channel lead
            // so the user isn't left in silence. We check both headless (session_manager)
            // and interactive (attached_coworkers) paths — if either is live, the lead
            // is reachable and we skip the expedite.
            let lead_is_dead = !state.session_manager.is_alive("lead").await
                && !state
                    .attached_coworkers
                    .lock()
                    .unwrap()
                    .contains_key("lead");
            if lead_is_dead {
                let should_expedite = {
                    let cooldowns = state.cooldowns.lock().unwrap();
                    cooldowns.check("lead_dead_expedite", "lead", Duration::from_secs(30))
                };
                if should_expedite {
                    {
                        let mut cooldowns = state.cooldowns.lock().unwrap();
                        cooldowns.record("lead_dead_expedite", "lead");
                    }
                    info!("Lead is dead — expediting respawn on user message");
                    state.expedite_lead_respawn_on_user_message().await;
                }
            }

            let nudge_msg = format!("user ({}): {}", msg.id, content);
            info!("Nudging Lead about user message");
            state.nudge_lead(&nudge_msg).await;
        }
    }

    // Nudge the Lead when a coworker explicitly mentions @lead or @{project_name}
    let content_lower = content.to_lowercase();
    let project_mention = format!("@{}", state.repo_name).to_lowercase();
    if is_coworker_sender(from)
        && (content_lower.contains("@lead") || content_lower.contains(&project_mention))
    {
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

            let nudge_msg = format!(
                "{} mentioned @{} ({}): {}",
                from, state.repo_name, msg.id, summary
            );
            info!(
                "Nudging Lead about @{} mention from {}",
                state.repo_name, from
            );
            state.nudge_lead(&nudge_msg).await;

            // Send push notification to mobile PWA
            state.send_push_notification(
                &format!("@{} from {}", state.repo_name, from),
                &summary,
                "mention",
            );
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

/// Handle channel.create RPC method.
///
/// Creates a new channel directory and history file.
/// If the channel already exists, this is a no-op (idempotent).
pub(super) fn handle_channel_create(id: RequestId, name: &str, state: &DaemonState) -> Response {
    let base_dir = state.channel_router.base_dir();
    match crate::Channel::create(base_dir, name) {
        Ok(_) => Response::success(
            id,
            serde_json::json!({
                "success": true,
                "message": format!("Channel '{}' created", name),
            }),
        ),
        Err(e) => {
            error!("Failed to create channel '{}': {}", name, e);
            Response::error(id, RpcError::new(-32603, e.to_string()))
        }
    }
}

/// Handle channel.archive RPC method.
///
/// Archives a channel by renaming its directory from `<name>/` to `<name>.archived/`.
/// Also cleans up any running channel lead session for the archived channel by
/// removing it from `channel_lead_sessions` and `headless_sessions` in persistent state.
/// Returns an error if the channel does not exist or if trying to archive 'midtown'.
pub(super) async fn handle_channel_archive(
    id: RequestId,
    name: &str,
    state: &DaemonState,
) -> Response {
    let base_dir = state.channel_router.base_dir();

    // Check existence before calling Channel::new(), which would create the
    // directory if it doesn't exist, silently archiving an empty ghost channel.
    let channel_dir = base_dir.join("channels").join(name);
    if !channel_dir.exists() {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Channel '{}' does not exist", name)),
        );
    }

    let channel = match crate::Channel::new(base_dir, name) {
        Ok(ch) => ch,
        Err(e) => {
            error!("Failed to open channel '{}' for archiving: {}", name, e);
            return Response::error(id, RpcError::new(-32603, e.to_string()));
        }
    };
    match channel.archive() {
        Ok(()) => {
            // Shut down the channel lead session (if running) and clean up state.
            // This mirrors the cleanup in Effect::ArchiveChannel (effects.rs).
            let lead_session_name = crate::launch::channel_lead_session_name(name);
            let goodbye = format!(
                "Channel '{}' has been archived. Your session is ending.",
                name
            );
            super::effects::execute_effects(
                vec![super::effects::Effect::ShutdownCoworker {
                    name: lead_session_name.clone(),
                    message: goodbye,
                }],
                state,
            )
            .await;

            // Remove from channel_lead_sessions and headless_sessions
            {
                let mut ps = state.persistent_state.lock().await;
                let removed_lead = ps.channel_lead_sessions.remove(name).is_some();
                let removed_headless = ps.headless_sessions.remove(&lead_session_name).is_some();
                if removed_lead || removed_headless {
                    debug!(
                        "Removed channel lead session for archived channel '{}'",
                        name
                    );
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
                        warn!(
                            "Failed to save daemon state after removing channel lead: {}",
                            e
                        );
                    }
                }
            }

            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Channel '{}' archived", name),
                }),
            )
        }
        Err(e) => {
            error!("Failed to archive channel '{}': {}", name, e);
            Response::error(id, RpcError::new(-32603, e.to_string()))
        }
    }
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
///
/// Accepts an optional `channel` parameter to read from a topic channel.
/// If not provided, defaults to the main channel. Respects `MIDTOWN_CHANNEL`
/// when called via the CLI client.
pub(super) fn handle_channel_read(
    id: RequestId,
    all: bool,
    last: Option<usize>,
    since: Option<&str>,
    channel: Option<&str>,
    state: &DaemonState,
) -> Response {
    info!(
        "channel.read called with all={}, last={:?}, since={:?}, channel={:?}",
        all, last, since, channel
    );

    // Read from the specified channel, or fall back to the default (main) channel
    let target_channel = match channel {
        Some(name) => match state.channel_router.get_channel(name) {
            Ok(ch) => ch,
            Err(e) => {
                error!("Failed to get channel '{}': {}", name, e);
                return Response::error(id, RpcError::new(-32603, e.to_string()));
            }
        },
        None => match state.channel_router.default_channel() {
            Ok(ch) => ch,
            Err(e) => {
                error!("Failed to get default channel: {}", e);
                return Response::error(id, RpcError::new(-32603, e.to_string()));
            }
        },
    };

    let messages = if let Some(n) = last {
        // Use --last flag: read last N messages
        info!("Reading last {} messages", n);
        match target_channel.read_last_n_messages(n) {
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

        match target_channel.read_all() {
            Ok(msgs) => msgs.into_iter().filter(|m| m.timestamp >= cutoff).collect(),
            Err(e) => {
                error!("Failed to read channel: {}", e);
                return Response::error(id, RpcError::new(-32603, e.to_string()));
            }
        }
    } else if all {
        // Read all messages
        match target_channel.read_all() {
            Ok(msgs) => msgs,
            Err(e) => {
                error!("Failed to read channel: {}", e);
                return Response::error(id, RpcError::new(-32603, e.to_string()));
            }
        }
    } else {
        // Read recent messages (last 20)
        match target_channel.read_all() {
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
            let mut obj = serde_json::json!({
                "from": m.from,
                "message": m.content,
                "timestamp": m.timestamp.to_rfc3339(),
            });
            if let Some(ref parent_id) = m.thread_parent_id {
                obj["thread_parent_id"] = serde_json::Value::String(parent_id.clone());
            }
            obj
        })
        .collect();

    Response::success(
        id,
        serde_json::json!({
            "messages": messages_json,
        }),
    )
}

#[path = "rpc_channel_tests.rs"]
#[cfg(test)]
mod tests;
