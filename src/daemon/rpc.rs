//! RPC request handlers for the daemon's Unix socket protocol.
//!
//! This module is the entry point for JSON-RPC request handling. It contains:
//! - Connection handling (reading from Unix socket, writing responses)
//! - Request dispatch (routing methods to handler functions)
//! - Coworker lifecycle handlers (spawn, break, list, view, nudge, report-state)
//! - Insight reporting
//!
//! Domain-specific handlers are delegated to sub-modules:
//! - `rpc_channel`: channel.post, channel.read, status, reminders
//! - `rpc_tasks`: task CRUD, claim, request
//! - `rpc_sessions`: session attach/detach/list
//! - `rpc_kanban`: kanban board data and PR helpers

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::message::Message;
use crate::rpc::{Request, RequestId, Response, RpcError};

use super::constants::*;
use super::helpers::*;
use super::{DaemonState, effects, snapshot};
use super::{rpc_channel, rpc_kanban, rpc_sessions, rpc_tasks};

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
/// Used to decide whether to auto-complete a task when a coworker reports
/// WorkflowPhase::Completed. Tasks with open PRs should complete on merge,
/// not on phase transition.
async fn task_has_open_pr(task_id: &str, state: &DaemonState) -> bool {
    let ps = state.persistent_state.lock().await;

    // Check all PR author sessions to see if any have this task_id
    for (_pr_number, session) in ps.github.pr_author_sessions.iter() {
        if let Some(ref stored_task_id) = session.task_id
            && stored_task_id == task_id
        {
            return true;
        }
    }

    false
}

/// Parse an optional auth provider from RPC params.
///
/// Defaults to Claude when the `provider` field is missing.
fn parse_provider_param(
    params: Option<&serde_json::Value>,
) -> Result<crate::auth::AuthProvider, String> {
    let provider = params
        .and_then(|p| p.get("provider"))
        .and_then(|v| v.as_str())
        .map(str::parse::<crate::auth::AuthProvider>)
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(provider)
}

/// Convenience: extract an `&str` param from an optional JSON object.
fn str_param<'a>(params: Option<&'a serde_json::Value>, key: &str) -> Option<&'a str> {
    params.and_then(|p| p.get(key)).and_then(|v| v.as_str())
}

/// Convenience: extract a `bool` param with a default.
fn bool_param(params: Option<&serde_json::Value>, key: &str, default: bool) -> bool {
    params
        .and_then(|p| p.get(key))
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

/// Convenience: extract a `u64` param.
fn u64_param(params: Option<&serde_json::Value>, key: &str) -> Option<u64> {
    params.and_then(|p| p.get(key)).and_then(|v| v.as_u64())
}

/// Convenience: extract an optional `Vec<String>` param from a JSON array.
fn string_array_param(params: Option<&serde_json::Value>, key: &str) -> Option<Vec<String>> {
    params
        .and_then(|p| p.get(key))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
}

// ============================================================================
// Connection handling
// ============================================================================

pub(super) async fn handle_connection(
    stream: tokio::net::UnixStream,
    mut shutdown_rx: broadcast::Receiver<()>,
    state: std::sync::Arc<DaemonState>,
) {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();

        tokio::select! {
            // Read next request line
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => {
                        debug!("Client disconnected");
                        break;
                    }
                    Ok(_) => {
                        let response = handle_request(&line, &state).await;
                        let response_json = match serde_json::to_string(&response) {
                            Ok(json) => json,
                            Err(e) => {
                                error!("Failed to serialize response: {}", e);
                                continue;
                            }
                        };

                        if let Err(e) = writer.write_all(response_json.as_bytes()).await {
                            warn!("Failed to write response: {}", e);
                            break;
                        }
                        if let Err(e) = writer.write_all(b"\n").await {
                            warn!("Failed to write newline: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("Read error: {}", e);
                        break;
                    }
                }
            }

            // Handle shutdown signal
            _ = shutdown_rx.recv() => {
                debug!("Connection handler received shutdown signal");
                break;
            }
        }
    }
}

// ============================================================================
// Request dispatch
// ============================================================================

