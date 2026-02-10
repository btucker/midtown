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

        // Convert MentionAction → Effects, execute via the standard pipeline.
        let effects = mention_action_to_effects(action, &name, &state.repo_name);
        super::effects::execute_effects(effects, state).await;
    }
}

/// Route an @all broadcast: nudge every running coworker and the lead, except the sender.
async fn route_at_all(state: &DaemonState, msg: &Message) {
    // Only nudge Running coworkers — Stopping/Starting coworkers have no tmux window.
    let running_coworkers = state.coworkers.list_running();
    let nudge_text = format!("{} said: {}", msg.from, msg.content);

    info!(
        "@all broadcast from {} to {} running coworker(s) + lead",
        msg.from,
        running_coworkers.len()
    );

    // Nudge the lead (unless the lead sent the message)
    if !msg.from.eq_ignore_ascii_case("lead") {
        if let Err(e) = state.coworkers.nudge_lead(&nudge_text) {
            warn!("Failed to nudge lead for @all: {}", e);
        } else {
            info!("Nudged lead for @all from {}", msg.from);
        }
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
            vec![Effect::NudgeCoworker {
                name,
                message,
                session_id: None,
            }]
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
                    channel: None,
                }],
                on_failure: vec![Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!("Failed to call in {} for @mention", name),
                    channel: None,
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
                    channel: None,
                }]
            } else {
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::effects::Effect;
    use super::*;
    use crate::rules::MentionAction;

    #[test]
    fn mention_nudge_produces_nudge_effect() {
        let action = MentionAction::Nudge {
            name: "lexington".to_string(),
            message: "lead said: @lexington check this".to_string(),
        };
        let effects = mention_action_to_effects(action, "lexington", "test-repo");

        assert_eq!(effects.len(), 1);
        assert!(
            matches!(&effects[0], Effect::NudgeCoworker { name, .. } if name == "lexington"),
            "Expected NudgeCoworker for lexington"
        );
    }

    #[test]
    fn mention_spawn_produces_spawn_with_callbacks() {
        let action = MentionAction::Spawn {
            name: "park".to_string(),
            message: "lead said: @park fix the bug".to_string(),
        };
        let effects = mention_action_to_effects(action, "park", "test-repo");

        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::SpawnCoworkerWithCallbacks {
                config,
                on_success,
                on_failure,
            } => {
                assert_eq!(config.name, "park");
                assert!(!on_success.is_empty(), "Should have success callback");
                assert!(!on_failure.is_empty(), "Should have failure callback");
                // Success callback should post to channel
                assert!(
                    matches!(&on_success[0], Effect::PostToChannel { message, .. }
                        if message.contains("park") && message.contains("@mention")),
                    "Success callback should mention park and @mention"
                );
            }
            _ => panic!("Expected SpawnCoworkerWithCallbacks, got {:?}", effects[0]),
        }
    }

    #[test]
    fn mention_skip_produces_no_effects() {
        let action = MentionAction::Skip {
            reason: "lexington is already active, no need to spawn".to_string(),
        };
        let effects = mention_action_to_effects(action, "lexington", "test-repo");
        assert!(
            effects.is_empty(),
            "Skip (non dev-limit) should produce no effects"
        );
    }

    #[test]
    fn mention_skip_dev_limit_posts_to_channel() {
        let action = MentionAction::Skip {
            reason: "Cannot spawn amsterdam: dev limit reached".to_string(),
        };
        let effects = mention_action_to_effects(action, "amsterdam", "test-repo");

        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::PostToChannel { message, .. } => {
                assert!(message.contains("amsterdam"), "Should mention the coworker");
                assert!(
                    message.contains("dev coworkers limit"),
                    "Should explain the limit"
                );
            }
            _ => panic!("Expected PostToChannel for dev limit, got {:?}", effects[0]),
        }
    }
}
