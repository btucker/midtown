//! RPC handlers for coworker operations (coworker.spawn, coworker.break,
//! coworker.list, coworker.view, coworker.report-state, coworker.nudge,
//! coworker.asking).
//!
//! Extracted from `rpc.rs` to keep the main dispatch module focused on routing.

use std::collections::HashMap;

use tracing::{debug, error, info, warn};

use crate::message::{Message, MessageType};
use crate::rpc::{RequestId, Response, RpcError};

use super::constants::*;
use super::{DaemonState, effects, snapshot};

/// Build a JSON array of active coworkers with their current tasks.
///
/// Shared by `handle_coworker_list` and `handle_status` — both need the same
/// coworker-name → task-subject mapping from Claude Code's task storage.
pub(super) fn collect_coworker_list(state: &DaemonState) -> Vec<serde_json::Value> {
    let coworker_tasks: HashMap<String, String> =
        crate::tasks::get_in_progress_tasks_with_subjects()
            .into_iter()
            .filter_map(|(_task_id, subject, owner)| {
                if owner.is_empty() {
                    None
                } else {
                    Some((owner.to_lowercase(), subject))
                }
            })
            .collect();

    state
        .coworkers
        .list()
        .iter()
        .map(|cw| {
            let current_task = coworker_tasks.get(&cw.name.to_lowercase()).cloned();
            serde_json::json!({
                "name": cw.name,
                "status": cw.status.to_string(),
                "current_task": current_task,
                "started_at": cw.started_at.to_rfc3339(),
            })
        })
        .collect()
}

/// Handle coworker.spawn RPC method.
pub(super) async fn handle_coworker_spawn(
    id: RequestId,
    state: &DaemonState,
    resume: bool,
    prompt: Option<String>,
    provider: crate::auth::AuthProvider,
) -> Response {
    // Check dev coworkers limit (reserve slots for reviewers)
    if state.is_at_dev_limit() {
        return Response::error(
            id,
            RpcError::new(
                -32603,
                format!(
                    "Dev coworkers limit ({}) reached (reserving {} slots for reviewers). Adjust with MIDTOWN_MAX_COWORKERS or max_coworkers in config.toml",
                    state.max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1),
                    REVIEW_HEADROOM
                ),
            ),
        );
    }

    // Pick a name for the coworker
    let name = match state.coworkers.next_available_name() {
        Some(n) => n,
        None => {
            return Response::error(
                id,
                RpcError::new(
                    -32603,
                    "No available coworker slots (all avenue names in use)".to_string(),
                ),
            );
        }
    };

    // Build headless launch config
    let team = crate::mailbox::team_name_for_repo(&state.repo_name);
    let config = crate::launch::LaunchConfig {
        name,
        session_mode: if resume {
            crate::launch::SessionMode::Resume
        } else {
            crate::launch::SessionMode::Fresh
        },
        role: crate::launch::CoworkerRole::Coworker,
        initial_prompt: prompt,
        additional_dirs: vec![],
        restrict_setting_sources: true,
        pr_number: None,
        team_name: Some(team),
        working_dir: None,
        model: "sonnet".to_string(),
        channel: None,
        auth_profile_dir: None,
        auth_provider: provider, // Resolved by spawn_coworker()
    };

    // Spawn via the headless path (creates worktree + headless session)
    match state.spawn_coworker(&config).await {
        Ok(()) => {
            info!("Spawned coworker: {}", config.name);
            state.broadcast_coworker_update(&config.name, "running", None);

            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Called in coworker: {}", config.name),
                    "coworkers": [{
                        "name": config.name,
                        "status": "running",
                        "current_task": null,
                        "started_at": chrono::Utc::now().to_rfc3339(),
                    }]
                }),
            )
        }
        Err(e) => {
            error!("Failed to spawn coworker: {}", e);
            Response::error(id, RpcError::new(-32603, e.to_string()))
        }
    }
}

