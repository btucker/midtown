//! RPC request handlers for the daemon's Unix socket protocol.
//!
//! This module is the entry point for JSON-RPC dispatch. It routes requests to
//! domain-specific handler modules:
//!
//! - `rpc_auth` — authentication switching
//! - `rpc_channel` — channel post/read/create/archive/list
//! - `rpc_coworker` — coworker lifecycle (spawn, break, list, view, state, nudge)
//! - `rpc_headless` — headless execution and snapshot
//! - `rpc_headed` — headed wrapper intercom (register/poll/ack)
//! - `rpc_insight` — insight reporting and deduplication
//! - `rpc_kanban` — kanban board data (legacy combined endpoint)
//! - `rpc_prs` — PR data for kanban board (`prs.status`)
//! - `rpc_reminder` — reminder CRUD
//! - `rpc_session` — session resolve/attach/detach/list
//! - `rpc_status` — daemon status overview
//! - `rpc_task` — task CRUD operations

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::rpc::{Request, RequestId, Response, RpcError};

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
    fn usize_param(&self, key: &str) -> Option<usize>;
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

    fn usize_param(&self, key: &str) -> Option<usize> {
        self.and_then(|p| p.get(key))
            .and_then(|v| v.as_u64())
            .and_then(|n| usize::try_from(n).ok())
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

/// Parse an optional auth provider from RPC params without applying a default.
fn parse_optional_provider_param(
    params: Option<&serde_json::Value>,
) -> Result<Option<crate::auth::AuthProvider>, String> {
    params
        .and_then(|p| p.get("provider"))
        .and_then(|v| v.as_str())
        .map(str::parse::<crate::auth::AuthProvider>)
        .transpose()
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
    // kanban.data, prs.status, and coworkers.status all use TTL caches or serve live data —
    // the idempotency cache (keyed by request ID) would shadow them incorrectly because the
    // web server reuses id=1 for repeated polls.
    let skip_rpc_cache = matches!(
        request_method.as_str(),
        "kanban.data" | "prs.status" | "coworkers.status"
    );

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
/// function in a dedicated `rpc_*` module.
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
            Response::success(request.id, serde_json::json!({"message": "shutting_down"}))
        }

        "daemon.exec-restart" => {
            info!("Exec-restart requested via RPC — daemon will re-exec after shutdown");
            state
                .restart_requested
                .store(true, std::sync::atomic::Ordering::SeqCst);
            // Trigger the shutdown broadcast so the main event loop exits.
            // The run() function checks restart_requested and returns ExecRestart.
            let _ = state.shutdown_tx.send(());
            Response::success(request.id, serde_json::json!({"message": "restarting"}))
        }

        "daemon.set-draining" => {
            info!("set-draining requested via RPC — blocking new task assignments");
            state
                .draining
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Response::success(request.id, serde_json::json!({"message": "draining"}))
        }

        "coworker.stop_all" => {
            info!("stop_all requested via RPC — sending SIGTERM to all headless sessions");
            // Set draining flag before shutdown to prevent new task assignments
            // during the SIGTERM wait window (TaskDispatchTick may fire during the wait).
            state
                .draining
                .store(true, std::sync::atomic::Ordering::SeqCst);
            // Send SIGTERM to all running coworker sessions and wait up to 10s for them to exit.
            let count = state
                .session_manager
                .graceful_shutdown_all(std::time::Duration::from_secs(10))
                .await;
            Response::success(
                request.id,
                serde_json::json!({"message": "stopped", "count": count}),
            )
        }

        "daemon.check-pending" => {
            info!("Check-pending triggered via RPC");
            let snap = snapshot::collect_world_snapshot(state).await;
            let pending_effects = super::dispatch::spawn_for_pending_tasks(&snap, state);
            state.mark_in_flight_spawns_from_effects(&pending_effects);
            effects::execute_effects(pending_effects, state).await;
            Response::success(request.id, serde_json::json!({"message": "ok"}))
        }

        // ---- Snapshot / headless ----
        "snapshot" => super::rpc_headless::handle_snapshot(request.id, state).await,

        "headless.execute" => {
            let prompt = require_str!(params, "prompt", request.id);
            let auth_provider = match parse_optional_provider_param(params) {
                Ok(Some(provider)) => provider,
                Ok(None) => crate::config::get_execution_provider_for_role(
                    &state.repo_name,
                    crate::config::ExecutionRole::HeadlessExecute,
                ),
                Err(msg) => return Response::error(request.id, RpcError::new(-32602, msg)),
            };
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
                project_name: Some(state.repo_name.clone()),
                persist_session: false,
                resume_session_id: None,
                inactivity_timeout: None,
                team_name: None,
                agent_id: None,
                agent_name: None,
                settings_path: None,
                setting_sources: None,
                auth_provider,
                env: std::collections::BTreeMap::new(),
            };
            super::rpc_headless::handle_headless_execute(request.id, prompt, &config).await
        }

        // ---- Coworker lifecycle ----
        "coworker.spawn" => {
            let resume = params.bool_or("resume", false);
            let prompt = params.str_param("prompt").map(|s| s.to_string());
            let provider = match parse_optional_provider_param(params) {
                Ok(Some(provider)) => provider,
                Ok(None) => crate::config::get_execution_provider_for_role(
                    &state.repo_name,
                    crate::config::ExecutionRole::Coworker,
                ),
                Err(msg) => return Response::error(request.id, RpcError::new(-32602, msg)),
            };
            super::rpc_coworker::handle_coworker_spawn(request.id, state, resume, prompt, provider)
                .await
        }

        "coworker.break" => {
            let name = require_str!(params, "name", request.id);
            super::rpc_coworker::handle_coworker_break(request.id, name, state).await
        }

        "coworker.list" => super::rpc_coworker::handle_coworker_list(request.id, state).await,

        "coworker.view" => {
            let name = require_str!(params, "name", request.id);
            super::rpc_coworker::handle_coworker_view(request.id, name, state).await
        }

        "coworker.report-state" => {
            let name = require_str!(params, "name", request.id);
            let phase = require_str!(params, "phase", request.id);
            let task_id = params.u64_param("task_id").map(|v| v as u32);
            let progress = params.u64_param("progress").map(|v| v as u8);
            super::rpc_coworker::handle_coworker_report_state(
                request.id, name, phase, task_id, progress, state,
            )
            .await
        }

        "coworker.nudge" => {
            let name = require_str!(params, "name", request.id);
            let message = require_str!(params, "message", request.id);
            let from: &str = params.str_param("from").unwrap_or(state.repo_name.as_str());
            super::rpc_coworker::handle_coworker_nudge(request.id, from, name, message, state).await
        }

        "coworker.asking" => {
            let name = require_str!(params, "name", request.id);
            let question = require_str!(params, "question", request.id);
            super::rpc_coworker::handle_coworker_asking(request.id, name, question, state).await
        }

        "coworker.questions" => {
            super::rpc_coworker::handle_coworker_questions(request.id, state).await
        }

        // ---- Lead lifecycle ----
        "lead.spawn" => {
            let provider = match parse_optional_provider_param(params) {
                Ok(Some(provider)) => provider,
                Ok(None) => crate::config::get_execution_provider_for_role(
                    &state.repo_name,
                    crate::config::ExecutionRole::Lead,
                ),
                Err(msg) => return Response::error(request.id, RpcError::new(-32602, msg)),
            };
            super::rpc_coworker::handle_lead_spawn(request.id, state, provider).await
        }

        // ---- Status / kanban ----
        "status" => super::rpc_status::handle_status(request.id, state).await,

        "kanban.data" => super::rpc_kanban::handle_kanban_data(request.id, state).await,

        "prs.status" => super::rpc_prs::handle_prs_status(request.id, state).await,

        "coworkers.status" => super::rpc_coworker::handle_coworkers_status(request.id, state).await,

        // ---- Channel ----
        "channel.post" => {
            let message = require_str!(params, "message", request.id);
            let from: &str = params.str_param("from").unwrap_or(state.repo_name.as_str());
            let channel = params.str_param("channel");
            let thread_parent_id = params.str_param("thread_parent_id");
            super::rpc_channel::handle_channel_post(
                request.id,
                from,
                message,
                channel,
                thread_parent_id,
                state,
            )
            .await
        }

        "channel.read" => {
            let all = params.bool_or("all", false);
            let last = params.usize_param("last");
            let since = params.str_param("since");
            let channel = params.str_param("channel");
            super::rpc_channel::handle_channel_read(request.id, all, last, since, channel, state)
        }

        "channel.list" => {
            let include_archived = params.bool_or("include_archived", false);
            super::rpc_channel::handle_channel_list(request.id, include_archived, state)
        }

        "channel.create" => {
            let name = require_str!(params, "name", request.id);
            super::rpc_channel::handle_channel_create(request.id, name, state)
        }

        "channel.archive" => {
            let name = require_str!(params, "name", request.id);
            super::rpc_channel::handle_channel_archive(request.id, name, state)
        }

        // ---- Tasks ----
        "task.create" => {
            let subject = require_str!(params, "subject", request.id);
            let description = params.str_or("description", "");
            let blocked_by = params.str_array_param("blocked_by");
            let channel = params.str_param("channel");
            let model = params.str_param("model");
            let pr = params.u64_param("pr");
            let plan = params.str_param("plan");
            let execution_skill = params.str_param("execution_skill");
            super::rpc_task::handle_task_create(
                request.id,
                subject,
                description,
                blocked_by.as_deref(),
                channel,
                model,
                pr,
                plan,
                execution_skill,
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
            super::rpc_task::handle_task_done(request.id, task_id, state).await
        }

        "task.metadata" => {
            let task_id = require_str!(params, "id", request.id);
            super::rpc_task::handle_task_metadata(request.id, task_id, state).await
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
                super::rpc_reminder::handle_reminder_create(request.id, message, state).await
            }
        }

        "reminder.list" => super::rpc_reminder::handle_reminder_list(request.id, state).await,

        "reminder.cancel" => {
            let reminder_id = require_str!(params, "id", request.id);
            super::rpc_reminder::handle_reminder_cancel(request.id, reminder_id, state).await
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
            super::rpc_insight::handle_insight_report(request.id, agent, insight, channel, state)
                .await
        }

        // ---- Sessions ----
        "session.resolve" => {
            let target = require_str!(params, "target", request.id);
            super::rpc_session::handle_session_resolve(request.id, target, state).await
        }

        "session.attach" => {
            let target = require_str!(params, "target", request.id);
            super::rpc_session::handle_session_attach(request.id, target, state).await
        }

        "session.detach" => {
            let name = require_str!(params, "name", request.id);
            super::rpc_session::handle_session_detach(request.id, name, state).await
        }

        "session.list" => super::rpc_session::handle_session_list(request.id, state).await,

        "session.view" => {
            let target = require_str!(params, "target", request.id);
            super::rpc_session::handle_session_view(request.id, target, state).await
        }

        "session.clear" => {
            let target = require_str!(params, "target", request.id);
            super::rpc_session::handle_session_clear(request.id, target, state).await
        }

        // ---- Headed wrapper intercom ----
        "headed.register" => {
            let session = require_str!(params, "session", request.id);
            let adapter_id = require_str!(params, "adapter_id", request.id);
            let provider = match parse_provider_param(params) {
                Ok(provider) => provider,
                Err(msg) => return Response::error(request.id, RpcError::new(-32602, msg)),
            };
            super::rpc_headed::handle_register(request.id, session, adapter_id, provider, state)
                .await
        }

        "headed.unregister" => {
            let session = require_str!(params, "session", request.id);
            let adapter_id = require_str!(params, "adapter_id", request.id);
            super::rpc_headed::handle_unregister(request.id, session, adapter_id, state).await
        }

        "headed.heartbeat" => {
            let session = require_str!(params, "session", request.id);
            let adapter_id = require_str!(params, "adapter_id", request.id);
            super::rpc_headed::handle_heartbeat(request.id, session, adapter_id, state).await
        }

        "headed.poll" => {
            let session = require_str!(params, "session", request.id);
            let adapter_id = require_str!(params, "adapter_id", request.id);
            let after_id = params.u64_param("after_id").unwrap_or(0);
            let limit = params.u64_param("limit").map(|v| v as usize);
            super::rpc_headed::handle_poll(request.id, session, adapter_id, after_id, limit, state)
                .await
        }

        "headed.ack" => {
            let session = require_str!(params, "session", request.id);
            let adapter_id = require_str!(params, "adapter_id", request.id);
            let Some(msg_id) = params.u64_param("msg_id") else {
                return Response::error(request.id, RpcError::invalid_params());
            };
            super::rpc_headed::handle_ack(request.id, session, adapter_id, msg_id, state).await
        }

        "headed.output" => {
            let session = require_str!(params, "session", request.id);
            let output = require_str!(params, "output", request.id);
            super::rpc_headed::handle_output(request.id, session, output, state).await
        }

        "headed.enqueue" => {
            let session = require_str!(params, "session", request.id);
            let text = require_str!(params, "text", request.id);
            super::rpc_headed::handle_enqueue(request.id, session, text, state).await
        }

        _ => {
            warn!("Unknown method: {}", request.method);
            Response::error(request.id, RpcError::method_not_found())
        }
    }
}

#[path = "rpc_tests.rs"]
#[cfg(test)]
mod tests;
