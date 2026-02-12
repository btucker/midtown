//! RPC request handlers for the daemon's Unix socket protocol.
//!
//! This module is the entry point for JSON-RPC dispatch. It routes requests to
//! domain-specific handler modules:
//!
//! - `rpc_auth` — authentication switching
//! - `rpc_channel` — channel post/read
//! - `rpc_kanban` — kanban board data
//! - `rpc_session` — session attach/detach/list
//! - `rpc_status` — daemon status overview
//! - `rpc_task` — task CRUD operations
//!
//! Handlers that are small or tightly coupled to the dispatch layer (coworker
//! lifecycle, insight reporting, reminders, headless execute) remain here.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::message::Message;
use crate::rpc::{Request, RequestId, Response, RpcError};

use super::constants::*;
use super::helpers::*;
use super::{DaemonState, effects, snapshot};

// ============================================================================
// Param extraction helpers
// ============================================================================

/// Extract a required string parameter from an RPC request, returning
/// `Response::error(id, invalid_params())` on missing values.
///
/// Usage: `require_str!(params, "name", id)` expands to a `let name = ...;`
/// binding or an early return with an invalid-params error.
macro_rules! require_str {
    ($params:expr, $key:literal, $id:expr) => {
        match $params.and_then(|p| p.get($key)).and_then(|v| v.as_str()) {
            Some(v) => v,
            None => return Response::error($id, RpcError::invalid_params()),
        }
    };
}

/// Extension trait for ergonomic RPC parameter extraction.
///
/// Reduces the common `params.and_then(|p| p.get("key")).and_then(|v| v.as_str())`
/// chain to `params.str_param("key")`.
trait ParamExt {
    fn str_param(&self, key: &str) -> Option<&str>;
    fn bool_or(&self, key: &str, default: bool) -> bool;
    fn str_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str;
    fn u64_param(&self, key: &str) -> Option<u64>;
    fn str_array_param(&self, key: &str) -> Option<Vec<String>>;
}

impl ParamExt for Option<&serde_json::Value> {
    fn str_param(&self, key: &str) -> Option<&str> {
        self.and_then(|p| p.get(key)).and_then(|v| v.as_str())
    }

    fn bool_or(&self, key: &str, default: bool) -> bool {
        self.and_then(|p| p.get(key))
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    }

    fn str_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.str_param(key).unwrap_or(default)
    }

    fn u64_param(&self, key: &str) -> Option<u64> {
        self.and_then(|p| p.get(key)).and_then(|v| v.as_u64())
    }

    fn str_array_param(&self, key: &str) -> Option<Vec<String>> {
        self.and_then(|p| p.get(key))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
    }
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

