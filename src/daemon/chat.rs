//! Chat monitor — @mention routing and @all broadcast.
//!
//! Runs a background `tailf` loop watching `channel.jsonl` for new messages.
//! When a message with @mentions is detected, spawns or nudges the mentioned
//! coworkers. Also handles @all broadcasts.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::message::Message;

use super::DaemonState;
use super::constants::SKIP_SENDERS;
use super::helpers::{contains_at_all, extract_mentions};

// Chat Monitor - @mention routing
// ============================================================================

/// Background task that monitors the channel for @mentions and routes them.
///
/// Uses `tailf` to watch `channel.jsonl` for new messages in real-time.
/// When a message with @mentions is detected, spawns/nudges the mentioned coworkers.
pub(super) async fn chat_monitor_loop(
    state: Arc<DaemonState>,
    channel_path: PathBuf,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    // Start tailing from the end of the file (0 = no initial lines)
    let mut tailer = match tailf::tailf(&channel_path, Some(0)) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to start tailf on channel file: {}", e);
            return;
        }
    };

    info!("Chat monitor watching: {}", channel_path.display());

    loop {
        tokio::select! {
            // New line from tailf
            Some(result) = async { Some(tailer.next().await) } => {
                match result {
                    Ok(Some(bytes)) => {
                        // Convert bytes to string
                        let line = match String::from_utf8(bytes) {
                            Ok(s) => s,
                            Err(e) => {
                                debug!("Invalid UTF-8 in channel line: {}", e);
                                continue;
                            }
                        };
                        // Parse the line as a Message
                        match serde_json::from_str::<Message>(&line) {
                            Ok(msg) => {
                                // Skip messages from protected senders (loop protection),
                                // but first check for @lead mentions that need nudging.
                                if SKIP_SENDERS.iter().any(|&s| s.eq_ignore_ascii_case(&msg.from))
                                    || state.is_user_sender(&msg.from)
                                {
                                    // System/daemon messages may contain @lead that still
                                    // needs to trigger a nudge (e.g., orphaned worktree
                                    // warnings). Route @lead before skipping.
                                    // Exclude user messages — already handled in
                                    // handle_channel_post to avoid double-nudging.
                                    if !state.is_user_sender(&msg.from)
                                        && msg.content.to_lowercase().contains("@lead")
                                    {
                                        let nudge_text = format!("{}: {}", msg.from, msg.content);
                                        if let Err(e) = state.coworkers.nudge_lead(&nudge_text) {
                                            warn!("Failed to nudge lead for @lead in {} message: {}", msg.from, e);
                                        } else {
                                            info!("Nudged lead about @lead mention in {} message", msg.from);
                                        }
                                        state.send_push_notification(
                                            &format!("@lead from {}", msg.from),
                                            &msg.content,
                                            "mention",
                                        );
                                    }
                                    continue;
                                }
                                // Route any @mentions in the message
                                route_mentions(&state, &msg).await;
                            }
                            Err(e) => {
                                debug!("Failed to parse channel message: {} (line: {})", e, line);
                            }
                        }
                    }
                    Ok(None) => {
                        // No new content, continue waiting
                    }
                    Err(e) => {
                        warn!("tailf error: {}", e);
                    }
                }
            }

            // Shutdown signal
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("Chat monitor task received shutdown signal");
                    break;
                }
            }
        }
    }
}

/// Extract @mentions from message content and route to coworkers.
///
/// For each valid coworker name mentioned:
/// - If the coworker is not running, spawn them with --resume
/// - Nudge them with the message context
///
/// Also supports @all to broadcast to every active coworker and the lead.
pub(super) async fn route_mentions(state: &DaemonState, msg: &Message) {
    // Check for @all broadcast first
    if contains_at_all(&msg.content) {
        route_at_all(state, msg);
        return;
    }

    let mentions = extract_mentions(&msg.content);

    if mentions.is_empty() {
        return;
    }

    debug!(
        "Found {} @mention(s) in message from {}: {:?}",
        mentions.len(),
        msg.from,
        mentions
    );

    for name in mentions {
        let is_running = state.coworkers.get(&name).is_some();
        let nudge_text = format!("{} said: {}", msg.from, msg.content);

        // Decide action using pure decision function
        let action = crate::rules::decide_mention_action(
            &name,
            &msg.from,
            is_running,
            state.is_at_dev_limit(),
            &nudge_text,
        );

        match action {
            crate::rules::MentionAction::Nudge {
                name: ref n,
                message: ref m,
            } => {
                if let Err(e) = state.coworkers.nudge(n, m) {
                    warn!("Failed to nudge {} about @mention: {}", n, e);
                } else {
                    info!("Nudged {} about @mention from {}", n, msg.from);
                }
            }
            crate::rules::MentionAction::Spawn {
                name: ref n,
                message: ref m,
            } => {
                info!("Spawning mentioned coworker {} (not currently running)", n);
                let config = crate::tmux::ClaudeLaunchConfig::coworker(
                    n.clone(),
                    state.repo_name.clone(),
                    crate::tmux::SessionMode::Resume,
                    Some(m.clone()),
                );
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Spawned coworker {} via @mention", n);
                        let spawn_msg = Message::text(
                            "midtown",
                            format!("🚀 Called in {} in response to @mention", n),
                        );
                        if let Err(e) = state.send_and_broadcast(&spawn_msg) {
                            warn!("Failed to post call-in message: {}", e);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to spawn coworker {}: {}", n, e);
                        let err_msg = Message::text(
                            "midtown",
                            format!("⚠️ Failed to call in {} for @mention: {}", n, e),
                        );
                        let _ = state.send_and_broadcast(&err_msg);
                    }
                }
            }
            crate::rules::MentionAction::Skip { ref reason } => {
                debug!("{}", reason);
                if reason.contains("dev limit") {
                    let err_msg = Message::text(
                        "midtown",
                        format!(
                            "⚠️ Cannot call in {} for @mention: dev coworkers limit reached",
                            name
                        ),
                    );
                    let _ = state.send_and_broadcast(&err_msg);
                }
            }
        }
    }
}

/// Route an @all broadcast: nudge every active coworker and the lead, except the sender.
fn route_at_all(state: &DaemonState, msg: &Message) {
    let active_coworkers = state.coworkers.list();
    let nudge_text = format!("{} said: {}", msg.from, msg.content);

    info!(
        "@all broadcast from {} to {} active coworker(s) + lead",
        msg.from,
        active_coworkers.len()
    );

    // Nudge the lead (unless the lead sent the message)
    if !msg.from.eq_ignore_ascii_case("lead") {
        if let Err(e) = state.coworkers.nudge_lead(&nudge_text) {
            warn!("Failed to nudge lead for @all: {}", e);
        } else {
            info!("Nudged lead for @all from {}", msg.from);
        }
    }

    // Nudge all active coworkers (except the sender)
    for coworker in &active_coworkers {
        if coworker.name.eq_ignore_ascii_case(&msg.from) {
            continue;
        }

        if let Err(e) = state.coworkers.nudge(&coworker.name, &nudge_text) {
            warn!("Failed to nudge {} for @all: {}", coworker.name, e);
        } else {
            info!("Nudged {} for @all from {}", coworker.name, msg.from);
        }
    }
}
