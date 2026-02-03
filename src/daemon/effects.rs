use tracing::{info, warn};

use super::DaemonState;
use super::trackers::PrIssueType;
use crate::message::Message;

/// A side effect that the daemon should execute.
///
/// Pure evaluation functions return `Vec<Effect>` instead of performing side
/// effects inline. The `execute_effects` function is the single place where
/// effects are carried out. This separation makes the decision logic testable
/// without mocking async infrastructure.
#[derive(Debug)]
pub enum Effect {
    /// Spawn a coworker using a typed launch configuration.
    SpawnCoworker(crate::tmux::ClaudeLaunchConfig),
    /// Shut down a running coworker with a message.
    ShutdownCoworker { name: String, message: String },
    /// Nudge a coworker by sending a message to their tmux pane.
    NudgeCoworker { name: String, message: String },
    /// Post a message to the IRC-style channel (and broadcast to WebSocket clients).
    PostToChannel { sender: String, message: String },
    /// Post a system message to the channel (and broadcast to WebSocket clients).
    PostSystemMessage { message: String },
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
    /// Assign task ownership on disk, then spawn a coworker atomically.
    ///
    /// If ownership assignment fails, neither spawn nor callbacks run.
    /// If spawn fails after ownership is assigned, ownership is rolled back
    /// (task reset to pending) and `on_failure` effects run.
    AssignAndSpawn {
        task_id: String,
        owner: String,
        repo_name: String,
        config: crate::tmux::ClaudeLaunchConfig,
        on_success: Vec<Effect>,
        on_failure: Vec<Effect>,
    },
    /// Assign task ownership on disk (no spawn).
    ///
    /// Used for tasks assigned to already-running coworkers. The ownership
    /// write is unconditional — if it fails, the error is logged.
    AssignTaskOwner { task_id: String, owner: String },
    /// Mark reminders as fired and persist to disk.
    ///
    /// Defers the mutation from the decision phase to the effect executor,
    /// keeping `check_and_fire_reminders` pure.
    MarkRemindersFired {
        fired_ids: Vec<String>,
        repo_name: String,
    },
    /// Auto-merge a PR using `gh pr merge --squash --auto`.
    AutoMergePr { pr_number: u64, title: String },
    /// Record a PR issue nudge in the tracker (prevents repeated nudges).
    RecordPrNudge {
        pr_number: u64,
        issue_type: PrIssueType,
    },
    /// Clear a saved PR break session after successful resume.
    ClearPrBreakSession { name: String },
    /// Send raw tmux keys to a coworker (e.g., Escape, Enter) without the
    /// nudge text mechanism. Used for recovering stuck states like compaction
    /// whirlpools or queued prompts.
    SendRawKeys { name: String, keys: String },
    /// Assign a reviewer to a PR in github_state and persist.
    AssignReviewer {
        pr_number: u64,
        reviewer_name: String,
        source: crate::github_state::AssignmentSource,
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
                // Record stop time for workflow features that need to track coworker lifecycle
                {
                    let mut stop_times = state.coworker_stop_times.write().unwrap();
                    stop_times.insert(name.to_lowercase(), chrono::Utc::now());
                }
                // Clear state file so next session doesn't read stale phase
                crate::coworker_state::clear_state(&state.repo_name, &name);
                // Clean up unified coworker record (health, workflow phase, etc.)
                {
                    let mut records = state.coworker_records.write().await;
                    records.remove(&name);
                }
                // Clear cooldown entries for this coworker (prevents stale state on respawn)
                {
                    let mut cooldowns = state.cooldowns.lock().unwrap();
                    cooldowns.clear_for_key(&name);
                }
            }
            Effect::NudgeCoworker { name, message } => {
                if let Err(e) = state.coworkers.nudge(&name, &message) {
                    warn!("Failed to nudge coworker {}: {}", name, e);
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
                // Clean up unified coworker record
                {
                    let mut records = state.coworker_records.write().await;
                    records.remove(&name);
                }
                // Clear cooldown entries for this coworker (prevents stale state on respawn)
                {
                    let mut cooldowns = state.cooldowns.lock().unwrap();
                    cooldowns.clear_for_key(&name);
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
            Effect::AssignAndSpawn {
                task_id,
                owner,
                repo_name,
                config,
                on_success,
                on_failure,
            } => {
                // Step 1: Assign ownership on disk
                if let Err(e) = crate::tasks::update_task_owner(&task_id, &owner) {
                    warn!(
                        "Failed to assign task #{} to {} — skipping spawn: {}",
                        task_id, owner, e
                    );
                    // Clear in-flight marker even on ownership failure
                    state.clear_task_spawn_in_flight(&task_id);
                    // Don't spawn or run callbacks — ownership write failed
                    continue;
                }
                info!("Assigned task #{} to {} on disk", task_id, owner);

                // Step 2: Spawn the coworker
                let name = config.name.clone();
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Spawned coworker {} successfully", name);
                        // Clear in-flight marker on success (task is now owned)
                        state.clear_task_spawn_in_flight(&task_id);
                        Box::pin(execute_effects(on_success, state)).await;
                    }
                    Err(e) => {
                        warn!("Failed to spawn coworker {}: {}", name, e);
                        // Roll back ownership — reset task to pending
                        if let Err(re) =
                            crate::tasks::reset_task_to_pending_for_repo(&task_id, &repo_name)
                        {
                            warn!(
                                "Failed to roll back task #{} ownership after spawn failure: {}",
                                task_id, re
                            );
                        } else {
                            info!(
                                "Rolled back task #{} to pending after spawn failure",
                                task_id
                            );
                        }
                        // Clear in-flight marker on failure (task was rolled back)
                        state.clear_task_spawn_in_flight(&task_id);
                        Box::pin(execute_effects(on_failure, state)).await;
                    }
                }
            }
            Effect::AssignTaskOwner { task_id, owner } => {
                if let Err(e) = crate::tasks::update_task_owner(&task_id, &owner) {
                    warn!("Failed to assign task #{} to {}: {}", task_id, owner, e);
                }
            }
            Effect::MarkRemindersFired {
                fired_ids,
                repo_name,
            } => {
                let mut ps = state.persistent_state.lock().await;
                for reminder in &mut ps.reminders.reminders {
                    if fired_ids.contains(&reminder.id) {
                        reminder.fired = true;
                    }
                }
                if let Err(e) = ps.save_for_repo(&repo_name) {
                    warn!(
                        "Failed to save daemon-state.json after firing reminders: {}",
                        e
                    );
                }
            }
            Effect::AutoMergePr { pr_number, title } => {
                auto_merge_pr(state, pr_number, &title).await;
            }
            Effect::RecordPrNudge {
                pr_number,
                issue_type,
            } => {
                let mut tracker = state.pr_issue_tracker.lock().await;
                tracker.record_nudge(pr_number, issue_type);
            }
            Effect::ClearPrBreakSession { name } => {
                let mut sessions = state.pr_break_sessions.write().unwrap();
                sessions.remove(&name);
                info!("Cleared PR break session for {}", name);
            }
            Effect::AssignReviewer {
                pr_number,
                reviewer_name,
                source,
            } => {
                let mut ps = state.persistent_state.lock().await;
                ps.github.assign_reviewer(pr_number, &reviewer_name, source);
                if let Err(e) = ps.save_for_repo(&state.repo_name) {
                    warn!("Failed to save daemon-state.json: {}", e);
                }
            }
            Effect::SendRawKeys { name, keys } => {
                if let Err(e) =
                    crate::tmux::send_keys_raw(state.coworkers.session_name(), &name, &keys)
                {
                    warn!("Failed to send raw keys to {}: {}", name, e);
                }
            }
            Effect::PostSystemMessage { message } => {
                let msg = Message::system(message);
                if let Err(e) = state.send_and_broadcast(&msg) {
                    warn!("Failed to post system message: {}", e);
                }
            }
        }
    }
}

