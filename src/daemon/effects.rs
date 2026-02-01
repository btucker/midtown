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
    /// Spawn a coworker using a typed launch configuration.
    SpawnCoworker(crate::tmux::ClaudeLaunchConfig),
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
    /// Kill a zombie coworker (blank pane) and respawn with --continue.
    RespawnZombieCoworker { name: String },
    /// Spawn a coworker with conditional follow-up effects.
    ///
    /// On success, `on_success` effects are executed. On failure, `on_failure`
    /// effects are executed. This allows decision functions to express
    /// spawn-dependent branching as data without calling spawn inline.
    SpawnCoworkerWithCallbacks {
        config: crate::tmux::ClaudeLaunchConfig,
        on_success: Vec<Effect>,
        on_failure: Vec<Effect>,
    },
    /// Nudge a coworker with conditional follow-up effects on success.
    ///
    /// On success, `on_success` effects are executed. On failure, nothing extra
    /// happens (the nudge failure is logged). This allows decision functions to
    /// record cooldowns only when nudges succeed.
    NudgeCoworkerWithCallbacks {
        name: String,
        message: String,
        on_success: Vec<Effect>,
    },
}

/// Execute a list of effects against the daemon state.
///
/// This is the imperative shell — the only place where side effects happen.
/// Each effect variant maps to a call on `DaemonState` or its subsystems.
pub async fn execute_effects(effects: Vec<Effect>, state: &DaemonState) {
    for effect in effects {
        match effect {
            Effect::SpawnCoworker(config) => {
                let name = config.name.clone();
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Spawned coworker {} successfully", name);
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
                // Clear state file so next session doesn't read stale phase
                crate::coworker_state::clear_state(&state.repo_name, &name);
                // Clean up lifecycle state for the shut-down coworker
                {
                    let mut lc = state.coworker_lifecycles.write().await;
                    lc.remove(&name);
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
            Effect::RespawnZombieCoworker { name } => {
                // Shut down properly (kills window + removes from internal registry)
                if let Err(e) = state.coworkers.shutdown(&name) {
                    warn!("Failed to shutdown zombie coworker {}: {}", name, e);
                }
                // Clear state file so respawned session doesn't read stale phase
                crate::coworker_state::clear_state(&state.repo_name, &name);
                // Clean up lifecycle state so respawn starts fresh
                {
                    let mut lc = state.coworker_lifecycles.write().await;
                    lc.remove(&name);
                }
                // Brief delay to let tmux clean up
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                // Respawn with --continue to resume the coworker's conversation
                let config = crate::tmux::ClaudeLaunchConfig::coworker(
                    name.clone(),
                    state.repo_name.clone(),
                    crate::tmux::SessionMode::Resume,
                    None,
                );
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Respawned zombie coworker {} successfully", name);
                    }
                    Err(e) => {
                        warn!("Failed to respawn zombie coworker {}: {}", name, e);
                    }
                }
            }
            Effect::SpawnCoworkerWithCallbacks {
                config,
                on_success,
                on_failure,
            } => {
                let name = config.name.clone();
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Spawned coworker {} successfully", name);
                        // Recursively execute success follow-ups
                        Box::pin(execute_effects(on_success, state)).await;
                    }
                    Err(e) => {
                        warn!("Failed to spawn coworker {}: {}", name, e);
                        // Recursively execute failure follow-ups
                        Box::pin(execute_effects(on_failure, state)).await;
                    }
                }
            }
            Effect::NudgeCoworkerWithCallbacks {
                name,
                message,
                on_success,
            } => match state.coworkers.nudge(&name, &message) {
                Ok(()) => {
                    info!("Nudged coworker {} successfully", name);
                    Box::pin(execute_effects(on_success, state)).await;
                }
                Err(e) => {
                    warn!("Failed to nudge coworker {}: {}", name, e);
                }
            },
        }
    }
}
