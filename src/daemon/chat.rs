//! Chat monitor — @mention routing and @all broadcast.
//!
//! Runs a background `tailf` loop watching `channel.jsonl` for new messages.
//! When a message with @mentions is detected, spawns or nudges the mentioned
//! coworkers. Also handles @all broadcasts.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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
                                        let lead_mention = format!("@{}", state.project_name).to_lowercase();
                                        if msg_lower.contains("@lead") || msg_lower.contains(&lead_mention) {
                                            let effect = super::effects::Effect::NudgeChannelLead {
                                                channel_name: state.default_channel_name().to_string(),
                                                reason: super::wake_reason::WakeReason::Mention {
                                                    from: msg.from.clone(),
                                                    content: msg.content.clone(),
                                                    msg_id: msg.thread_anchor_id().to_string(),
                                                    thread_ctx: None,
                                                },
                                            };
                                            super::effects::execute_effects(vec![effect], &state).await;
                                            info!(
                                                "Nudged lead about @{} mention in {} message",
                                                state.project_name,
                                                msg.from
                                            );
                                        }
                                        if msg_lower.contains("@ops") {
                                            let effect = super::effects::Effect::NudgeChannelLead {
                                                channel_name: OPS_CHANNEL.to_string(),
                                                reason: super::wake_reason::WakeReason::Mention {
                                                    from: msg.from.clone(),
                                                    content: msg.content.clone(),
                                                    msg_id: msg.thread_anchor_id().to_string(),
                                                    thread_ctx: None,
                                                },
                                            };
                                            super::effects::execute_effects(vec![effect], &state).await;
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

    // Extract task ID if present (!N pattern) and resolve to the owning coworker.
    // Coworker.current_task is a display-only field (always None in storage);
    // the task system is the authoritative source for task-to-owner mapping.
    let task_owner: Option<String> = extract_task_id(&msg.content).and_then(|tid| {
        debug!("Found task ID !{} in message", tid);
        let owner =
            crate::tasks::get_in_progress_tasks_with_subjects_for_repo(state.paths.dir_key())
                .into_iter()
                .find(|(task_id, _, _)| task_id == &tid)
                .map(|(_, _, owner)| owner)
                .filter(|o| !o.is_empty());
        if owner.is_none() {
            debug!(
                "No in-progress task !{} found, falling back to name-based routing",
                tid
            );
        }
        owner
    });

    let channel_lead_names = {
        let ps = state.persistent_state.lock().await;
        ps.channel_lead_names()
    };

    let mut task_rerouted = false;
    for name in mentions {
        // If a task owner was resolved, route to their session instead of the @mentioned name.
        // This ensures nudges reach the correct session even when coworker names are reassigned.
        // Only reroute the first mention; secondary mentions (e.g., "cc @other") route normally.
        let target_name = match &task_owner {
            Some(owner) if !task_rerouted && !owner.eq_ignore_ascii_case(&name) => {
                if state.coworkers.get(owner).is_some() {
                    info!(
                        "Task-based routing: @{} routes to {} (working on the task)",
                        name, owner
                    );
                    task_rerouted = true;
                    owner.clone()
                } else {
                    debug!(
                        "Task owner {} is not running, falling back to @{}",
                        owner, name
                    );
                    name
                }
            }
            _ => name,
        };

        // Deduplicate: skip if we've already nudged the actual target for this message.
        // Keyed on target_name (the resolved recipient) to correctly handle task-based rerouting.
        let should_nudge = state.cooldowns.lock().unwrap().check_and_record(
            &format!("chat_mention_{}", target_name),
            &msg.id,
            Duration::from_secs(3600),
        );
        if !should_nudge {
            debug!(
                "Skipping duplicate @mention nudge for {} (msg {})",
                target_name, msg.id
            );
            continue;
        }

        let is_running = state.coworkers.get(&target_name).is_some();
        let nudge_text = render_thread_context(msg);

        // Decide action using pure decision function
        let action = crate::rules::decide_mention_action(
            &target_name,
            &msg.from,
            is_running,
            state.is_at_dev_limit(&channel_lead_names),
            &nudge_text,
        );

        // Convert MentionAction → Effects, execute via the standard pipeline.
        let name_session_map: std::collections::HashMap<String, String> =
            state.name_to_session.lock().unwrap().clone();

        // Look up whether the @mentioned name has an existing reviewer session.
        // If so, we resume it instead of spawning a fresh dev session.
        let reviewer_session = {
            let ps = state.persistent_state.lock().await;
            ps.sessions
                .iter()
                .filter(|(_, r)| {
                    r.is_reviewer
                        && (r.preferred_name.as_deref() == Some(&target_name)
                            || r.current_name.as_deref() == Some(&target_name))
                })
                // Pick the most recently active reviewer to avoid nondeterminism
                // when multiple stopped reviewer sessions share the same name.
                .max_by_key(|(_, r)| r.last_active)
                .map(|(sid, r)| ReviewerSessionInfo {
                    session_id: sid.clone(),
                    task_id: r.task_id.clone(),
                })
        };

        let effects = mention_action_to_effects(
            action,
            &target_name,
            &state.project_name,
            &name_session_map,
            reviewer_session,
        );
        super::effects::execute_effects(effects, state).await;
    }
}