/// Parse an optional auth provider from RPC params.
///
/// Defaults to Claude when the `provider` field is missing.
fn parse_provider_param(
    params: Option<&serde_json::Value>,
) -> Result<crate::auth::AuthProvider, String> {
    params
        .and_then(|p| p.get("provider"))
        .and_then(|v| v.as_str())
        .map(str::parse::<crate::auth::AuthProvider>)
        .transpose()
        .map(|opt| opt.unwrap_or_default())
        .map_err(|e| e.to_string())
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
    // kanban.data has a dedicated 30s TTL cache in DaemonState; the RPC cache (60s, keyed by
    // request ID) would shadow it because the web server always sends id=1 for kanban requests.
    let skip_rpc_cache = request_method == "kanban.data";

    // Check cache for idempotent response (within 60 second TTL)
    if !skip_rpc_cache {
        let now = std::time::Instant::now();
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

    // Dispatch based on method
    let response = dispatch_request(request, state).await;

    // Cache only successful responses for idempotency (60 second TTL).
    // Error responses are NOT cached so that clients can retry after transient
    // failures (e.g., invalid params due to race conditions) without getting
    // a stale cached error.
    // Methods with domain-specific caching (e.g., kanban.data) are excluded.
    if !skip_rpc_cache && !response.is_error() {
        let mut cache = state.rpc_response_cache.lock().await;
        cache.insert(
            request_id_for_cache,
            (response.clone(), std::time::Instant::now()),
        );
    }

    response
}

/// Route a parsed request to the appropriate handler.
///
/// This is the central dispatch table. Each RPC method maps to a handler
/// function, most of which live in dedicated `rpc_*` modules.
async fn dispatch_request(request: Request, state: &DaemonState) -> Response {
    let params = request.params.as_ref();

    match request.method.as_str() {
        // ---- Simple / inline handlers ----
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

        "daemon.check-pending" => {
            info!("Check-pending triggered via RPC");
            let snap = snapshot::collect_world_snapshot(state).await;
            let pending_effects = super::dispatch::spawn_for_pending_tasks(&snap, state);
            state.mark_in_flight_spawns_from_effects(&pending_effects);
            effects::execute_effects(pending_effects, state).await;
            Response::success(request.id, serde_json::json!({"status": "ok"}))
        }

        "daemon.exec-restart" => {
            info!("Exec-restart requested via RPC — daemon will re-exec after shutdown");
            state
                .restart_requested
                .store(true, std::sync::atomic::Ordering::SeqCst);
            // Trigger the shutdown broadcast so the main event loop exits.
            // The run() function checks restart_requested and returns ExecRestart.
            let _ = state.shutdown_tx.send(());
            Response::success(request.id, serde_json::json!({"status": "restarting"}))
        }

        "snapshot" => handle_snapshot(request.id, state).await,

        // ---- Coworker lifecycle ----
        "coworker.spawn" => {
            let resume = params.bool_or("resume", false);
            let prompt = params.str_param("prompt").map(|s| s.to_string());
            let provider = match parse_provider_param(params) {
                Ok(provider) => provider,
                Err(msg) => return Response::error(request.id, RpcError::new(-32602, msg)),
            };
            handle_coworker_spawn(request.id, state, resume, prompt, provider).await
        }

        "coworker.break" => {
            let name = require_str!(params, "name", request.id);
            handle_coworker_break(request.id, name, state).await
        }

        "coworker.list" => handle_coworker_list(request.id, state),

        "coworker.view" => {
            let name = require_str!(params, "name", request.id);
            handle_coworker_view(request.id, name, state).await
        }

        "coworker.report-state" => {
            let name = require_str!(params, "name", request.id);
            let phase = require_str!(params, "phase", request.id);
            let task_id = params.u64_param("task_id").map(|v| v as u32);
            handle_coworker_report_state(request.id, name, phase, task_id, state).await
        }

        "coworker.nudge" => {
            let name = require_str!(params, "name", request.id);
            let message = require_str!(params, "message", request.id);
            let from = params.str_or("from", "lead");
            handle_coworker_nudge(request.id, from, name, message, state).await
        }

        "coworker.asking" => {
            let name = require_str!(params, "name", request.id);
            let question = require_str!(params, "question", request.id);
            handle_coworker_asking(request.id, name, question, state).await
        }

        // ---- Status / kanban ----
        "status" => super::rpc_status::handle_status(request.id, state).await,

        "kanban.data" => super::rpc_kanban::handle_kanban_data(request.id, state).await,

        // ---- Channel ----
        "channel.post" => {
            let message = require_str!(params, "message", request.id);
            let from = params.str_or("from", "lead");
            let channel = params.str_param("channel");
            super::rpc_channel::handle_channel_post(request.id, from, message, channel, state).await
        }

        "channel.read" => {
            let all = params.bool_or("all", false);
            super::rpc_channel::handle_channel_read(request.id, all, state)
        }

        // ---- Tasks ----
        "task.create" => {
            let subject = require_str!(params, "subject", request.id);
            let description = params.str_or("description", "");
            let blocked_by = params.str_array_param("blocked_by");
            let channel = params.str_param("channel");
            let model = params.str_param("model");
            let pr = params.u64_param("pr");
            super::rpc_task::handle_task_create(
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

        "task.update" => {
            let task_id = require_str!(params, "id", request.id);
            let owner = params.str_param("owner");
            let status = params.str_param("status");
            let description = params.str_param("description");
            let blocked_by = params.str_array_param("blocked_by");
            let channel = params.str_param("channel");
            let model = params.str_param("model");
            let pr = params.u64_param("pr");
            super::rpc_task::handle_task_update(
                request.id,
                task_id,
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

        "task.done" => {
            let task_id = require_str!(params, "id", request.id);
            super::rpc_task::handle_task_done(request.id, task_id, state)
        }

        "task.metadata" => {
            let task_id = require_str!(params, "id", request.id);
            super::rpc_task::handle_task_metadata(request.id, task_id, state)
        }

        "task.request" => {
            let message = require_str!(params, "message", request.id);
            let from = params.str_or("from", "unknown");
            super::rpc_task::handle_task_request(request.id, from, message, state).await
        }

        "task.claim" => {
            let task_id = require_str!(params, "id", request.id);
            let from = params.str_or("from", "unknown");
            super::rpc_task::handle_task_claim(request.id, task_id, from, state)
        }

        // ---- Reminders ----
        "reminder.create" => {
            let trigger = require_str!(params, "trigger", request.id);
            let message = require_str!(params, "message", request.id);
            if trigger != "all-work-merged" {
                Response::error(request.id, RpcError::invalid_params())
            } else {
                handle_reminder_create(request.id, message, state).await
            }
        }

        "reminder.list" => handle_reminder_list(request.id, state).await,

        "reminder.cancel" => {
            let reminder_id = require_str!(params, "id", request.id);
            handle_reminder_cancel(request.id, reminder_id, state).await
        }

        // ---- Auth ----
        "auth.switch" => {
            let profile = params.str_param("profile");
            let all = params.bool_or("all", false);
            let provider = params
                .str_param("provider")
                .map(str::parse::<crate::auth::AuthProvider>)
                .transpose();

            match (profile, provider) {
                (Some(name), Ok(provider)) => {
                    let provider = provider.unwrap_or_default();
                    super::rpc_auth::handle_auth_switch(request.id, name, all, provider, state)
                        .await
                }
                (_, Err(e)) => Response::error(request.id, RpcError::new(-32602, e)),
                (None, Ok(_)) => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        // ---- Insight ----
        "insight.report" => {
            let agent = require_str!(params, "agent", request.id);
            let insight = require_str!(params, "insight", request.id);
            let channel = params.str_param("channel");
            handle_insight_report(request.id, agent, insight, channel, state).await
        }

        // ---- Headless ----
        "headless.execute" => {
            let prompt = require_str!(params, "prompt", request.id);
            let config = crate::headless::HeadlessConfig {
                model: params.str_or("model", "sonnet").to_string(),
                system_prompt: params.str_or("system_prompt", "").to_string(),
                json_schema: params.and_then(|p| p.get("json_schema")).cloned(),
                max_budget_usd: params
                    .and_then(|p| p.get("max_budget_usd"))
                    .and_then(|v| v.as_f64()),
                allow_tools: params.bool_or("allow_tools", false),
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

        // ---- Sessions ----
        "session.attach" => {
            let target = require_str!(params, "target", request.id);
            super::rpc_session::handle_session_attach(request.id, target, state).await
        }

        "session.detach" => {
            let name = require_str!(params, "name", request.id);
            super::rpc_session::handle_session_detach(request.id, name, state).await
        }

        "session.list" => super::rpc_session::handle_session_list(request.id, state).await,

        _ => {
            warn!("Unknown method: {}", request.method);
            Response::error(request.id, RpcError::method_not_found())
        }
    }
}

// ============================================================================
// Coworker handlers (tightly coupled to dispatch)
// ============================================================================

/// Handle coworker.spawn RPC method.
async fn handle_coworker_spawn(
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
async fn handle_coworker_break(id: RequestId, name: &str, state: &DaemonState) -> Response {
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
fn handle_coworker_list(id: RequestId, state: &DaemonState) -> Response {
    // Build a map of coworker name -> task subject from in_progress tasks
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
            // Look up current task from task storage (case-insensitive)
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
///
/// Returns the recent output from a headless coworker session by reading
/// the JSONL log file.
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
/// sent on break. When they report `Completed`, task cleanup is handled.
async fn handle_coworker_report_state(
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
    // Post question to channel
    let msg = Message::text(name, format!("Question for Lead: {}", question));
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        error!("Failed to post question to channel: {}", e);
    }

    // Mark the coworker as waiting for feedback in tmux tab and nudge the Lead.
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
// Insight handler
// ============================================================================

/// Handle insight.report RPC method.
///
/// Deduplicates via in-memory hash set, posts the insight to the channel,
/// and spawns a headless architect session to optionally generate a diagram.
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

    // Determine working directory for the architect session.
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

// ============================================================================
// Headless handler
// ============================================================================

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
// Reminder handlers
// ============================================================================

/// Handle reminder.create RPC method.
async fn handle_reminder_create(id: RequestId, message: &str, state: &DaemonState) -> Response {
    let mut ps = state.persistent_state.lock().await;
    let reminder_id = ps.reminders.add(
        crate::reminders::ReminderTrigger::AllWorkMerged,
        message.to_string(),
    );

    if let Err(e) = ps.save_for_repo(&state.repo_name) {
        error!("Failed to save daemon-state.json: {}", e);
    }

    let confirmation = format!(
        "Reminder set (id: {}): I'll notify you when all tasks are completed and all PRs are merged. Message: \"{}\"",
        reminder_id, message
    );
    info!("{}", confirmation);
    Response::success(id, serde_json::json!({ "message": confirmation }))
}

/// Handle reminder.list RPC method.
async fn handle_reminder_list(id: RequestId, state: &DaemonState) -> Response {
    let ps = state.persistent_state.lock().await;
    let active = ps.reminders.active();

    if active.is_empty() {
        return Response::success(id, serde_json::json!({ "message": "No active reminders." }));
    }

    let lines: Vec<String> = active
        .iter()
        .map(|r| {
            format!(
                "  {} [{}] \"{}\" (created {})",
                r.id,
                r.trigger,
                r.message,
                r.created_at.format("%Y-%m-%d %H:%M UTC")
            )
        })
        .collect();

    let output = format!("Active reminders:\n{}", lines.join("\n"));
    Response::success(id, serde_json::json!({ "message": output }))
}

/// Handle reminder.cancel RPC method.
async fn handle_reminder_cancel(id: RequestId, reminder_id: &str, state: &DaemonState) -> Response {
    let mut ps = state.persistent_state.lock().await;
    if ps.reminders.cancel(reminder_id) {
        if let Err(e) = ps.save_for_repo(&state.repo_name) {
            error!("Failed to save daemon-state.json: {}", e);
        }
        let msg = format!("Reminder {} cancelled.", reminder_id);
        info!("{}", msg);
        Response::success(id, serde_json::json!({ "message": msg }))
    } else {
        Response::error(
            id,
            RpcError::new(-32602, format!("Reminder '{}' not found", reminder_id)),
        )
    }
}

// ============================================================================
// Snapshot handler
// ============================================================================

/// Handle snapshot RPC method — collect and return the full WorldSnapshot.
async fn handle_snapshot(id: RequestId, state: &DaemonState) -> Response {
    let default_channel = match state.channel_router.default_channel() {
        Ok(ch) => ch,
        Err(e) => {
            error!("Failed to get default channel for snapshot: {}", e);
            return Response::error(id, RpcError::new(-32603, e.to_string()));
        }
    };
    let snapshot = super::snapshot::collect_world_snapshot(state)
        .await
        .with_debug_context(&default_channel);
    match serde_json::to_value(&snapshot) {
        Ok(value) => Response::success(id, value),
        Err(e) => Response::error(
            id,
            RpcError::new(-32603, format!("Failed to serialize snapshot: {}", e)),
        ),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[path = "rpc_tests.rs"]
#[cfg(test)]
mod tests;
