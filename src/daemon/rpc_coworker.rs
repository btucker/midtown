//! Coworker lifecycle RPC handlers.
//!
//! Handles `coworker.*` methods: spawn, break, list, view, report-state,
//! nudge, and asking. Also handles the `coworkers.status` method which
//! returns live in-memory coworker state for the TUI at 1-2s poll intervals.

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use tracing::{debug, error, info, warn};

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;
use super::constants::*;
use super::snapshot::ProcessHealth;
use super::{effects, snapshot};

// ============================================================================
// Handlers
// ============================================================================

/// Handle coworker.spawn RPC method.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_coworker_spawn(
    id: RequestId,
    state: &DaemonState,
    resume: bool,
    prompt: Option<String>,
    provider: crate::auth::AuthProvider,
    agent: Option<String>,
    channel: Option<String>,
    thread: Option<String>,
    task_id: Option<String>,
) -> Response {
    // Check dev coworkers limit (reserve slots for reviewers)
    let channel_lead_names = {
        let ps = state.persistent_state.lock().await;
        ps.channel_lead_names()
    };
    if state.is_at_task_limit() {
        return Response::error(
            id,
            RpcError::new(
                -32603,
                format!(
                    "In-progress task limit ({}) reached. Adjust with MIDTOWN_MAX_IN_PROGRESS_TASKS or max_in_progress_tasks in config.toml",
                    state.max_in_progress_tasks
                ),
            ),
        );
    }

    // Generate a name for the coworker. If a task_id is provided, derive from task;
    // otherwise generate a random worker name. Exclude active session names to prevent collisions.
    let mut excluded_names = channel_lead_names;
    for active_name in state.session_manager.list_names().await {
        if state.session_manager.is_alive(&active_name).await {
            excluded_names.insert(active_name.to_lowercase());
        }
    }
    let name = if let Some(ref tid) = task_id {
        // Derive name from task ID
        let candidate = format!("task-{}", tid).to_lowercase();
        if excluded_names.contains(&candidate) {
            format!("{}-{}", candidate, fastrand::u32(1000..9999))
        } else {
            candidate
        }
    } else {
        // No task — generate a generic worker name
        format!("worker-{}", fastrand::u32(1000..9999))
    };

    // Load agent definition if --agent was provided
    let agent_def = if let Some(ref agent_name) = agent {
        match crate::agent_definition::load_agent_definition(agent_name) {
            Ok(def) => {
                info!(
                    "Loaded agent definition '{}' from {}",
                    def.name,
                    def.source_path.display()
                );
                Some(def)
            }
            Err(e) => {
                return Response::error(id, RpcError::new(-32602, e));
            }
        }
    } else {
        None
    };

    // Validate and load the task if --task was provided
    let task = if let Some(ref tid) = task_id {
        match state.task_store.load(tid).ok() {
            Some(t) => {
                if t.status == crate::task_store::TaskStatus::Completed {
                    return Response::error(
                        id,
                        RpcError::new(-32602, format!("Task !{} is already completed", tid)),
                    );
                }
                if t.status == crate::task_store::TaskStatus::InProgress {
                    return Response::error(
                        id,
                        RpcError::new(
                            -32602,
                            format!(
                                "Task !{} is already in progress (owner: {})",
                                tid,
                                if t.agent_name.is_empty() {
                                    "unknown"
                                } else {
                                    &t.agent_name
                                }
                            ),
                        ),
                    );
                }
                Some(t)
            }
            None => {
                return Response::error(
                    id,
                    RpcError::new(-32602, format!("Task !{} not found", tid)),
                );
            }
        }
    } else {
        None
    };

    // Build initial prompt, combining agent instructions, user prompt, and task prompt.
    // Priority: agent instructions wrap everything, user --prompt is preserved, task
    // prompt is appended when --task is provided.
    let task_prompt = if let Some(ref t) = task {
        // Read plan/execution-skill data. Try TaskStore first, then persistent state.
        let plan_section = {
            let plan_path = state
                .task_store
                .load(&t.id)
                .ok()
                .and_then(|st| st.plan.clone());
            super::dispatch::build_plan_prompt_section_from_parts(
                &t.id,
                plan_path.as_deref(),
                None, // execution_skill dropped
            )
        };
        Some(crate::agents::coworker_task_prompt(
            &t.id,
            &t.subject,
            &plan_section,
            false,
        ))
    } else {
        None
    };
    let base_prompt = match (&prompt, &task_prompt) {
        (Some(p), Some(tp)) => Some(format!("{}\n\n{}", p, tp)),
        (Some(p), None) => Some(p.clone()),
        (None, Some(tp)) => Some(tp.clone()),
        (None, None) => None,
    };
    let effective_prompt = match (&agent_def, &base_prompt) {
        (Some(def), Some(bp)) => Some(format!(
            "## Agent Instructions\n\n{}\n\n---\n\n{}",
            def.system_prompt, bp
        )),
        (Some(def), None) => Some(format!("## Agent Instructions\n\n{}", def.system_prompt)),
        (None, bp) => bp.clone(),
    };

    // Resolve auth_provider: if the agent definition specifies a model,
    // infer the correct provider from the model alias (e.g., "opus" → Claude).
    // Without this, spawn_coworker() silently normalizes the model to match
    // the passed-in provider, defeating the agent definition's model intent.
    // (Same pattern as PR #1988 fix for reviewer resume.)
    let effective_provider = agent_def
        .as_ref()
        .and_then(|d| d.model.as_deref())
        .and_then(super::helpers::provider_for_model_alias)
        .unwrap_or(provider);

    // When --task is provided, set up the task worktree and resolve channel from task
    let effective_channel = channel
        .clone()
        .or_else(|| task.as_ref().and_then(|t| t.channel.clone()));
    let task_worktree = task.as_ref().map(|t| {
        let worktree_id = crate::worktree_registry::branch_slug_for_task(&t.id, &t.subject);
        let path = crate::paths::worktrees_dir_for_repo(state.paths.dir_key()).join(&worktree_id);
        (worktree_id, path)
    });

    // Build headless launch config
    let session_mode = if resume {
        crate::launch::SessionMode::Resume
    } else {
        crate::launch::SessionMode::Fresh
    };
    let mut config = crate::launch::LaunchConfig::new(
        name,
        "midtown-code-author",
        state.paths.dir_key(),
        effective_prompt,
        None,
    )
    .with_session_mode(session_mode)
    .with_working_dir(task_worktree.as_ref().map(|(_, path)| path.clone()))
    .with_channel(effective_channel.clone())
    .with_auth_provider(effective_provider)
    .with_task_id(task_id.clone());
    if let Some(m) = agent_def.as_ref().and_then(|d| d.model.clone()) {
        config.model = m;
    }
    if let Some(badge) = agent_def.as_ref().and_then(|d| d.avatar_badge.clone()) {
        config.avatar_badge = Some(badge);
    }

    // Pre-spawn: ensure task worktree exists and register assignment
    if let Some((ref worktree_id, ref path)) = task_worktree {
        let ensure_effect = effects::Effect::EnsureWorktree {
            worktree_id: worktree_id.clone(),
            path: path.clone(),
        };
        effects::execute_effects(vec![ensure_effect], state).await;

        // Register worktree assignment if not already tracked
        let needs_registration = {
            let ps = state.persistent_state.lock().await;
            ps.worktree_registry
                .find_worktree_by_task(task.as_ref().unwrap().id.as_str())
                .is_none()
        };
        if needs_registration {
            let register_effect = effects::Effect::RegisterWorktreeAssignment {
                assignment: crate::worktree_registry::WorktreeAssignment {
                    worktree_id: worktree_id.clone(),
                    branch_name: worktree_id.clone(),
                    task_id: task_id.clone(),
                    current_coworker: None,
                    pr_number: None,
                    created_at: Utc::now(),
                    completed_at: None,
                },
            };
            effects::execute_effects(vec![register_effect], state).await;
        }
    }

    // Spawn via the headless path (creates worktree + headless session)
    match state.spawn_coworker(&config).await {
        Ok(_) => {
            info!("Spawned coworker: {}", config.name);
            state.broadcast_coworker_update(
                &config.name,
                "running",
                None,
                None,
                None,
                config.avatar_badge.as_deref(),
            );

            // If --task was provided, execute task assignment effects and update
            // the task file on disk (same as dispatch + SpawnForTask do)
            if let (Some(tid), Some((worktree_id, _))) = (&task_id, &task_worktree) {
                // Update task file on disk: set owner and transition to in_progress
                if let Err(e) = state.task_store.set_agent_name(tid, &config.name) {
                    warn!(
                        "Failed to set task !{} owner to {} after spawn: {}",
                        tid, config.name, e
                    );
                }
                if let Err(e) = state.task_store.set_task_in_progress(tid) {
                    warn!(
                        "Failed to set task !{} to in_progress after spawn: {}",
                        tid, e
                    );
                }

                let task_effects = vec![
                    effects::Effect::RecordTaskAssignment {
                        coworker: config.name.clone(),
                        task_id: tid.clone(),
                    },
                    effects::Effect::BindCoworkerToWorktree {
                        worktree_id: worktree_id.clone(),
                        coworker: config.name.clone(),
                    },
                    effects::Effect::BroadcastCoworkerUpdate {
                        name: config.name.clone(),
                        status: "running".to_string(),
                        current_task: None,
                        color: None,
                        icon: None,
                        avatar_badge: None,
                    },
                    effects::Effect::post_to_ops(format!(
                        "Called in coworker {} for task !{}",
                        config.name, tid
                    )),
                ];
                effects::execute_effects(task_effects, state).await;
            }

            // Register thread binding so the coworker's channel posts
            // are automatically routed to the specified thread.
            // Note: DM channels skip bound_thread_id (see rpc_channel.rs and effects.rs),
            // so thread binding is silently ignored for dm-* channels.
            if let Some(ref tid) = thread {
                if channel.as_deref().is_some_and(|c| c.starts_with("dm-")) {
                    warn!(
                        "Thread binding for {} ignored: DM channels do not use bound_thread_id",
                        config.name
                    );
                }

                // Persist to SessionRecord — the single source of truth.
                // spawn_coworker() resolves bound_thread_id from task metadata, but
                // call-in with --thread has no task — we set it directly here.
                {
                    let mut ps = state.persistent_state.lock().await;
                    if let Some(record) = ps.sessions.values_mut().find(|r| r.name == config.name) {
                        record.bound_thread_id = Some(tid.clone());
                    }
                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                        warn!("Failed to save bound_thread_id for {}: {}", config.name, e);
                    }
                }
            }

            let task_display = task_id
                .as_ref()
                .map(|tid| format!(" for task !{}", tid))
                .unwrap_or_default();
            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Called in coworker: {}{}", config.name, task_display),
                    "coworkers": [{
                        "name": config.name,
                        "status": "running",
                        "current_task": task_id,
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
    if state.session_manager.is_alive(&state.project_name).await
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

    let mut config = crate::launch::LaunchConfig::lead(state.paths.dir_key(), None);
    config.auth_provider = provider;
    config.model =
        super::helpers::resolve_model_for_role(state.paths.dir_key(), provider, &config.agent_type);

    // Use the canonical lead worktree path so spawn_coworker uses it
    // instead of falling through to the legacy coworker-named path.
    let lead_wt = state.paths.lead_worktree();
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
    {
        let mut ps = state.persistent_state.lock().await;
        ps.clear_reviewer_assignment(name, state.paths.dir_key());
    }

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

    state.broadcast_coworker_update(name, "stopped", None, None, None, None);

    // Shut down the headless session, then deregister from tracking
    if let Err(e) = state.session_manager.shutdown(name).await {
        warn!("Failed to shut down headless session for {}: {}", name, e);
    }
    // Clean up all transient coworker state through the centralized path.
    // This handles: deregistration, stop-time, coworker_records, cooldowns,
    // pending nudges, task assignments,
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
    let coworker_tasks: std::collections::HashMap<String, String> = state
        .task_store
        .load_all()
        .into_iter()
        .filter(|t| {
            t.status == crate::task_store::TaskStatus::InProgress && !t.agent_name.is_empty()
        })
        .map(|t| {
            let task_display = format!("!{} {}", t.id, t.subject);
            (t.agent_name.to_lowercase(), task_display)
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
        .filter(|cw| !super::helpers::is_project_lead(&cw.name, &state.project_name))
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
    // Parse the phase string via FromStr (implemented in workflow_phase.rs)
    let phase: crate::workflow_phase::WorkflowPhase = match phase_str.parse() {
        Ok(p) => p,
        Err(e) => return Response::error(id, RpcError::new(-32602, e)),
    };

    // If --pr was provided, write it to task.pr so the daemon can auto-complete
    // the task when the PR merges (the merge handler checks task.pr against merged PR numbers).
    if let Some(pr_num) = pr_number {
        let effective_task_id: Option<String> = match task_id {
            Some(id) => Some(id.to_string()),
            None => state.get_task_id_for_coworker(name).await,
        };

        if let Some(ref tid) = effective_task_id {
            if let Err(e) =
                state
                    .task_store
                    .update_task_fields(tid, None, None, None, None, None, Some(pr_num))
            {
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
    if phase == crate::workflow_phase::WorkflowPhase::Idle && state.coworkers.get(name).is_some() {
        // Project lead must remain available for user interaction; ignore idle
        // self-reports instead of sending it on break.
        if super::helpers::is_project_lead(name, &state.project_name) {
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
        // a GitHub API check can be made when the persistent cache has no record yet.
        // Without this, a reviewer who posts their review and immediately goes idle can
        // get stuck in a nudge loop: the webhook marking the review as cached hasn't
        // arrived yet, the poll tick hasn't run, so the snapshot says "not reviewed"
        // even though the review exists on GitHub.
        //
        // Note: `is_pr_reviewed()` has a negative-result cache with a 2-minute TTL
        // (`PR_REVIEW_NEGATIVE_CACHE_SECS`). If a recent poll tick confirmed no review,
        // the API call is skipped within that window. This is acceptable: the negative
        // cache only populates during poll ticks, and the common nudge-loop scenario
        // (reviewer posts then immediately idles) happens before any poll tick runs,
        // so the negative cache is empty. (Bug fix for !1990)
        let reviewer_pr = {
            let ps = state.persistent_state.lock().await;
            snapshot::build_reviewer_pr_assignments_from_spans(&ps)
                .get(name)
                .copied()
        };
        if let Some(pr_number) = reviewer_pr
            && !state.is_pr_reviewed(pr_number).await
        {
            warn!(
                "Reviewer {} reported idle but has not posted review for PR #{} — nudging to post first",
                name, pr_number
            );
            let nudge_effects = vec![effects::Effect::nudge_session(
                state.session_id_for_name(name).await,
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
                    color: None,
                    icon: None,
                    avatar_badge: None,
                },
            ],
        }];

        effects::execute_effects(shutdown_effects, state).await;

        // Immediately trigger task dispatch so pending tasks get picked up
        let ps = state.persistent_state.lock().await;
        let tasks = state.task_store.load_all();
        let pending_effects = super::dispatch::spawn_for_pending_tasks(&ps, &tasks, state);
        drop(ps);
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
    if phase == crate::workflow_phase::WorkflowPhase::Completed {
        let effective_task_id: Option<String> = match task_id {
            Some(id) => Some(id.to_string()),
            None => state.get_task_id_for_coworker(name).await,
        };

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
                match state.task_store.complete_task(tid) {
                    Err(e) => {
                        warn!("Failed to complete task !{}: {}", tid, e);
                        // Don't proceed with downstream cleanup (blocked_by,
                        // worktree, channel post) — the task is still in_progress
                        // on disk and the coworker will be respawned to retry.
                    }
                    Ok(()) => {
                        if let Err(e) = state.task_store.clear_blocked_by(tid) {
                            warn!("Failed to clear blockedBy for task !{}: {}", tid, e);
                        }
                        // Mark worktree as completed (for time-based cleanup)
                        {
                            let mut ps = state.persistent_state.lock().await;
                            if let Some(wt_id) = ps.worktree_registry.find_worktree_by_task(tid) {
                                ps.worktree_registry
                                    .mark_completed(&wt_id, chrono::Utc::now());
                                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                                    warn!("Failed to save worktree completion timestamp: {}", e);
                                }
                            }
                        }
                        let completion_effects = vec![effects::Effect::post_to_ops(format!(
                            "✅ Task !{} completed by {} (no PR)",
                            tid, name
                        ))];
                        effects::execute_effects(completion_effects, state).await;
                    }
                }
            }
        }

        // Clear the task assignment — use clear_task_assignment_by_task when we
        // have a task_id so it defensively clears ALL session records that might
        // reference this task (e.g., stale records from crash recovery).
        if let Some(ref tid) = effective_task_id {
            state.clear_task_assignment_by_task(tid).await;
        } else {
            // No task_id known — clear this coworker's session record directly
            let mut ps = state.persistent_state.lock().await;
            if let Some(record) = ps.session_by_name_mut(&name.to_lowercase()) {
                record.task_id = None;
            }
            if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                warn!(
                    "Failed to save state after clearing coworker assignment for {}: {}",
                    name, e
                );
            }
        }
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
    from: &str,
    name: &str,
    message: &str,
    state: &DaemonState,
) -> Response {
    // Deliver via core nudge function (send_message + DM post + attribution).
    let delivered = if let Ok(follow_up) =
        super::effects::deliver_coworker_nudge(state, name, message, "manual_nudge", from).await
    {
        super::effects::execute_effects(follow_up, state).await;
        true
    } else {
        false
    };

    // Clear pending questions after delivery.
    {
        let mut questions = state.pending_questions.lock().unwrap();
        questions.retain(|q| q.coworker_name != name);
    }

    info!("Nudge for coworker {}: {}", name, message);
    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!("Nudged coworker: {}", name),
            "delivered_headless": delivered
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
/// 1. **`SessionRecord.pr_number` (primary — task-centric model)**: Checks if
///    any session for this task has a `pr_number` set.
///
/// 2. **`task.pr` field on disk + GitHub API verification**: The task file may
///    have an explicit PR number set via `--pr` or auto-detected. This survives
///    daemon restarts (unlike in-memory state which is rebuilt over time).
///    However, `task.pr` is never cleared when a PR is closed, so we verify the
///    PR is actually open via `gh pr view` before trusting it.
///
/// Used to decide completion strategy when a coworker reports
/// `WorkflowPhase::Completed`:
/// - Tasks WITH open PRs defer completion to the merge path (auto-complete on merge).
/// - Tasks WITHOUT open PRs are completed directly to avoid the respawn loop (!1879).
async fn task_has_open_pr(task_id: &str, state: &DaemonState) -> bool {
    // Source 1: SessionRecord (primary — task-centric model)
    let in_memory = {
        let ps = state.persistent_state.lock().await;
        ps.sessions
            .values()
            .any(|s| s.task_id.as_deref() == Some(task_id) && s.pr_number.is_some())
    };
    if in_memory {
        return true;
    }

    // Source 2: task.pr field on disk (survives daemon restarts)
    // Must verify via GitHub API since task.pr is never cleared on PR close.
    if let Some(task) = state.task_store.load(task_id).ok()
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

/// Handle `coworkers.status` RPC method — returns live coworker state.
///
/// This is a lightweight endpoint with no GraphQL queries and no caching.
/// It reads directly from in-memory daemon state so responses are always
/// current (microsecond latency). The TUI polls this at 1–2s to keep the
/// coworker status panel up-to-date without delay.
///
/// Returns: coworkers, max_in_progress_tasks, lead_working, tool_activity,
///          channel_leads, channel_leads_working.
pub(crate) async fn handle_coworkers_status(id: RequestId, state: &DaemonState) -> Response {
    let (coworkers_data, channel_lead_names) = build_coworkers_data(state).await;

    // Read health once for both main-lead and per-channel-lead activity checks
    let health_guard = state.headless_health.read().unwrap();
    let lead_working = is_lead_health_active(&health_guard, &state.project_name);
    let channel_leads_working = build_channel_leads_working(&health_guard, &channel_lead_names);
    drop(health_guard);

    let tool_activity = collect_tool_activity(state);
    let channel_leads: Vec<&String> = channel_lead_names.iter().collect();

    Response::success(
        id,
        serde_json::json!({
            "coworkers": coworkers_data,
            "max_in_progress_tasks": state.max_in_progress_tasks,
            "lead_working": lead_working,
            "tool_activity": tool_activity,
            "channel_leads": channel_leads,
            "channel_leads_working": channel_leads_working,
        }),
    )
}

// ============================================================================
// coworkers.status helpers
// ============================================================================

/// Build the coworker data array from daemon state.
///
/// Returns `(coworkers_data, channel_lead_names)`.
async fn build_coworkers_data(
    state: &DaemonState,
) -> (Vec<serde_json::Value>, std::collections::HashSet<String>) {
    // Get reviewer assignments, worktree registry, and channel lead names from persistent state
    // (best-effort via try_lock)
    let (reviewer_pr_map, worktree_pr_map, channel_lead_names): (
        HashMap<String, u64>,
        HashMap<String, u64>,
        std::collections::HashSet<String>,
    ) = state
        .persistent_state
        .try_lock()
        .map(|ps| {
            // Build reviewer -> PR map from active spans
            let rev_map: HashMap<String, u64> = ps
                .active_reviewer_sessions()
                .into_iter()
                .filter_map(|s| s.pr_number.map(|pr| (s.name.clone(), pr)))
                .collect();
            // Build coworker -> PR map from worktree registry (for reviewers)
            let wt_map: HashMap<String, u64> = ps
                .worktree_registry
                .all_assignments()
                .iter()
                .filter_map(|(_, assignment)| {
                    let coworker = assignment.current_coworker.as_ref()?;
                    let pr_number = assignment.pr_number?;
                    Some((coworker.clone(), pr_number))
                })
                .collect();
            let cl_names = ps.channel_lead_names();
            (rev_map, wt_map, cl_names)
        })
        .unwrap_or_default();

    // Extract avatar color/icon/badge overrides from session records
    struct AvatarOverrides {
        color: Option<String>,
        icon: Option<String>,
        avatar_badge: Option<String>,
    }
    let session_avatars: HashMap<String, AvatarOverrides> = state
        .persistent_state
        .try_lock()
        .map(|ps| {
            ps.sessions
                .values()
                .filter(|s| s.color.is_some() || s.icon.is_some() || s.avatar_badge.is_some())
                .map(|s| {
                    (
                        s.name.clone(),
                        AvatarOverrides {
                            color: s.color.clone(),
                            icon: s.icon.clone(),
                            avatar_badge: s.avatar_badge.clone(),
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let active_coworkers = state.coworkers.list();
    let coworker_records = state.coworker_records.read().await;

    // Read tasks to get explicit PR associations (task !1151)
    let all_tasks = state.task_store.load_all();
    let task_pr_map: HashMap<u32, u64> = all_tasks
        .iter()
        .filter_map(|task| {
            let task_id: u32 = task.id.parse().ok()?;
            let pr = task.pr?;
            Some((task_id, pr))
        })
        .collect();

    // Clone health data to avoid holding the lock across await
    let health_snapshot: HashMap<String, ProcessHealth> = {
        let health_guard = state.headless_health.read().unwrap();
        health_guard.clone()
    };

    let coworkers_data = active_coworkers
        .iter()
        .filter_map(|cw| {
            // Skip channel lead sessions — they are scoped to a specific topic
            // channel and must not appear in the general coworker status panel.
            // The lead session itself is also excluded: it uses either the legacy
            // "lead" name or the canonical repo name (e.g., "midtown").
            if is_channel_lead(&cw.name, &channel_lead_names)
                || super::helpers::is_project_lead(&cw.name, &state.project_name)
            {
                return None;
            }

            // Get coworker's workflow state from records
            let record = coworker_records.get(&cw.name);
            let workflow_phase = record.and_then(|r| r.workflow_phase);
            let task_id = record.and_then(|r| r.task_id);

            // Skip idle coworkers (phase = Idle or Completed)
            if matches!(
                workflow_phase,
                Some(crate::workflow_phase::WorkflowPhase::Idle)
                    | Some(crate::workflow_phase::WorkflowPhase::Completed)
            ) {
                return None;
            }

            // Get health status
            let health = health_snapshot.get(&cw.name);
            let health_color = if let Some(h) = health {
                if !h.is_alive {
                    "red" // dead
                } else if h.has_usage_limit || h.has_api_error {
                    "yellow" // degraded
                } else {
                    "green" // healthy
                }
            } else {
                "green" // default healthy
            };

            // Find PR number for this coworker, trying sources in priority order:
            // 1. Explicit task.pr field (task !1151) - most authoritative
            // 2. GitHub reviewer assignment (for review tasks)
            // 3. Worktree registry (for reviewers when reviewer_pr_map is empty)
            let pr_number = task_id
                .and_then(|tid| task_pr_map.get(&tid).copied())
                .or_else(|| reviewer_pr_map.get(&cw.name).copied())
                .or_else(|| worktree_pr_map.get(&cw.name).copied());

            let avatar = session_avatars.get(&cw.name);

            Some(serde_json::json!({
                "name": cw.name,
                "task_id": task_id,
                "phase": workflow_phase.map(|p| p.abbreviation()),
                "status": cw.status.to_string(),
                "pr_number": pr_number,
                "health": health_color,
                "provider": cw.provider.as_str(),
                "profile": cw.profile,
                "progress": record.and_then(|r| r.progress),
                "time_estimate": record.and_then(|r| r.format_time_remaining()),
                "color": avatar.and_then(|a| a.color.as_deref()),
                "icon": avatar.and_then(|a| a.icon.as_deref()),
                "avatar_badge": avatar.and_then(|a| a.avatar_badge.as_deref()),
            }))
        })
        .collect::<Vec<_>>();

    (coworkers_data, channel_lead_names)
}

/// Returns true if the coworker name identifies a channel lead session.
///
/// Channel leads are tracked in `DaemonPersistentState::channel_lead_sessions`.
/// They are scoped to a specific topic channel and must not appear in the
/// general coworker status list.
pub(crate) fn is_channel_lead(
    name: &str,
    channel_lead_names: &std::collections::HashSet<String>,
) -> bool {
    channel_lead_names.contains(name)
}

/// Timeout for considering the lead session "actively working".
///
/// If the last stream event from the lead session is older than this, the
/// lead is considered idle (waiting for user input, between turns, etc.).
const LEAD_ACTIVITY_TIMEOUT: Duration = Duration::from_secs(5);

/// Core lead-activity lookup: checks both the canonical repo-name key and the
/// legacy "lead" key, returning true if either session is actively working.
pub(crate) fn is_lead_health_active(
    health: &HashMap<String, ProcessHealth>,
    repo_name: &str,
) -> bool {
    // Check both keys: modern sessions use repo_name, legacy use "lead".
    // Either being active counts — handles stale entries or in-flight transitions.
    is_session_actively_working(health.get(repo_name))
        || is_session_actively_working(health.get("lead"))
}

/// Core logic for activity detection: returns `true` when a session is alive
/// and has received a stream event within `LEAD_ACTIVITY_TIMEOUT`.
fn is_session_actively_working(health: Option<&ProcessHealth>) -> bool {
    let Some(h) = health else {
        return false;
    };
    if !h.is_alive {
        return false;
    }
    h.last_event_at.is_some_and(|ts| {
        let elapsed = (Utc::now() - ts).num_seconds();
        elapsed >= 0 && elapsed < LEAD_ACTIVITY_TIMEOUT.as_secs() as i64
    })
}

/// Build a map of channel name → active-working boolean for all registered
/// channel leads, using the same `is_session_actively_working()` logic that
/// drives the main lead's `lead_working` flag.
///
/// Channel lead sessions are named after their channel (e.g., "web"), so
/// looking up `health.get(channel_name)` finds the right entry.
pub(crate) fn build_channel_leads_working(
    health: &HashMap<String, ProcessHealth>,
    channel_lead_names: &std::collections::HashSet<String>,
) -> serde_json::Map<String, serde_json::Value> {
    channel_lead_names
        .iter()
        .map(|name| {
            let active = is_session_actively_working(health.get(name.as_str()));
            (name.clone(), serde_json::Value::Bool(active))
        })
        .collect()
}

/// Collect pre-formatted tool activity headers per agent as a JSON value for the RPC response.
///
/// Returns a JSON object mapping agent name → array of header strings
/// (e.g. `{"lead": ["✓ read foo.rs", "› $ git status"]}`).
/// Sourced from `tool_activity_headers`, populated from `tool_data` on channel messages.
fn collect_tool_activity(state: &DaemonState) -> serde_json::Value {
    let headers_map = state.tool_activity_headers.read().unwrap();
    serialize_tool_activity_headers(&headers_map)
}

/// Serialize a tool activity headers map to a JSON object.
///
/// Separated from `collect_tool_activity` for testability without `DaemonState`.
fn serialize_tool_activity_headers(
    headers_map: &HashMap<String, Vec<String>>,
) -> serde_json::Value {
    let obj: serde_json::Map<String, serde_json::Value> = headers_map
        .iter()
        .map(|(agent, headers)| {
            let arr: Vec<serde_json::Value> = headers
                .iter()
                .map(|h| serde_json::Value::String(h.clone()))
                .collect();
            (agent.clone(), serde_json::Value::Array(arr))
        })
        .collect();
    serde_json::Value::Object(obj)
}

#[path = "rpc_coworker_tests.rs"]
#[cfg(test)]
mod tests;
