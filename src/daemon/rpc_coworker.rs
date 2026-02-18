//! Coworker lifecycle RPC handlers.
//!
//! Handles `coworker.*` methods: spawn, break, list, view, report-state,
//! nudge, and asking. These are tightly coupled to the coworker management
//! subsystem (worktrees, headless sessions).

use tracing::{debug, error, info, warn};

use crate::rpc::{RequestId, Response, RpcError};

use super::constants::*;
use super::{DaemonState, effects, snapshot};

// ============================================================================
// Handlers
// ============================================================================

/// Handle coworker.spawn RPC method.
pub(super) async fn handle_coworker_spawn(
    id: RequestId,
    state: &DaemonState,
    resume: bool,
    prompt: Option<String>,
    provider: crate::auth::AuthProvider,
) -> Response {
    // Check dev coworkers limit (reserve slots for reviewers)
    let channel_lead_names: std::collections::HashSet<String> = {
        let ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions.keys().cloned().collect()
    };
    if state.is_at_dev_limit(&channel_lead_names) {
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

/// Handle lead.spawn RPC method.
///
/// Spawns the Lead as a headless session. Idempotent — returns success
/// if the lead is already running.
pub(super) async fn handle_lead_spawn(
    id: RequestId,
    state: &DaemonState,
    provider: crate::auth::AuthProvider,
) -> Response {
    // Idempotent: if lead is already running, return success
    if state.session_manager.is_alive("lead").await {
        return Response::success(
            id,
            serde_json::json!({
                "success": true,
                "message": "Lead already running",
            }),
        );
    }

    let mut config = crate::launch::LaunchConfig::lead(&state.repo_name);
    config.auth_provider = provider;

    // Use the canonical lead worktree path so spawn_coworker uses it
    // instead of falling through to the legacy coworker-named path.
    let lead_wt = crate::paths::lead_worktree_path(&state.repo_name);
    if lead_wt.exists() {
        config.working_dir = Some(lead_wt);
    }

    match state.spawn_coworker(&config).await {
        Ok(()) => {
            info!("Spawned headless lead session");
            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": "Spawned headless lead session",
                }),
            )
        }
        Err(e) => {
            error!("Failed to spawn lead: {}", e);
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
    // Build a map of coworker name -> task display string from in_progress tasks
    // Format: "!1234 Task subject" (task ID + subject) — matches handle_status()
    let coworker_tasks: std::collections::HashMap<String, String> =
        crate::tasks::get_in_progress_tasks_with_subjects()
            .into_iter()
            .filter_map(|(task_id, subject, owner)| {
                if owner.is_empty() {
                    None
                } else {
                    let task_display = format!("!{} {}", task_id, subject);
                    Some((owner.to_lowercase(), task_display))
                }
            })
            .collect();

    let coworkers: Vec<serde_json::Value> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| {
            // Look up current task from task storage (case-insensitive)
            let current_task = coworker_tasks.get(&cw.name.to_lowercase()).cloned();
            serde_json::json!({
                "name": cw.name,
                "status": cw.status.to_string(),
                "current_task": current_task,
                "started_at": cw.started_at.to_rfc3339(),
                "provider": cw.provider.as_str(),
                "profile": cw.profile,
            })
        })
        .collect();

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
/// the JSONL log file.
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
/// Stores the coworker's workflow phase and progress in daemon memory and updates the
/// web UI status. When a coworker reports `Idle`, they are immediately
/// sent on break. When they report `Completed`, task cleanup is handled.
pub(super) async fn handle_coworker_report_state(
    id: RequestId,
    name: &str,
    phase_str: &str,
    task_id: Option<u32>,
    progress: Option<u8>,
    state: &DaemonState,
) -> Response {
    // Parse the phase string via FromStr (implemented in coworker_state.rs)
    let phase: crate::coworker_state::WorkflowPhase = match phase_str.parse() {
        Ok(p) => p,
        Err(e) => return Response::error(id, RpcError::new(-32602, e)),
    };

    // For Idle phase, immediately send the coworker on break.
    if phase == crate::coworker_state::WorkflowPhase::Idle && state.coworkers.get(name).is_some() {
        let shutdown_effects = vec![effects::Effect::ShutdownCoworkerWithCallbacks {
            name: name.to_string(),
            message: String::new(),
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

    // For Completed phase, handle task cleanup.
    if phase == crate::coworker_state::WorkflowPhase::Completed {
        let effective_task_id: Option<String> = task_id.map(|id| id.to_string()).or_else(|| {
            let assignments = state.coworker_task_assignments.lock().unwrap();
            assignments
                .get(&name.to_lowercase())
                .map(|a| a.task_id.clone())
        });

        if let Some(ref tid) = effective_task_id {
            let has_open_pr = task_has_open_pr(tid, state).await;

            if has_open_pr {
                debug!(
                    "Task !{} has open PR, deferring completion to merge path",
                    tid
                );
            } else {
                warn!(
                    "Task !{} reported completed by {} but has no PR — nudging to open PR",
                    tid, name
                );
                let nudge_effects = vec![
                    effects::Effect::NudgeCoworker {
                        name: name.to_string(),
                        message: format!(
                            "Task !{} has no PR yet. Please open a PR for your changes and then go idle with `midtown state idle`. The daemon will complete the task when the PR merges.",
                            tid
                        ),
                        session_id: None,
                    },
                    effects::Effect::PostToChannel {
                        sender: "midtown".to_string(),
                        message: format!(
                            "⚠️ {} reported task !{} completed without a PR — nudged to open PR first",
                            name, tid
                        ),
                        channel: None,
                    },
                ];
                effects::execute_effects(nudge_effects, state).await;
            }
        }

        // Always clear the coworker's assignment (they're done regardless)
        state.clear_coworker_assignments(name);
    }

    // Store in unified coworker record and capture updated progress for broadcast
    let (status_display, phase_abbrev, updated_progress, time_estimate) = {
        let mut records = state.coworker_records.write().await;
        crate::rules::set_workflow(&mut records, name, phase, task_id, progress);
        let record = records.get(name);
        let display = record.and_then(|r| r.display_status()).unwrap_or_default();
        let phase_abbrev = record
            .and_then(|r| r.workflow_phase)
            .map(|p| p.abbreviation().to_string());
        let updated_progress = record.and_then(|r| r.progress);
        let time_estimate = record.and_then(|r| r.format_time_remaining());
        (display, phase_abbrev, updated_progress, time_estimate)
    };

    // Broadcast progress/phase update to web UI so it doesn't have to wait for the 30s poll
    let health = {
        let health_guard = state.headless_health.read().unwrap();
        health_guard.get(name).map(|h| {
            if !h.is_alive {
                "red".to_string()
            } else if h.has_usage_limit || h.has_api_error {
                "yellow".to_string()
            } else {
                "green".to_string()
            }
        })
    };
    state.broadcast_web_update(crate::web::coworker_progress_update(
        name,
        phase_abbrev,
        updated_progress,
        time_estimate,
        health,
    ));

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
pub(super) async fn handle_coworker_nudge(
    id: RequestId,
    _from: &str,
    name: &str,
    message: &str,
    state: &DaemonState,
) -> Response {
    // Always enqueue to headed intercom for wrapper-managed sessions.
    state.enqueue_headed_nudge(name, message).await;

    // Best-effort headless delivery for active headless sessions.
    let delivered_headless = match state.session_manager.send_message(name, message).await {
        Ok(()) => true,
        Err(e) => {
            let text = e.to_string();
            if text.contains("No headless session for") || text.contains("has stopped") {
                debug!(
                    "No active headless session for coworker {}, queued headed nudge only",
                    name
                );
            } else {
                warn!(
                    "Headless nudge delivery failed for coworker {} (still queued headed): {}",
                    name, e
                );
            }
            false
        }
    };

    info!("Queued nudge for coworker {}: {}", name, message);
    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!("Nudged coworker: {}", name),
            "queued_headed": true,
            "delivered_headless": delivered_headless
        }),
    )
}

/// Handle coworker.asking RPC method.
pub(super) async fn handle_coworker_asking(
    id: RequestId,
    name: &str,
    question: &str,
    state: &DaemonState,
) -> Response {
    // Post question to channel
    let msg = crate::message::Message::text(name, format!("Question for Lead: {}", question));
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        error!("Failed to post question to channel: {}", e);
    }

    // Nudge the Lead about the question.
    let nudge_message = format!("{} is asking: {}", name, question);
    state.nudge_lead(&nudge_message).await;

    info!("Coworker {} asking: {}", name, question);
    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!("Notified Lead about question from {}", name),
        }),
    )
}

// ============================================================================
// Helper functions
// ============================================================================

/// Check if a task has an associated open PR.
///
/// Returns true if any open PR has this task_id stored in its PrAuthorSession.
/// This mapping is established when a coworker opens a PR - the daemon extracts
/// the task ID from the PR title's "[Midtown !XXX]" marker and stores it in
/// persistent state.
///
/// Presence in `pr_author_sessions` implies the PR is still open — closed PRs
/// are cleaned up by `cleanup_closed_pr_state`.
///
/// Used to decide whether to auto-complete a task when a coworker reports
/// WorkflowPhase::Completed. Tasks with open PRs should complete on merge,
/// not on phase transition.
async fn task_has_open_pr(task_id: &str, state: &DaemonState) -> bool {
    let ps = state.persistent_state.lock().await;
    ps.github
        .pr_author_sessions
        .values()
        .any(|session| session.task_id.as_deref() == Some(task_id))
}
