use tracing::{info, warn};

use super::DaemonState;
use crate::message::Message;

/// A side effect that the daemon should execute.
///
/// Pure evaluation functions return `Vec<Effect>` instead of performing side
/// effects inline. The `execute_effects` function is the single place where
/// effects are carried out. This separation makes the decision logic testable
/// without mocking async infrastructure.
#[derive(Debug)]
#[allow(dead_code)] // SpawnCoworker defined for when inline spawns are fully extracted
pub enum Effect {
    /// Spawn a coworker (or respawn an existing one).
    SpawnCoworker {
        name: String,
        prompt: String,
        isolated: bool,
    },
    /// Shut down a running coworker with a message.
    ShutdownCoworker { name: String, message: String },
    /// Nudge a coworker by sending a message to their tmux pane.
    NudgeCoworker { name: String, message: String },
    /// Nudge the lead by sending a message to the lead's tmux pane.
    NudgeLead { message: String },
    /// Post a message to the IRC-style channel (and broadcast to WebSocket clients).
    PostToChannel { sender: String, message: String },
    /// Broadcast a coworker status update to WebSocket clients.
    BroadcastCoworkerUpdate {
        name: String,
        status: String,
        current_task: Option<String>,
    },
    /// Record a cooldown entry (category + key).
    RecordCooldown { category: String, key: String },
    /// Schedule a usage-limit nudge at a specific time.
    SetUsageLimitNudge { at: tokio::time::Instant },
    /// Clear the scheduled usage-limit nudge (after it fires).
    ClearUsageLimitNudge,
    /// Reset a task back to pending (e.g. when a coworker can't be respawned).
    ResetTaskToPending { task_id: String, repo_name: String },
}

/// Execute a list of effects against the daemon state.
///
/// This is the imperative shell — the only place where side effects happen.
/// Each effect variant maps to a call on `DaemonState` or its subsystems.
pub async fn execute_effects(effects: Vec<Effect>, state: &DaemonState) {
    for effect in effects {
        match effect {
            Effect::SpawnCoworker {
                name,
                prompt,
                isolated,
            } => {
                match state
                    .coworkers
                    .spawn_with_name(&name, true, Some(&prompt), isolated)
                {
                    Ok(_) => {
                        info!("Respawned coworker {} successfully", name);
                        state.clear_coworker_activity(&name);
                    }
                    Err(e) => {
                        warn!("Failed to spawn coworker {}: {}", name, e);
                    }
                }
            }
            Effect::ShutdownCoworker { name, message } => {
                // Nudge the goodbye message first, then shut down
                if !message.is_empty()
                    && let Err(e) = state.coworkers.nudge(&name, &message)
                {
                    warn!("Failed to send shutdown message to {}: {}", name, e);
                }
                if let Err(e) = state.coworkers.shutdown(&name) {
                    warn!("Failed to shut down coworker {}: {}", name, e);
                }
            }
            Effect::NudgeCoworker { name, message } => {
                if let Err(e) = state.coworkers.nudge(&name, &message) {
                    warn!("Failed to nudge coworker {}: {}", name, e);
                }
            }
            Effect::NudgeLead { message } => {
                if let Err(e) = state.coworkers.nudge_lead(&message) {
                    warn!("Failed to nudge lead: {}", e);
                }
            }
            Effect::PostToChannel { sender, message } => {
                let msg = Message::text(&sender, &message);
                if let Err(e) = state.send_and_broadcast(&msg) {
                    warn!("Failed to post channel message: {}", e);
                }
            }
            Effect::BroadcastCoworkerUpdate {
                name,
                status,
                current_task,
            } => {
                state.broadcast_coworker_update(&name, &status, current_task.as_deref());
            }
            Effect::RecordCooldown { category, key } => {
                let mut cooldowns = state.cooldowns.lock().unwrap();
                cooldowns.record(&category, &key);
            }
            Effect::SetUsageLimitNudge { at } => {
                let mut nudge_at = state.usage_limit_nudge_at.lock().await;
                *nudge_at = Some(at);
            }
            Effect::ClearUsageLimitNudge => {
                let mut nudge_at = state.usage_limit_nudge_at.lock().await;
                *nudge_at = None;
            }
            Effect::ResetTaskToPending { task_id, repo_name } => {
                if let Err(e) = crate::tasks::reset_task_to_pending_for_repo(&task_id, &repo_name) {
                    warn!("Failed to reset task #{} to pending: {}", task_id, e);
                }
            }
        }
    }
}