/// Process a JSON-RPC request and return a response.
async fn handle_request(line: &str, state: &DaemonState) -> Response {
    // Parse the request
    let request: Request = match serde_json::from_str(line) {
        Ok(req) => req,
        Err(e) => {
            warn!("Failed to parse request: {}", e);
            return Response::error(RequestId::Null, RpcError::parse_error());
        }
    };

    debug!(
        "Received request: method={}, id={:?}",
        request.method, request.id
    );

    // Clone request ID and method for cache operations (they will be moved during dispatch)
    let request_id_for_cache = request.id.clone();
    let request_method = request.method.clone();

    // Methods with their own domain-specific caching should skip the RPC idempotency cache.
    let skip_rpc_cache = request_method == "kanban.data";

    // Check cache for idempotent response (within 60 second TTL)
    if !skip_rpc_cache {
        let now = Instant::now();
        let cache = state.rpc_response_cache.lock().await;
        if let Some((cached_response, timestamp)) = cache.get(&request_id_for_cache)
            && now.duration_since(*timestamp).as_secs() < 60
        {
            debug!(
                "Cache hit for request id={:?}, returning cached response",
                request_id_for_cache
            );
            return cached_response.clone();
        }
    }

    let params = request.params.as_ref();

    // Dispatch based on method
    let response = match request.method.as_str() {
        "ping" => Response::success(request.id, serde_json::json!("pong")),

        "version" => Response::success(
            request.id,
            serde_json::json!({
                "name": "midtown",
                "version": env!("CARGO_PKG_VERSION"),
            }),
        ),

        "shutdown" => {
            info!("Shutdown requested via RPC");
            Response::success(request.id, serde_json::json!({"status": "shutting_down"}))
        }

        "daemon.enter-drain" => {
            info!("Drain mode requested via RPC");
            state
                .draining
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Response::success(request.id, serde_json::json!({"status": "draining"}))
        }

        "coworker.spawn" => {
            let resume = bool_param(params, "resume", false);
            let prompt = str_param(params, "prompt").map(|s| s.to_string());
            let provider = match parse_provider_param(params) {
                Ok(provider) => provider,
                Err(msg) => return Response::error(request.id, RpcError::new(-32602, msg)),
            };
            handle_coworker_spawn(request.id, state, resume, prompt, provider).await
        }

        "coworker.break" => match str_param(params, "name") {
            Some(name) => handle_coworker_break(request.id, name, state).await,
            None => Response::error(request.id, RpcError::invalid_params()),
        },

        "coworker.list" => handle_coworker_list(request.id, state),

        "coworker.view" => match str_param(params, "name") {
            Some(name) => handle_coworker_view(request.id, name, state).await,
            None => Response::error(request.id, RpcError::invalid_params()),
        },

        "coworker.report-state" => {
            let name = str_param(params, "name");
            let phase = str_param(params, "phase");
            let task_id = u64_param(params, "task_id").map(|v| v as u32);

            match (name, phase) {
                (Some(name), Some(phase)) => {
                    handle_coworker_report_state(request.id, name, phase, task_id, state).await
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "coworker.nudge" => {
            let name = str_param(params, "name");
            let message = str_param(params, "message");
            let from = str_param(params, "from").unwrap_or("lead");

            match (name, message) {
                (Some(name), Some(message)) => {
                    handle_coworker_nudge(request.id, from, name, message, state).await
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "coworker.asking" => {
            let name = str_param(params, "name");
            let question = str_param(params, "question");

            match (name, question) {
                (Some(name), Some(question)) => {
                    handle_coworker_asking(request.id, name, question, state).await
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        // --- Delegated to rpc_channel ---
        "status" => rpc_channel::handle_status(request.id, state).await,

        "channel.post" => {
            let message = str_param(params, "message");
            let from = str_param(params, "from").unwrap_or("lead");
            let channel = str_param(params, "channel");

            match message {
                Some(msg) => {
                    rpc_channel::handle_channel_post(request.id, from, msg, channel, state).await
                }
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "channel.read" => {
            let all = bool_param(params, "all", false);
            rpc_channel::handle_channel_read(request.id, all, state)
        }

        "reminder.create" => {
            let trigger = str_param(params, "trigger");
            let message = str_param(params, "message");

            match (trigger, message) {
                (Some("all-work-merged"), Some(msg)) => {
                    rpc_channel::handle_reminder_create(request.id, msg, state).await
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "reminder.list" => rpc_channel::handle_reminder_list(request.id, state).await,

        "reminder.cancel" => match str_param(params, "id") {
            Some(id) => rpc_channel::handle_reminder_cancel(request.id, id, state).await,
            None => Response::error(request.id, RpcError::invalid_params()),
        },

        // --- Delegated to rpc_kanban ---
        "kanban.data" => rpc_kanban::handle_kanban_data(request.id, state).await,

        // --- Delegated to rpc_tasks ---
        "task.create" => {
            let subject = str_param(params, "subject");
            let description = str_param(params, "description").unwrap_or("");
            let blocked_by = string_array_param(params, "blocked_by");
            let channel = str_param(params, "channel");
            let model = str_param(params, "model");
            let pr = u64_param(params, "pr");

            match subject {
                Some(subject) => {
                    rpc_tasks::handle_task_create(
                        request.id,
                        subject,
                        description,
                        blocked_by.as_deref(),
                        channel,
                        model,
                        pr,
                        state,
                    )
                    .await
                }
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "task.update" => {
            let id = str_param(params, "id");

            match id {
                Some(id) => {
                    let owner = str_param(params, "owner");
                    let status = str_param(params, "status");
                    let description = str_param(params, "description");
                    let blocked_by = string_array_param(params, "blocked_by");
                    let channel = str_param(params, "channel");
                    let model = str_param(params, "model");
                    let pr = u64_param(params, "pr");

                    rpc_tasks::handle_task_update(
                        request.id,
                        id,
                        owner,
                        status,
                        description,
                        blocked_by.as_deref(),
                        channel,
                        model,
                        pr,
                        state,
                    )
                    .await
                }
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "task.done" => match str_param(params, "id") {
            Some(id) => rpc_tasks::handle_task_done(request.id, id, state),
            None => Response::error(request.id, RpcError::invalid_params()),
        },

        "task.metadata" => match str_param(params, "id") {
            Some(id) => rpc_tasks::handle_task_metadata(request.id, id, state),
            None => Response::error(request.id, RpcError::invalid_params()),
        },

        "task.request" => {
            let message = str_param(params, "message");
            let from = str_param(params, "from").unwrap_or("unknown");

            match message {
                Some(msg) => rpc_tasks::handle_task_request(request.id, from, msg, state).await,
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "task.claim" => {
            let id = str_param(params, "id");
            let from = str_param(params, "from").unwrap_or("unknown");
            match id {
                Some(id) => rpc_tasks::handle_task_claim(request.id, id, from, state),
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        // --- Delegated to rpc_sessions ---
        "session.attach" => match str_param(params, "target") {
            Some(target) => rpc_sessions::handle_session_attach(request.id, target, state).await,
            None => Response::error(request.id, RpcError::invalid_params()),
        },

        "session.detach" => match str_param(params, "name") {
            Some(name) => rpc_sessions::handle_session_detach(request.id, name, state).await,
            None => Response::error(request.id, RpcError::invalid_params()),
        },

        "session.list" => rpc_sessions::handle_session_list(request.id, state).await,

        // --- Inline handlers ---
        "daemon.check-pending" => {
            info!("Check-pending triggered via RPC");
            let snap = snapshot::collect_world_snapshot(state).await;
            let pending_effects = super::dispatch::spawn_for_pending_tasks(&snap, state);
            state.mark_in_flight_spawns_from_effects(&pending_effects);
            effects::execute_effects(pending_effects, state).await;
            Response::success(request.id, serde_json::json!({"status": "ok"}))
        }

        "auth.switch" => {
            let profile = str_param(params, "profile");
            let all = bool_param(params, "all", false);
            let provider = params
                .and_then(|p| p.get("provider"))
                .and_then(|v| v.as_str())
                .map(str::parse::<crate::auth::AuthProvider>)
                .transpose();

            match (profile, provider) {
                (Some(name), Ok(provider)) => {
                    let provider = provider.unwrap_or_default();
                    handle_auth_switch(request.id, name, all, provider, state).await
                }
                (_, Err(e)) => Response::error(request.id, RpcError::new(-32602, e)),
                (None, Ok(_)) => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "insight.report" => {
            let agent = str_param(params, "agent");
            let insight = str_param(params, "insight");
            let channel = str_param(params, "channel");

            match (agent, insight) {
                (Some(agent), Some(insight)) => {
                    handle_insight_report(request.id, agent, insight, channel, state).await
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "headless.execute" => {
            let prompt = str_param(params, "prompt");

            match prompt {
                Some(prompt) => {
                    let config = crate::headless::HeadlessConfig {
                        model: str_param(params, "model").unwrap_or("sonnet").to_string(),
                        system_prompt: str_param(params, "system_prompt").unwrap_or("").to_string(),
                        json_schema: params.and_then(|p| p.get("json_schema")).cloned(),
                        max_budget_usd: params
                            .and_then(|p| p.get("max_budget_usd"))
                            .and_then(|v| v.as_f64()),
                        allow_tools: bool_param(params, "allow_tools", false),
                        cwd: state
                            .all_repo_paths
                            .first()
                            .map(|p| p.to_string_lossy().to_string()),
                        persist_session: false,
                        resume_session_id: None,
                        inactivity_timeout: None,
                        team_name: None,
                        agent_id: None,
                        agent_name: None,
                        settings_path: None,
                        setting_sources: None,
                        auth_provider: crate::auth::AuthProvider::Claude,
                        env: std::collections::HashMap::new(),
                    };
                    handle_headless_execute(request.id, prompt, &config).await
                }
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "snapshot" => {
            let default_channel = match state.channel_router.default_channel() {
                Ok(ch) => ch,
                Err(e) => {
                    error!("Failed to get default channel for snapshot: {}", e);
                    return Response::error(request.id, RpcError::new(-32603, e.to_string()));
                }
            };
            let snapshot = super::snapshot::collect_world_snapshot(state)
                .await
                .with_debug_context(&default_channel);
            match serde_json::to_value(&snapshot) {
                Ok(value) => Response::success(request.id, value),
                Err(e) => Response::error(
                    request.id,
                    RpcError::new(-32603, format!("Failed to serialize snapshot: {}", e)),
                ),
            }
        }

        _ => {
            warn!("Unknown method: {}", request.method);
            Response::error(request.id, RpcError::method_not_found())
        }
    };

    // Cache only successful responses for idempotency (60 second TTL).
    if !skip_rpc_cache && !response.is_error() {
        let mut cache = state.rpc_response_cache.lock().await;
        cache.insert(request_id_for_cache, (response.clone(), Instant::now()));
    }

    response
}

// ============================================================================
// Coworker lifecycle handlers
// ============================================================================

/// Handle coworker.spawn RPC method.
async fn handle_coworker_spawn(
    id: RequestId,
    state: &DaemonState,
    resume: bool,
    prompt: Option<String>,
    provider: crate::auth::AuthProvider,
) -> Response {
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
        auth_provider: provider,
    };

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
async fn handle_coworker_break(id: RequestId, name: &str, state: &DaemonState) -> Response {
    // Clear reviewer assignment first (before early return for untracked coworkers)
    let cleanup_effects = vec![effects::Effect::ClearOrphanedReviewerAssignments {
        orphaned_coworkers: vec![name.to_string()],
    }];
    effects::execute_effects(cleanup_effects, state).await;

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
fn handle_coworker_list(id: RequestId, state: &DaemonState) -> Response {
    let coworker_tasks: std::collections::HashMap<String, String> =
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

    let coworkers: Vec<serde_json::Value> = state
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
async fn handle_coworker_view(id: RequestId, name: &str, state: &DaemonState) -> Response {
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
/// tmux tab display. When a coworker reports `Idle`, they are immediately
/// sent on break. When `Completed`, the task is handled appropriately
/// (defer to merge if PR exists, or nudge to open PR).
async fn handle_coworker_report_state(
    id: RequestId,
    name: &str,
    phase_str: &str,
    task_id: Option<u32>,
    state: &DaemonState,
) -> Response {
    let phase = match phase_str {
        "claiming" => crate::coworker_state::WorkflowPhase::Claiming,
        "developing" => crate::coworker_state::WorkflowPhase::Developing,
        "testing" => crate::coworker_state::WorkflowPhase::Testing,
        "pull_request" | "pull-request" => crate::coworker_state::WorkflowPhase::PullRequest,
        "reviewing" => crate::coworker_state::WorkflowPhase::Reviewing,
        "debugging" => crate::coworker_state::WorkflowPhase::Debugging,
        "completed" => crate::coworker_state::WorkflowPhase::Completed,
        "idle" => crate::coworker_state::WorkflowPhase::Idle,
        _ => {
            return Response::error(
                id,
                RpcError::new(-32602, format!("Unknown phase: {}", phase_str)),
            );
        }
    };

    // For Idle phase, immediately send the coworker on break
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

        // Immediately trigger task dispatch
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

    // For Completed phase, handle task completion logic
    if phase == crate::coworker_state::WorkflowPhase::Completed {
        handle_coworker_completed(name, task_id, state).await;
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

/// Handle the Completed phase for a coworker's report-state.
///
/// Tasks with open PRs defer completion to the merge path.
/// Tasks without PRs get a nudge to open one first.
async fn handle_coworker_completed(name: &str, task_id: Option<u32>, state: &DaemonState) {
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

/// Handle coworker.nudge RPC method.
async fn handle_coworker_nudge(
    id: RequestId,
    _from: &str,
    name: &str,
    message: &str,
    state: &DaemonState,
) -> Response {
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
async fn handle_coworker_asking(
    id: RequestId,
    name: &str,
    question: &str,
    state: &DaemonState,
) -> Response {
    let msg = Message::text(name, format!("Question for Lead: {}", question));
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        error!("Failed to post question to channel: {}", e);
    }

    let coworkers = state.coworkers.clone();
    let name_owned = name.to_string();
    let nudge_message = format!("{} is asking: {}", name, question);

    tokio::task::spawn_blocking(move || {
        if let Err(e) = coworkers.update_status_display(&name_owned, Some("waiting for feedback")) {
            debug!("Failed to update tmux tab for {}: {}", name_owned, e);
        }
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

// ============================================================================
// Auth switch handler
// ============================================================================

fn filter_coworkers_by_provider(
    coworkers: &[crate::coworker::Coworker],
    provider: crate::auth::AuthProvider,
) -> Vec<crate::coworker::Coworker> {
    coworkers
        .iter()
        .filter(|cw| cw.provider == provider)
        .cloned()
        .collect()
}

fn build_coworker_relaunch_config(
    coworker: &crate::coworker::Coworker,
    repo_name: &str,
) -> crate::launch::LaunchConfig {
    let mut config = crate::launch::LaunchConfig::coworker(
        coworker.name.clone(),
        repo_name.to_string(),
        crate::launch::SessionMode::Resume,
        None,
    );
    config.model = coworker.model.clone();
    config.auth_provider = coworker.provider;
    config
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeadRelaunchStatus {
    Relaunched,
    Failed,
    Unchanged,
}

impl LeadRelaunchStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Relaunched => "relaunched",
            Self::Failed => "failed",
            Self::Unchanged => "unchanged",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::Relaunched => "re-launched lead",
            Self::Failed => "lead re-launch failed",
            Self::Unchanged => "lead unchanged",
        }
    }

    fn relaunched(self) -> bool {
        matches!(self, Self::Relaunched)
    }

    fn attempted(self) -> bool {
        !matches!(self, Self::Unchanged)
    }
}

/// Handle auth.switch RPC method.
async fn handle_auth_switch(
    id: RequestId,
    profile: &str,
    all: bool,
    provider: crate::auth::AuthProvider,
    state: &DaemonState,
) -> Response {
    // Validate the profile name format
    if let Err(e) = crate::auth::validate_profile_name(profile) {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Invalid profile name: {}", e)),
        );
    }

    // Validate the profile exists
    if !crate::auth::profile_exists_for(provider, profile) {
        return Response::error(
            id,
            RpcError::new(
                -32602,
                format!(
                    "Profile '{}' does not exist for {}. Create it with: midtown auth --provider {} login {}",
                    profile, provider, provider, profile
                ),
            ),
        );
    }

    // Check if already on this profile
    if !all {
        let path = crate::config::project_config_path(&state.repo_name);
        if let Some(config) = crate::config::FullProjectConfig::load_from(&path)
            && crate::auth::project_profile_override(&config.project, provider) == Some(profile)
        {
            return Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Already on {} profile '{}'", provider, profile),
                    "switched": false,
                }),
            );
        }
    }

    // Switch the profile on disk
    if all {
        let current = crate::auth::current_profile_for(provider);
        let cleared = crate::config::clear_all_project_auth_profiles_for(provider);
        if current != profile
            && let Err(e) = crate::auth::set_current_profile_for(provider, profile)
        {
            return Response::error(
                id,
                RpcError::new(-32603, format!("Failed to switch profile: {}", e)),
            );
        }
        if current == profile && cleared == 0 {
            return Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Already on {} profile '{}'", provider, profile),
                    "switched": false,
                }),
            );
        }
    } else {
        let path = crate::config::project_config_path(&state.repo_name);
        let mut config = crate::config::FullProjectConfig::load_from(&path).unwrap_or_default();
        crate::auth::set_project_profile_override(
            &mut config.project,
            provider,
            profile.to_string(),
        );
        if let Err(e) = config.save_to(&path) {
            return Response::error(
                id,
                RpcError::new(-32603, format!("Failed to save project config: {}", e)),
            );
        }
    }

    info!(
        "Auth profile switched to '{}' for {} ({})",
        profile,
        provider,
        if all { "global" } else { "project" }
    );

    // Shut down all running coworkers for this provider
    let running_coworkers: Vec<crate::coworker::Coworker> =
        filter_coworkers_by_provider(&state.coworkers.list(), provider);

    let shutdown_count = running_coworkers.len();
    for coworker in &running_coworkers {
        let name = &coworker.name;
        let coworkers = state.coworkers.clone();
        let name_owned = name.clone();
        let shutdown_result =
            tokio::task::spawn_blocking(move || coworkers.shutdown(&name_owned)).await;

        match shutdown_result {
            Ok(Ok(())) => {
                state.record_coworker_stop_time(name);
                {
                    let mut records = state.coworker_records.write().await;
                    records.remove(name);
                }
                state.broadcast_coworker_update(name, "stopped", None);
            }
            Ok(Err(e)) => {
                warn!("Failed to shut down coworker {}: {}", name, e);
            }
            Err(e) => {
                warn!("spawn_blocking panic while shutting down {}: {}", name, e);
            }
        }
    }

    // Capture reviewer assignments before relaunch
    let reviewer_pr_by_name: std::collections::HashMap<String, u64> = {
        let persistent = state.persistent_state.lock().await;
        running_coworkers
            .iter()
            .filter_map(|cw| {
                persistent
                    .github
                    .pr_for_reviewer(&cw.name)
                    .map(|pr| (cw.name.clone(), pr))
            })
            .collect()
    };

    // Re-launch lead only when switching the provider backing the interactive session
    let lead_relaunch_status = if provider == crate::auth::AuthProvider::Claude {
        let session = state.coworkers.session_name().to_string();
        let workdir = state
            .all_repo_paths
            .first()
            .cloned()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let project_name = state.repo_name.clone();
        let additional_dirs: Vec<std::path::PathBuf> =
            state.all_repo_paths.iter().skip(1).cloned().collect();

        let lead_result = tokio::task::spawn_blocking(move || {
            crate::tmux::spawn_lead(&session, &workdir, &project_name, &additional_dirs)
        })
        .await;

        match lead_result {
            Ok(Ok(())) => {
                info!("Re-launched lead with auth profile '{}'", profile);
                LeadRelaunchStatus::Relaunched
            }
            Ok(Err(e)) => {
                warn!("Failed to re-launch lead: {}", e);
                LeadRelaunchStatus::Failed
            }
            Err(e) => {
                warn!("spawn_blocking panic while re-launching lead: {}", e);
                LeadRelaunchStatus::Failed
            }
        }
    } else {
        LeadRelaunchStatus::Unchanged
    };

    // Re-launch all sessions for this provider using the updated auth profile
    let provider_auth_dir =
        crate::auth::active_profile_dir_for_project_with_provider(&state.repo_name, provider);
    let mut relaunch_count = 0usize;
    for coworker in &running_coworkers {
        let mut config = if let Some(pr_number) = reviewer_pr_by_name.get(&coworker.name).copied() {
            let mut reviewer =
                crate::launch::LaunchConfig::reviewer(coworker.name.clone(), pr_number);
            reviewer.session_mode = crate::launch::SessionMode::Resume;
            reviewer.model = coworker.model.clone();
            reviewer
        } else {
            build_coworker_relaunch_config(coworker, &state.repo_name)
        };
        config.auth_profile_dir = Some(provider_auth_dir.clone());

        match state.spawn_coworker(&config).await {
            Ok(()) => relaunch_count += 1,
            Err(e) => warn!(
                "Failed to relaunch coworker '{}' after {} auth switch: {}",
                coworker.name, provider, e
            ),
        }
    }

    // Post to channel
    let msg = Message::system(format!(
        "Switched to {} auth profile '{}' - restarted {}/{} coworker(s), {}",
        provider,
        profile,
        relaunch_count,
        shutdown_count,
        lead_relaunch_status.summary()
    ));
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        warn!("Failed to post auth switch message: {}", e);
    }

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!(
                "Switched to {} profile '{}'. Restarted {}/{} coworker(s), {}.",
                provider,
                profile,
                relaunch_count,
                shutdown_count,
                lead_relaunch_status.summary()
            ),
            "switched": true,
            "coworkers_shutdown": shutdown_count,
            "coworkers_relaunched": relaunch_count,
            "lead_relaunched": lead_relaunch_status.relaunched(),
            "lead_relaunch_attempted": lead_relaunch_status.attempted(),
            "lead_relaunch_status": lead_relaunch_status.as_str(),
        }),
    )
}

// ============================================================================
// Insight & headless handlers
// ============================================================================

/// Handle insight.report RPC method.
async fn handle_insight_report(
    id: RequestId,
    agent: &str,
    insight: &str,
    channel: Option<&str>,
    state: &DaemonState,
) -> Response {
    // Deduplicate: normalize and hash the insight content
    let hash = hash_insight(insight);
    {
        let mut hashes = state.insight_hashes.lock().unwrap();
        if !hashes.insert(hash) {
            debug!("insight.report: duplicate insight from {}, skipping", agent);
            return Response::success(
                id,
                serde_json::json!({
                    "posted": false,
                    "reason": "duplicate",
                }),
            );
        }
    }

    // Post insight to specified channel (or main if None)
    let channel_name = channel.unwrap_or_else(|| state.channel_router.default_channel_name());
    let msg = Message::for_channel(
        channel_name,
        agent,
        format!("💡 {}", insight),
        crate::message::MessageType::Text,
    );
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        warn!("insight.report: failed to post to channel: {}", e);
        return Response::error(
            id,
            RpcError::new(-32603, format!("Failed to post insight: {}", e)),
        );
    }

    info!(
        "insight.report: posted insight from {} to channel '{}'",
        agent, channel_name
    );

    // Determine working directory for the architect session
    let cwd = if is_coworker_sender(agent) {
        let worktree = crate::paths::coworkers_dir_for_repo(&state.repo_name).join(agent);
        if worktree.exists() {
            worktree
        } else {
            state.all_repo_paths.first().cloned().unwrap_or_default()
        }
    } else {
        state.all_repo_paths.first().cloned().unwrap_or_default()
    };

    // Spawn the architect task asynchronously
    let repo_name = state.repo_name.clone();
    let insight_owned = insight.to_string();
    let channel_owned = channel.map(|s| s.to_string());
    tokio::spawn(async move {
        super::architect::generate_insight_diagram(insight_owned, cwd, repo_name, channel_owned)
            .await;
    });

    Response::success(
        id,
        serde_json::json!({
            "posted": true,
        }),
    )
}

/// Hash insight content for deduplication.
fn hash_insight(insight: &str) -> u64 {
    let normalized: String = insight
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    hasher.finish()
}

/// Handle headless.execute RPC method.
async fn handle_headless_execute(
    id: RequestId,
    prompt: &str,
    config: &crate::headless::HeadlessConfig,
) -> Response {
    info!(
        "Headless execute: model={}, prompt_len={}, has_schema={}",
        config.model,
        prompt.len(),
        config.json_schema.is_some()
    );

    let timeout = std::time::Duration::from_secs(300);

    match crate::headless::execute(config, prompt, timeout).await {
        Ok(result) => {
            info!(
                "Headless execute complete: cost=${:.4}, duration={}ms, error={}",
                result.cost_usd.unwrap_or(0.0),
                result.duration_ms.unwrap_or(0),
                result.is_error,
            );
            Response::success(
                id,
                serde_json::json!({
                    "success": !result.is_error,
                    "result": result.result,
                    "cost_usd": result.cost_usd,
                    "duration_ms": result.duration_ms,
                    "session_id": result.session_id,
                }),
            )
        }
        Err(e) => {
            warn!("Headless execute failed: {}", e);
            Response::error(
                id,
                RpcError::new(-32603, format!("Headless execution failed: {}", e)),
            )
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    #[test]
    fn test_hash_insight_deterministic() {
        let hash1 = hash_insight("Test insight content");
        let hash2 = hash_insight("Test insight content");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_insight_different_content() {
        let hash1 = hash_insight("Insight one");
        let hash2 = hash_insight("Insight two");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_hash_insight_normalizes_whitespace() {
        let hash1 = hash_insight("This is an insight");
        let hash2 = hash_insight("  This  is   an   insight  ");
        let hash3 = hash_insight("This\n  is\nan\ninsight");
        let hash4 = hash_insight("THIS IS AN INSIGHT");

        assert_eq!(hash1, hash2, "extra whitespace should be normalized");
        assert_eq!(hash1, hash3, "newlines should be normalized");
        assert_eq!(hash1, hash4, "case should be normalized");
    }

    #[test]
    fn test_filter_coworkers_by_provider() {
        let coworkers = vec![
            crate::coworker::Coworker {
                slot_id: "1".to_string(),
                name: "lexington".to_string(),
                status: crate::coworker::CoworkerStatus::Running,
                working_dir: "/tmp/lexington".to_string(),
                started_at: chrono::Utc::now(),
                current_task: Some("Build auth".to_string()),
                session_id: None,
                model: "sonnet".to_string(),
                provider: crate::auth::AuthProvider::Claude,
            },
            crate::coworker::Coworker {
                slot_id: "2".to_string(),
                name: "park".to_string(),
                status: crate::coworker::CoworkerStatus::Running,
                working_dir: "/tmp/park".to_string(),
                started_at: chrono::Utc::now(),
                current_task: Some("Review PR".to_string()),
                session_id: None,
                model: "gpt-5-codex".to_string(),
                provider: crate::auth::AuthProvider::Codex,
            },
        ];

        let claude = filter_coworkers_by_provider(&coworkers, crate::auth::AuthProvider::Claude);
        let codex = filter_coworkers_by_provider(&coworkers, crate::auth::AuthProvider::Codex);

        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].name, "lexington");
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].name, "park");
    }

    #[test]
    fn test_parse_provider_param_defaults_to_claude() {
        let provider = parse_provider_param(None).expect("should parse default provider");
        assert_eq!(provider, crate::auth::AuthProvider::Claude);
    }

    #[test]
    fn test_parse_provider_param_parses_codex() {
        let params = serde_json::json!({ "provider": "codex" });
        let provider = parse_provider_param(Some(&params)).expect("should parse codex");
        assert_eq!(provider, crate::auth::AuthProvider::Codex);
    }

    #[test]
    fn test_parse_provider_param_rejects_unknown_provider() {
        let params = serde_json::json!({ "provider": "unknown" });
        let err = parse_provider_param(Some(&params)).expect_err("provider should be rejected");
        assert!(err.contains("Unsupported provider"));
    }

    #[test]
    fn test_build_coworker_relaunch_config_preserves_name_and_model() {
        let coworker = crate::coworker::Coworker {
            slot_id: "1".to_string(),
            name: "madison".to_string(),
            status: crate::coworker::CoworkerStatus::Running,
            working_dir: "/tmp/madison".to_string(),
            started_at: chrono::Utc::now(),
            current_task: Some("Fix tests".to_string()),
            session_id: None,
            model: "opus".to_string(),
            provider: crate::auth::AuthProvider::Claude,
        };

        let config = build_coworker_relaunch_config(&coworker, "midtown");
        assert_eq!(config.name, "madison");
        assert_eq!(config.model, "opus");
        assert_eq!(config.session_mode, crate::launch::SessionMode::Resume);
    }

    #[test]
    fn test_lead_relaunch_status_strings() {
        assert_eq!(LeadRelaunchStatus::Relaunched.as_str(), "relaunched");
        assert_eq!(LeadRelaunchStatus::Failed.as_str(), "failed");
        assert_eq!(LeadRelaunchStatus::Unchanged.as_str(), "unchanged");
        assert_eq!(LeadRelaunchStatus::Unchanged.summary(), "lead unchanged");
        assert!(!LeadRelaunchStatus::Unchanged.attempted());
        assert!(LeadRelaunchStatus::Relaunched.relaunched());
    }

    // ---- RPC idempotency cache tests ----

    #[test]
    fn test_rpc_cache_ttl_expiration() {
        use crate::rpc::{RequestId, Response};

        let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();
        let request_id = RequestId::String("test-ttl-123".to_string());
        let cached_response =
            Response::success(request_id.clone(), serde_json::json!({"task_id": 42}));

        let old_timestamp = Instant::now() - Duration::from_secs(61);
        cache.insert(request_id.clone(), (cached_response, old_timestamp));

        let now = Instant::now();
        let cache_hit = cache
            .get(&request_id)
            .filter(|(_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

        assert!(
            cache_hit.is_none(),
            "Entry older than 60 seconds should be a cache miss"
        );
    }

    #[test]
    fn test_rpc_cache_within_ttl() {
        use crate::rpc::{RequestId, Response};

        let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();
        let request_id = RequestId::String("test-fresh-456".to_string());
        let cached_response =
            Response::success(request_id.clone(), serde_json::json!({"task_id": 99}));

        cache.insert(request_id.clone(), (cached_response, Instant::now()));

        let now = Instant::now();
        let cache_hit = cache
            .get(&request_id)
            .filter(|(_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

        assert!(cache_hit.is_some(), "Recent entry should be a cache hit");
    }

    #[test]
    fn test_rpc_cache_cleanup_removes_expired_entries() {
        use crate::rpc::{RequestId, Response};

        let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();

        let old_timestamp = Instant::now() - Duration::from_secs(120);
        for i in 0..100 {
            let id = RequestId::String(format!("expired-{}", i));
            let resp = Response::success(id.clone(), serde_json::json!({"i": i}));
            cache.insert(id, (resp, old_timestamp));
        }

        let fresh_timestamp = Instant::now();
        for i in 0..3 {
            let id = RequestId::String(format!("fresh-{}", i));
            let resp = Response::success(id.clone(), serde_json::json!({"i": i}));
            cache.insert(id, (resp, fresh_timestamp));
        }

        assert_eq!(cache.len(), 103);

        let now = Instant::now();
        cache.retain(|_, (_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn test_rpc_cache_only_caches_success_responses() {
        use crate::rpc::{RequestId, Response, RpcError};

        let success = Response::success(
            RequestId::String("s1".to_string()),
            serde_json::json!({"ok": true}),
        );
        let error = Response::error(
            RequestId::String("e1".to_string()),
            RpcError::invalid_params(),
        );

        assert!(!success.is_error());
        assert!(error.is_error());
    }

    #[test]
    fn test_rpc_cache_numeric_id_collision() {
        use crate::rpc::{RequestId, Response};

        let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();

        let id_from_process_a = RequestId::Number(1);
        let response_a = Response::success(
            id_from_process_a.clone(),
            serde_json::json!({"task_id": 100}),
        );
        cache.insert(id_from_process_a.clone(), (response_a, Instant::now()));

        let id_from_process_b = RequestId::Number(1);
        let now = Instant::now();
        let cache_hit = cache
            .get(&id_from_process_b)
            .filter(|(_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

        assert!(
            cache_hit.is_some(),
            "Numeric ID collision: same id=1 from different processes hits cache (this is the bug)"
        );

        let id_with_pid_a = RequestId::String("12345-1".to_string());
        let id_with_pid_b = RequestId::String("12346-1".to_string());

        let response_a2 =
            Response::success(id_with_pid_a.clone(), serde_json::json!({"task_id": 100}));
        cache.insert(id_with_pid_a, (response_a2, Instant::now()));

        let cache_hit = cache
            .get(&id_with_pid_b)
            .filter(|(_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

        assert!(
            cache_hit.is_none(),
            "PID-prefixed string IDs from different processes should NOT collide"
        );
    }
}