/// Handle coworker.break RPC method.
pub(super) async fn handle_coworker_break(
    id: RequestId,
    name: &str,
    state: &DaemonState,
) -> Response {
    // Clear reviewer assignment if this coworker is reviewing a PR.
    // This must happen BEFORE the early return to handle the case where
    // the coworker is not tracked (already deregistered, crashed, or broken twice)
    // but still has an active reviewer assignment. Otherwise the daemon would
    // respawn them on the next tick.
    //
    // Uses the Effect-based architecture to stay consistent with other RPC handlers
    // and avoid duplicating cleanup logic.
    let cleanup_effects = vec![effects::Effect::ClearOrphanedReviewerAssignments {
        orphaned_coworkers: vec![name.to_string()],
    }];
    effects::execute_effects(cleanup_effects, state).await;

    // Check if the coworker is tracked - if not, they're already "on break"
    if state.coworkers.get(name).is_none() {
        info!("Coworker {} is already on break (not tracked)", name);
        return Response::success(
            id,
            serde_json::json!({
                "success": true,
                "message": format!("{} is already on break", name),
            }),
        );
    }

    state.broadcast_coworker_update(name, "stopped", None);

    // Shut down the headless session, then deregister from tracking
    if let Err(e) = state.session_manager.shutdown(name).await {
        warn!("Failed to shut down headless session for {}: {}", name, e);
    }
    state.coworkers.deregister(name);
    state.record_coworker_stop_time(name);

    info!("Sent coworker on a break: {}", name);
    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!("Sent {} on a break", name),
        }),
    )
}

/// Handle coworker.list RPC method.
pub(super) fn handle_coworker_list(id: RequestId, state: &DaemonState) -> Response {
    let coworkers = collect_coworker_list(state);
    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "coworkers": coworkers,
        }),
    )
}

/// Handle coworker.view RPC method.
///
/// Returns the recent output from a headless coworker session by reading
/// the JSONL log file. This enables `midtown coworker view` to work with
/// headless coworkers.
pub(super) async fn handle_coworker_view(
    id: RequestId,
    name: &str,
    state: &DaemonState,
) -> Response {
    match state.session_manager.get_output(name).await {
        Some(output) => Response::success(
            id,
            serde_json::json!({
                "success": true,
                "output": output,
            }),
        ),
        None => Response::error(
            id,
            RpcError::new(
                -32602,
                format!("No headless session found for coworker '{}'", name),
            ),
        ),
    }
}

/// Handle coworker.report-state RPC method.
///
/// Stores the coworker's workflow phase in daemon memory and updates the
/// tmux tab display. The daemon is the single authority for coworker state.
///
/// When a coworker reports `Idle`, they are immediately sent on break.
/// This eliminates the race between idle detection (daemon tick) and stuck
/// detection (pane unchanged), which could cause idle coworkers to be
/// incorrectly restarted as "stuck".
pub(super) async fn handle_coworker_report_state(
    id: RequestId,
    name: &str,
    phase_str: &str,
    task_id: Option<u32>,
    state: &DaemonState,
) -> Response {
    // Parse the phase string via FromStr (implemented in coworker_state.rs)
    let phase: crate::coworker_state::WorkflowPhase = match phase_str.parse() {
        Ok(p) => p,
        Err(e) => return Response::error(id, RpcError::new(-32602, e)),
    };

    // For Idle phase, immediately send the coworker on break.
    // This prevents the race condition where stuck detection fires before
    // the daemon's periodic idle check.
    if phase == crate::coworker_state::WorkflowPhase::Idle {
        // Check if coworker is tracked (should be, since they're reporting state)
        if state.coworkers.get(name).is_some() {
            // Build shutdown effect with conditional follow-up effects.
            // The channel message and WebSocket broadcast only execute if shutdown succeeds.
            // This ensures all cleanup steps (cooldowns, pending nudges, worktree unbinding)
            // stay in sync with Effect::ShutdownCoworker in effects.rs.
            let shutdown_effects = vec![effects::Effect::ShutdownCoworkerWithCallbacks {
                name: name.to_string(),
                message: String::new(), // No goodbye message needed for idle shutdown
                session_id: None,
                on_success: vec![
                    effects::Effect::PostSystemMessage {
                        message: format!("☕ {} reported idle, taking a break", name),
                    },
                    effects::Effect::BroadcastCoworkerUpdate {
                        name: name.to_string(),
                        status: "stopped".to_string(),
                        current_task: None,
                    },
                ],
            }];

            effects::execute_effects(shutdown_effects, state).await;

            // Immediately trigger task dispatch so pending tasks get picked up
            // without waiting for the next TaskDispatchTick (up to 5 seconds).
            // This is the same pattern as daemon.check-pending RPC.
            let snap = snapshot::collect_world_snapshot(state).await;
            let pending_effects = super::dispatch::spawn_for_pending_tasks(&snap, state);
            if !pending_effects.is_empty() {
                info!(
                    "Immediate dispatch after {} idle: {} effect(s)",
                    name,
                    pending_effects.len()
                );
                state.mark_in_flight_spawns_from_effects(&pending_effects);
                effects::execute_effects(pending_effects, state).await;
            }

            info!("Coworker {} went on break after reporting idle", name);
            return Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("{} → break (idle)", name),
                }),
            );
        }
    }

    // For Completed phase, delegate to rpc_tasks for task completion logic
    if phase == crate::coworker_state::WorkflowPhase::Completed {
        super::rpc_tasks::handle_completed_phase(name, task_id, state).await;
    }

    // Store in unified coworker record
    let status_display = {
        let mut records = state.coworker_records.write().await;
        crate::rules::set_workflow(&mut records, name, phase, task_id);
        records
            .get(name)
            .and_then(|r| r.display_status())
            .unwrap_or_default()
    };

    // Update tmux tab display
    if let Err(e) = state
        .coworkers
        .update_status_formatted(name, &status_display)
    {
        debug!("Failed to update tmux tab for {}: {}", name, e);
    }

    info!("Coworker {} reported state: {}", name, status_display);
    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!("{} → {}", name, status_display),
        }),
    )
}

