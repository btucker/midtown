//! Coworker lifecycle RPC handlers.
//!
//! Handles `coworker.*` methods: spawn, break, list, view, report-state,
//! nudge, and asking. Also handles the `coworkers.status` method which
//! returns live in-memory coworker state for the TUI at 1-2s poll intervals.

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
    let channel_lead_names = {
        let ps = state.persistent_state.lock().await;
        ps.channel_lead_names()
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

    // Pick a name for the coworker (excluding channel lead names to prevent collision)
    let name = match state
        .coworkers
        .next_available_name_excluding(&channel_lead_names)
    {
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
        model: super::helpers::resolve_model_for_role(
            &state.repo_name,
            provider,
            &crate::launch::CoworkerRole::Coworker,
        ),
        channel: None,
        auth_profile_dir: None,
        auth_provider: provider, // Resolved by spawn_coworker()
        persisted_initial_prompt: None,
    };

    // Spawn via the headless path (creates worktree + headless session)
    match state.spawn_coworker(&config).await {
        Ok(_) => {
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
    // Idempotent: if lead is already running, return success.
    // Project lead session name is repo-based ("midtown"), with legacy "lead"
    // retained only for backward compatibility.
    if state.session_manager.is_alive(&state.repo_name).await
        || state.session_manager.is_alive("lead").await
    {
        return Response::success(
            id,
            serde_json::json!({
                "success": true,
                "message": "Lead already running",
            }),
        );
    }

    let mut config = crate::launch::LaunchConfig::lead(&state.repo_name, None);
    config.auth_provider = provider;
    config.model = super::helpers::resolve_model_for_role(&state.repo_name, provider, &config.role);

    // Use the canonical lead worktree path so spawn_coworker uses it
    // instead of falling through to the legacy coworker-named path.
    let lead_wt = crate::paths::lead_worktree_path(&state.repo_name);
    if lead_wt.exists() {
        config.working_dir = Some(lead_wt);
    }

    match state.spawn_coworker(&config).await {
        Ok(_) => {
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
    // Clean up all transient coworker state through the centralized path.
    // This handles: deregistration, stop-time, coworker_records, cooldowns,
    // pending nudges, task assignments, recent_tool_items, NamePool release,
    // session reverse maps, SessionRecord update, and pending_questions.
    // Note: we intentionally do NOT unbind the worktree here — break preserves
    // the worktree for potential resumption.
    state.cleanup_coworker_state(name).await;

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
pub(super) async fn handle_coworker_list(id: RequestId, state: &DaemonState) -> Response {
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

    let channel_lead_names = {
        let ps = state.persistent_state.lock().await;
        ps.channel_lead_names()
    };

    let coworkers: Vec<serde_json::Value> = state
        .coworkers
        .list()
        .iter()
        .filter(|cw| !super::rpc_kanban::is_project_lead(&cw.name, &state.repo_name))
        .map(|cw| {
            // Look up current task from task storage (case-insensitive)
            let current_task = coworker_tasks.get(&cw.name.to_lowercase()).cloned();
            let is_channel_lead = channel_lead_names.contains(&cw.name);
            serde_json::json!({
                "name": cw.name,
                "status": cw.status.to_string(),
                "current_task": current_task,
                "started_at": cw.started_at.to_rfc3339(),
                "provider": cw.provider.as_str(),
                "profile": cw.profile,
                "is_channel_lead": is_channel_lead,
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
/// When `pr_number` is provided, writes it to `task.pr` so the daemon can
/// auto-complete the task when the PR merges.
pub(super) async fn handle_coworker_report_state(
    id: RequestId,
    name: &str,
    phase_str: &str,
    task_id: Option<u32>,
    progress: Option<u8>,
    pr_number: Option<u64>,
    state: &DaemonState,
) -> Response {
    // Parse the phase string via FromStr (implemented in coworker_state.rs)
    let phase: crate::coworker_state::WorkflowPhase = match phase_str.parse() {
        Ok(p) => p,
        Err(e) => return Response::error(id, RpcError::new(-32602, e)),
    };

    // If --pr was provided, write it to task.pr so the daemon can auto-complete
    // the task when the PR merges (the merge handler checks task.pr against merged PR numbers).
    if let Some(pr_num) = pr_number {
        let effective_task_id: Option<String> = task_id.map(|id| id.to_string()).or_else(|| {
            let assignments = state.coworker_task_assignments.lock().unwrap();
            assignments
                .get(&name.to_lowercase())
                .map(|a| a.task_id.clone())
        });

        if let Some(ref tid) = effective_task_id {
            if let Err(e) = crate::tasks::update_task_fields_for_repo(
                tid,
                &state.repo_name,
                None,
                None,
                None,
                None,
                None,
                Some(pr_num),
            ) {
                warn!(
                    "Failed to write pr_number {} to task {}: {}",
                    pr_num, tid, e
                );
            } else {
                info!("Set task !{} pr={} (reported by {})", tid, pr_num, name);
            }
        } else {
            warn!(
                "Coworker {} reported pr_number {} but has no task assignment to update",
                name, pr_num
            );
        }
    }

    // For Idle phase, immediately send the coworker on break.
    if phase == crate::coworker_state::WorkflowPhase::Idle && state.coworkers.get(name).is_some() {
        // Project lead must remain available for user interaction; ignore idle
        // self-reports instead of sending it on break.
        if super::rpc_kanban::is_project_lead(name, &state.repo_name) {
            info!(
                "Project lead {} reported idle; keeping lead session active",
                name
            );
            return Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("{} remains active (project lead)", name),
                }),
            );
        }

        // Before shutting down, check if this coworker is an active reviewer who hasn't
        // posted their review yet. If so, nudge them to post the review first instead of
        // going idle. This prevents the case where a reviewer calls `midtown state idle`
        // before completing their review (e.g., thinking they're done but forgot to post).
        //
        // Use `is_pr_reviewed()` instead of the snapshot's `reviewed_prs` cache so that
        // a fresh GitHub API check is made when the persistent cache has no record yet.
        // Without this, a reviewer who posts their review and immediately goes idle can
        // get stuck in a nudge loop: the webhook marking the review as cached hasn't
        // arrived yet, the poll tick hasn't run, so the snapshot says "not reviewed"
        // even though the review exists on GitHub. (Bug fix for !1990)
        let pre_snap = snapshot::collect_world_snapshot(state).await;
        if let Some(&pr_number) = pre_snap.reviewer.reviewer_pr_assignments.get(name)
            && !state.is_pr_reviewed(pr_number).await
        {
            warn!(
                "Reviewer {} reported idle but has not posted review for PR #{} — nudging to post first",
                name, pr_number
            );
            let nudge_effects = vec![effects::Effect::nudge_session(
                state.session_id_for_name(name),
                format!(
                    "You are assigned as reviewer for PR #{pr_number} but have not posted \
                     your review yet. Please complete and post your review comment on the PR \
                     before going idle."
                ),
            )];
            effects::execute_effects(nudge_effects, state).await;
            return Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("{} nudged to post review for PR #{}", name, pr_number),
                }),
            );
        }

        let shutdown_effects = vec![effects::Effect::ShutdownCoworkerWithCallbacks {
            name: name.to_string(),
            message: String::new(),
            on_success: vec![
                effects::Effect::PostSystemMessage {
                    message: format!("☕ {} reported idle, taking a break", name),
                    channel: Some(OPS_CHANNEL.to_string()),
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
                // No open PR — complete the task directly.
                // This handles legitimate no-PR tasks (release management, ops,
                // investigations) without entering a respawn loop (!1879).
                // Previously, the daemon nudged "open a PR first" and cleared
                // the assignment but left the task in_progress, causing
                // dispatch_via_sessions to repeatedly respawn the coworker.
                info!(
                    "Task !{} reported completed by {} with no PR — completing directly",
                    tid, name
                );
                match crate::tasks::complete_task_for_repo(tid, &state.repo_name) {
                    Err(e) => {
                        warn!("Failed to complete task !{}: {}", tid, e);
                        // Don't proceed with downstream cleanup (blocked_by,
                        // worktree, channel post) — the task is still in_progress
                        // on disk and the coworker will be respawned to retry.
                    }
                    Ok(()) => {
                        if let Err(e) =
                            crate::tasks::clear_blocked_by_for_repo(tid, &state.repo_name)
                        {
                            warn!("Failed to clear blockedBy for task !{}: {}", tid, e);
                        }
                        // Mark worktree as completed (for time-based cleanup)
                        {
                            let mut ps = state.persistent_state.lock().await;
                            if let Some(wt_id) = ps.worktree_registry.find_worktree_by_task(tid) {
                                ps.worktree_registry
                                    .mark_completed(&wt_id, chrono::Utc::now());
                                if let Err(e) = ps.save_for_repo(&state.repo_name) {
                                    warn!("Failed to save worktree completion timestamp: {}", e);
                                }
                            }
                        }
                        let completion_effects = vec![effects::Effect::PostToChannel {
                            sender: "midtown".to_string(),
                            message: format!("✅ Task !{} completed by {} (no PR)", tid, name),
                            channel: Some(OPS_CHANNEL.to_string()),
                        }];
                        effects::execute_effects(completion_effects, state).await;
                    }
                }
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

    // Clear pending questions AFTER delivery — the nudge has been enqueued (headed)
    // and best-effort delivered (headless). Clearing after ensures we don't lose the
    // question if enqueue_headed_nudge were to fail in the future.
    {
        let mut questions = state.pending_questions.lock().unwrap();
        questions.retain(|q| q.coworker_name != name);
    }

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

    // Assign a unique ID and store the question in pending state.
    // Replace any existing question from the same coworker (only one active question per coworker).
    let question_id = state
        .pending_question_id_counter
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let timestamp = chrono::Utc::now();
    {
        let mut questions = state.pending_questions.lock().unwrap();
        questions.retain(|q| q.coworker_name != name);
        questions.push(super::PendingQuestion {
            id: question_id,
            coworker_name: name.to_string(),
            question: question.to_string(),
            timestamp,
        });
    }

    // Broadcast the pending question to WebSocket clients (e.g., TUI).
    state.broadcast_web_update(crate::web::WebUpdate::CoworkerQuestion(
        crate::web::CoworkerQuestionData {
            id: question_id,
            coworker_name: name.to_string(),
            question: question.to_string(),
            timestamp: timestamp.to_rfc3339(),
        },
    ));

    info!("Coworker {} asking: {}", name, question);
    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!("Notified Lead about question from {}", name),
        }),
    )
}

/// Handle coworker.questions RPC method.
///
/// Returns the list of pending questions from coworkers waiting for Lead input.
/// Used by the TUI to display unanswered questions that need attention.
pub(super) async fn handle_coworker_questions(id: RequestId, state: &DaemonState) -> Response {
    let questions: Vec<serde_json::Value> = {
        let questions = state.pending_questions.lock().unwrap();
        questions
            .iter()
            .map(|q| {
                serde_json::json!({
                    "id": q.id,
                    "coworker_name": q.coworker_name,
                    "question": q.question,
                    "timestamp": q.timestamp.to_rfc3339(),
                })
            })
            .collect()
    };
    Response::success(id, serde_json::json!({ "questions": questions }))
}

// ============================================================================
// Helper functions
// ============================================================================

/// Check if a task has an associated open PR.
///
/// Returns true if the task has an associated open PR, checking two sources:
///
/// 1. **`pr_author_sessions` (in-memory persistent state)**: Presence implies
///    the PR is still open — closed PRs are cleaned up by `cleanup_closed_prs`.
///    This mapping is established when a coworker opens a PR and the daemon
///    extracts the task ID from the PR title's `[Midtown !XXX]` marker.
///
/// 2. **`task.pr` field on disk + GitHub API verification**: The task file may
///    have an explicit PR number set via `--pr` or auto-detected. This survives
///    daemon restarts (unlike `pr_author_sessions` which is rebuilt over time).
///    However, `task.pr` is never cleared when a PR is closed, so we verify the
///    PR is actually open via `gh pr view` before trusting it.
///
/// Used to decide completion strategy when a coworker reports
/// `WorkflowPhase::Completed`:
/// - Tasks WITH open PRs defer completion to the merge path (auto-complete on merge).
/// - Tasks WITHOUT open PRs are completed directly to avoid the respawn loop (!1879).
async fn task_has_open_pr(task_id: &str, state: &DaemonState) -> bool {
    // Source 1: in-memory pr_author_sessions
    let in_memory = {
        let ps = state.persistent_state.lock().await;
        ps.github
            .pr_author_sessions
            .values()
            .any(|session| session.task_id.as_deref() == Some(task_id))
    };
    if in_memory {
        return true;
    }

    // Source 2: task.pr field on disk (survives daemon restarts)
    // Must verify via GitHub API since task.pr is never cleared on PR close.
    if let Some(task) = crate::tasks::read_task_for_repo(task_id, &state.repo_name)
        && let Some(pr_num) = task.pr
    {
        let repo_path = state.all_repo_paths.first().cloned();
        let is_open = tokio::task::spawn_blocking(move || is_pr_open(pr_num, repo_path.as_deref()))
            .await
            .unwrap_or(false);

        if is_open {
            debug!(
                "Task !{} has pr={} on disk — verified open via GitHub",
                task_id, pr_num
            );
            return true;
        } else {
            debug!(
                "Task !{} has pr={} on disk but PR is not open — ignoring",
                task_id, pr_num
            );
        }
    }

    false
}

/// Check if a specific PR is open by querying GitHub.
///
/// Returns `true` only if the PR state is "OPEN". Returns `false` for
/// closed, merged, or if the API call fails (conservative: treat failures
/// as "not open" so the task can be completed directly rather than
/// getting stuck in the deferred merge path for a stale PR).
fn is_pr_open(pr_number: u64, repo_path: Option<&std::path::Path>) -> bool {
    let mut cmd = std::process::Command::new("gh");
    if let Some(path) = repo_path {
        cmd.current_dir(path);
    }
    cmd.args([
        "pr",
        "view",
        &pr_number.to_string(),
        "--json",
        "state",
        "--jq",
        ".state",
    ]);

    match cmd.output() {
        Ok(output) if output.status.success() => {
            let state = String::from_utf8_lossy(&output.stdout);
            state.trim() == "OPEN"
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            debug!(
                "Failed to check PR #{} state via gh CLI: {}",
                pr_number,
                stderr.trim()
            );
            false
        }
        Err(e) => {
            warn!("Failed to execute gh pr view for PR #{}: {}", pr_number, e);
            false
        }
    }
}

// ============================================================================
// coworkers.status handler
// ============================================================================

/// Handle `coworkers.status` RPC method.
///
/// Returns live in-memory coworker state — no GraphQL, no cache.
/// Delegates to `rpc_kanban::build_coworkers_data` for the actual data
/// assembly, then adds lead activity and tool activity on top.
///
/// Response fields:
/// - `coworkers`: active non-idle coworkers with phase, task_id, pr_number, health, etc.
/// - `max_coworkers`: configured coworker limit
/// - `lead_working`: whether the headless lead session is actively computing
/// - `tool_activity`: recent tool call/result items per agent
/// - `channel_leads`: names of active channel lead sessions
pub(crate) async fn handle_coworkers_status(id: RequestId, state: &DaemonState) -> Response {
    super::rpc_kanban::handle_coworkers_status(id, state).await
}

#[path = "rpc_coworker_tests.rs"]
#[cfg(test)]
mod tests;