/// Route an @all broadcast: nudge every running coworker and the lead, except the sender.
async fn route_at_all(state: &DaemonState, msg: &Message) {
    // Only nudge Running coworkers — Stopping/Starting coworkers have no active session.
    let running_coworkers = state.coworkers.list_running();
    let nudge_text = render_thread_context(msg);

    info!(
        "@all broadcast from {} to {} running coworker(s) + lead",
        msg.from,
        running_coworkers.len()
    );

    // Nudge the lead (unless the lead sent the message)
    if !msg.from.eq_ignore_ascii_case(&state.project_name) {
        let should_nudge_lead = state.cooldowns.lock().unwrap().check_and_record(
            "chat_at_all_lead",
            &msg.id,
            Duration::from_secs(3600),
        );
        if should_nudge_lead {
            // Use Nudge (not Mention) for @all: it's an undirected broadcast,
            // not a directed mention. "said" framing matches what coworkers see.
            let nudge_effect = super::effects::Effect::NudgeChannelLead {
                channel_name: state.default_channel_name().to_string(),
                reason: super::wake_reason::WakeReason::Nudge {
                    message: nudge_text.clone(),
                },
            };
            super::effects::execute_effects(vec![nudge_effect], state).await;
            info!("Nudged lead for @all from {}", msg.from);
        }
    }

    // Nudge all running coworkers (except the sender)
    for coworker in &running_coworkers {
        if coworker.name.eq_ignore_ascii_case(&msg.from) {
            continue;
        }

        // Deduplicate: skip if we've already nudged this coworker for this message.
        let should_nudge = state.cooldowns.lock().unwrap().check_and_record(
            &format!("chat_at_all_{}", coworker.name),
            &msg.id,
            Duration::from_secs(3600),
        );
        if !should_nudge {
            debug!(
                "Skipping duplicate @all nudge for {} (msg {})",
                coworker.name, msg.id
            );
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

/// Human-friendly label for nudges that preserves thread context by using
/// the parent message ID when the source message is a thread reply.
///
/// For thread replies, appends instructions for replying and reading the thread
/// so the recipient knows how to participate in the conversation.
fn render_thread_context(msg: &Message) -> String {
    let base = format!(
        "{} said ({}): {}",
        msg.from,
        msg.thread_anchor_id(),
        msg.content
    );

    if let Some(parent_id) = &msg.thread_parent_id {
        let ctx = super::wake_reason::ThreadContext {
            parent_id: parent_id.clone(),
            channel_name: msg.channel_name().to_string(),
        };
        format!("{base}\n\n{}", ctx.reply_instructions())
    } else {
        base
    }
}

/// Convert a `MentionAction` decision into executable effects.
///
/// Pure conversion: takes the decision from `decide_mention_action` and maps
/// it to `Effect` variants that the standard `execute_effects` pipeline handles.
/// Info about an existing reviewer session for the @mentioned name.
///
/// When set, the `Spawn` arm of `mention_action_to_effects` produces a
/// `ResumeCoworker` effect instead of `SpawnCoworkerWithCallbacks`, preserving
/// the reviewer's session context and role.
#[derive(Debug, Clone)]
pub(crate) struct ReviewerSessionInfo {
    pub session_id: String,
    pub task_id: Option<String>,
}

fn mention_action_to_effects(
    action: crate::rules::MentionAction,
    coworker_name: &str,
    repo_name: &str,
    name_session_map: &std::collections::HashMap<String, String>,
    reviewer_session: Option<ReviewerSessionInfo>,
) -> Vec<super::effects::Effect> {
    use super::effects::Effect;

    match action {
        crate::rules::MentionAction::Nudge { name, message } => {
            let session_id = name_session_map
                .get(&name.to_lowercase())
                .cloned()
                .unwrap_or_default();
            vec![Effect::nudge_session(session_id, message)]
        }
        crate::rules::MentionAction::Spawn { name, message } => {
            // If the @mentioned name has an existing reviewer session, resume it
            // instead of spawning a fresh dev session. This preserves reviewer
            // context (ephemeral lifecycle, review state, etc.).
            if let Some(info) = reviewer_session {
                let config = crate::launch::LaunchConfig::resume_reviewer(
                    name.clone(),
                    repo_name.to_string(),
                    info.session_id.clone(),
                    Some(message),
                    info.task_id,
                );
                return vec![Effect::ResumeCoworker {
                    name,
                    session_id: info.session_id,
                    config,
                }];
            }

            let config = crate::launch::LaunchConfig::coworker(
                name.clone(),
                repo_name.to_string(),
                crate::launch::SessionMode::Resume,
                Some(message),
                None,
            );
            vec![Effect::SpawnCoworkerWithCallbacks {
                config,
                on_success: vec![Effect::post_to_ops(format!(
                    "Called in {} in response to @mention",
                    name
                ))],
                on_failure: vec![Effect::post_to_ops(format!(
                    "Failed to call in {} for @mention",
                    name
                ))],
            }]
        }
        crate::rules::MentionAction::Skip { ref reason } => {
            debug!("{}", reason);
            if reason.contains("dev limit") {
                vec![Effect::post_to_ops(format!(
                    "Cannot call in {} for @mention: dev coworkers limit reached",
                    coworker_name
                ))]
            } else {
                vec![]
            }
        }
    }
}

#[path = "chat_tests.rs"]
#[cfg(test)]
mod tests;
