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
use super::constants::{OPS_CHANNEL, SKIP_SENDERS};
use super::helpers::{contains_at_all, extract_mentions, extract_task_id};

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
                                    // System/daemon messages may contain @lead or @ops that still
                                    // need to trigger a nudge (e.g., stuck PR warnings).
                                    // Route before skipping. Exclude user messages — already
                                    // handled in handle_channel_post to avoid double-nudging.
                                    if !state.is_user_sender(&msg.from) {
                                        let msg_lower = msg.content.to_lowercase();
                                        let lead_mention = format!("@{}", state.repo_name).to_lowercase();
                                        if msg_lower.contains("@lead") || msg_lower.contains(&lead_mention) {
                                            let nudge_text =
                                                format!("{} ({}): {}", msg.from, msg.id, msg.content);
                                            state.nudge_lead(&nudge_text).await;
                                            info!(
                                                "Nudged lead about @{} mention in {} message",
                                                state.repo_name,
                                                msg.from
                                            );
                                            state.send_push_notification(
                                                &format!("@{} from {}", state.repo_name, msg.from),
                                                &msg.content,
                                                "mention",
                                            );
                                        }
                                        if msg_lower.contains("@ops") {
                                            let nudge_text =
                                                format!("{} ({}): {}", msg.from, msg.id, msg.content);
                                            state.nudge_ops_channel_lead(&nudge_text).await;
                                            info!(
                                                "Nudged ops channel lead about @ops mention in {} message",
                                                msg.from
                                            );
                                        }
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
/// - If the message contains a task ID (!N), route to the session working on that task
/// - Otherwise, route to the mentioned coworker by name
///
/// Also supports @all to broadcast to every active coworker and the lead.
pub(super) async fn route_mentions(state: &DaemonState, msg: &Message) {
    // Check for @all broadcast first
    if contains_at_all(&msg.content) {
        route_at_all(state, msg).await;
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

    // Extract task ID if present (!N pattern)
    let task_id = extract_task_id(&msg.content);
    if let Some(ref tid) = task_id {
        debug!("Found task ID !{} in message", tid);
    }

    let channel_lead_names: std::collections::HashSet<String> = {
        let ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions.keys().cloned().collect()
    };

    for name in mentions {
        // If a task ID is present, look up the coworker working on that task
        // and route to their session. This ensures the nudge goes to the correct
        // session even if the @mentioned name doesn't match.
        let target_name = if let Some(ref tid) = task_id {
            if let Some(coworker) = state.coworkers.get_by_task_id(tid) {
                if coworker.name != name {
                    info!(
                        "Task-based routing: @{} with !{} routes to {} (working on this task)",
                        name, tid, coworker.name
                    );
                }
                Some(coworker.name)
            } else {
                // No coworker working on this task - fall back to name-based routing
                debug!(
                    "No coworker found working on task !{}, falling back to @{}",
                    tid, name
                );
                Some(name)
            }
        } else {
            Some(name)
        };

        let Some(target_name) = target_name else {
            continue;
        };

        let is_running = state.coworkers.get(&target_name).is_some();
        let nudge_text = format!("{} said ({}): {}", msg.from, msg.id, msg.content);

        // Decide action using pure decision function
        let action = crate::rules::decide_mention_action(
            &target_name,
            &msg.from,
            is_running,
            state.is_at_dev_limit(&channel_lead_names),
            &nudge_text,
        );

        // Convert MentionAction → Effects, execute via the standard pipeline.
        let effects = mention_action_to_effects(action, &target_name, &state.repo_name);
        super::effects::execute_effects(effects, state).await;
    }
}

/// Route an @all broadcast: nudge every running coworker and the lead, except the sender.
async fn route_at_all(state: &DaemonState, msg: &Message) {
    // Only nudge Running coworkers — Stopping/Starting coworkers have no active session.
    let running_coworkers = state.coworkers.list_running();
    let nudge_text = format!("{} said ({}): {}", msg.from, msg.id, msg.content);

    info!(
        "@all broadcast from {} to {} running coworker(s) + lead",
        msg.from,
        running_coworkers.len()
    );

    // Nudge the lead (unless the lead sent the message)
    if !msg.from.eq_ignore_ascii_case("lead") && !msg.from.eq_ignore_ascii_case(&state.repo_name) {
        state.nudge_lead(&nudge_text).await;
        info!("Nudged lead for @all from {}", msg.from);
    }

    // Nudge all running coworkers (except the sender)
    for coworker in &running_coworkers {
        if coworker.name.eq_ignore_ascii_case(&msg.from) {
            continue;
        }

        if let Err(e) = state
            .session_manager
            .send_message(&coworker.name, &nudge_text)
            .await
        {
            warn!("Failed to nudge {} for @all: {}", coworker.name, e);
        } else {
            info!("Nudged {} for @all from {}", coworker.name, msg.from);
        }
    }
}

/// Convert a `MentionAction` decision into executable effects.
///
/// Pure conversion: takes the decision from `decide_mention_action` and maps
/// it to `Effect` variants that the standard `execute_effects` pipeline handles.
fn mention_action_to_effects(
    action: crate::rules::MentionAction,
    coworker_name: &str,
    repo_name: &str,
) -> Vec<super::effects::Effect> {
    use super::effects::Effect;

    match action {
        crate::rules::MentionAction::Nudge { name, message } => {
            vec![Effect::NudgeCoworker { name, message }]
        }
        crate::rules::MentionAction::Spawn { name, message } => {
            let config = crate::launch::LaunchConfig::coworker(
                name.clone(),
                repo_name.to_string(),
                crate::launch::SessionMode::Resume,
                Some(message),
            );
            vec![Effect::SpawnCoworkerWithCallbacks {
                config,
                on_success: vec![Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!("Called in {} in response to @mention", name),
                    channel: Some(OPS_CHANNEL.to_string()),
                }],
                on_failure: vec![Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!("Failed to call in {} for @mention", name),
                    channel: Some(OPS_CHANNEL.to_string()),
                }],
            }]
        }
        crate::rules::MentionAction::Skip { ref reason } => {
            debug!("{}", reason);
            if reason.contains("dev limit") {
                vec![Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!(
                        "Cannot call in {} for @mention: dev coworkers limit reached",
                        coworker_name
                    ),
                    channel: Some(OPS_CHANNEL.to_string()),
                }]
            } else {
                vec![]
            }
        }
    }
}

#[path = "chat_tests.rs"]
#[cfg(test)]
mod tests;