/// Auto-merge a PR using `gh pr merge --squash`.
///
/// Posts a channel message on success or failure.
async fn auto_merge_pr(state: &DaemonState, pr_number: u64, title: &str) {
    use super::helpers::truncate_str;

    let output = match tokio::process::Command::new("gh")
        .args(["pr", "merge", &pr_number.to_string(), "--squash", "--auto"])
        .output()
        .await
    {
        Ok(output) => output,
        Err(e) => {
            warn!("Failed to run gh pr merge for PR #{}: {}", pr_number, e);
            return;
        }
    };

    if output.status.success() {
        info!("Auto-merge enabled for PR #{} ({})", pr_number, title);
        let msg = Message::new(
            "midtown",
            format!(
                "🤝 Auto-merge enabled for PR #{} ({}) — approved with all checks passing",
                pr_number,
                truncate_str(title, 40)
            ),
            crate::message::MessageType::Text,
        );
        if let Err(e) = state.send_and_broadcast(&msg) {
            warn!("Failed to post auto-merge message: {}", e);
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("gh pr merge failed for PR #{}: {}", pr_number, stderr);
        let msg = Message::new(
            "midtown",
            format!(
                "⚠️ Auto-merge failed for PR #{} ({}) — {}",
                pr_number,
                truncate_str(title, 40),
                truncate_str(stderr.trim(), 80)
            ),
            crate::message::MessageType::Text,
        );
        if let Err(e) = state.send_and_broadcast(&msg) {
            warn!("Failed to post auto-merge failure message: {}", e);
        }
    }
}