/// Handle coworker.nudge RPC method.
///
/// Sends the nudge directly to the coworker's tmux window without posting to the channel,
/// to avoid the chat monitor seeing the @mention and creating a duplicate nudge.
pub(super) async fn handle_coworker_nudge(
    id: RequestId,
    _from: &str,
    name: &str,
    message: &str,
    state: &DaemonState,
) -> Response {
    // Run blocking tmux operation in spawn_blocking to avoid blocking async runtime
    let coworkers = state.coworkers.clone();
    let name_owned = name.to_string();
    let message_owned = message.to_string();

    match tokio::task::spawn_blocking(move || coworkers.nudge(&name_owned, &message_owned)).await {
        Ok(Ok(())) => {
            info!("Nudged coworker {}: {}", name, message);
            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Nudged coworker: {}", name),
                }),
            )
        }
        Ok(Err(e)) => {
            error!("Failed to nudge coworker {}: {}", name, e);
            Response::error(id, RpcError::new(-32603, e.to_string()))
        }
        Err(e) => {
            error!("spawn_blocking panic while nudging {}: {}", name, e);
            Response::error(id, RpcError::new(-32603, "Internal error".to_string()))
        }
    }
}

/// Handle coworker.asking RPC method.
///
/// Called when a coworker uses AskUserQuestion tool. This:
/// 1. Posts the question to the channel
/// 2. Nudges the Lead with the question
/// 3. Marks the coworker as waiting for feedback
pub(super) async fn handle_coworker_asking(
    id: RequestId,
    name: &str,
    question: &str,
    state: &DaemonState,
) -> Response {
    // Post question to channel - route based on coworker's current task
    let task_channel = {
        let records = state.coworker_records.read().await;
        let task_id = records.get(name).and_then(|r| r.task_id);
        if let Some(tid) = task_id {
            let ps = state.persistent_state.lock().await;
            ps.task_channel.get(&tid.to_string()).cloned()
        } else {
            None
        }
    };

    let msg = if let Some(ch) = task_channel {
        Message::for_channel(
            &ch,
            name,
            format!("Question for Lead: {}", question),
            MessageType::Text,
        )
    } else {
        Message::text(name, format!("Question for Lead: {}", question))
    };

    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        error!("Failed to post question to channel: {}", e);
    }

    // Mark the coworker as waiting for feedback in tmux tab and nudge the Lead.
    // Run blocking tmux operations in spawn_blocking to avoid blocking async runtime.
    let coworkers = state.coworkers.clone();
    let name_owned = name.to_string();
    let nudge_message = format!("{} is asking: {}", name, question);

    tokio::task::spawn_blocking(move || {
        // Update tmux tab status
        if let Err(e) = coworkers.update_status_display(&name_owned, Some("waiting for feedback")) {
            debug!("Failed to update tmux tab for {}: {}", name_owned, e);
        }
        // Nudge the Lead with the question
        if let Err(e) = coworkers.nudge("Lead", &nudge_message) {
            debug!("Failed to nudge Lead: {}", e);
        }
    });

    info!("Coworker {} asking: {}", name, question);
    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!("Notified Lead about question from {}", name),
        }),
    )
}
