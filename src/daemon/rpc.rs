//! RPC request handlers for the daemon's Unix socket protocol.
//!
//! Each `handle_*` function processes a specific JSON-RPC method and returns
//! a `Response`. The entry point is `handle_connection`, which reads requests
//! from a Unix stream and dispatches them via `handle_request`.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::message::{Message, MessageType};
use crate::rpc::{Request, RequestId, Response, RpcError};

use super::constants::*;
use super::helpers::*;
use super::{DaemonState, effects, snapshot};

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
            // Found a PR associated with this task
            // The PR is still in pr_author_sessions, which means it's open
            // (closed PRs are cleaned up by cleanup_closed_pr_state)
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
            let params = request.params.as_ref();
            let resume = params
                .and_then(|p| p.get("resume"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let prompt = params
                .and_then(|p| p.get("prompt"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let provider = match parse_provider_param(params) {
                Ok(provider) => provider,
                Err(msg) => return Response::error(request.id, RpcError::new(-32602, msg)),
            };
            handle_coworker_spawn(request.id, state, resume, prompt, provider).await
        }

        "coworker.break" => {
            let name = request
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str());

            match name {
                Some(name) => handle_coworker_break(request.id, name, state).await,
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "coworker.list" => handle_coworker_list(request.id, state),

        "coworker.view" => {
            let name = request
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str());

            match name {
                Some(name) => handle_coworker_view(request.id, name, state).await,
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "coworker.report-state" => {
            let params = request.params.as_ref();
            let name = params.and_then(|p| p.get("name")).and_then(|v| v.as_str());
            let phase = params.and_then(|p| p.get("phase")).and_then(|v| v.as_str());
            let task_id = params
                .and_then(|p| p.get("task_id"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);

            match (name, phase) {
                (Some(name), Some(phase)) => {
                    handle_coworker_report_state(request.id, name, phase, task_id, state).await
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "coworker.nudge" => {
            let params = request.params.as_ref();
            let name = params.and_then(|p| p.get("name")).and_then(|v| v.as_str());
            let message = params
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str());
            let from = params
                .and_then(|p| p.get("from"))
                .and_then(|v| v.as_str())
                .unwrap_or("lead");

            match (name, message) {
                (Some(name), Some(message)) => {
                    handle_coworker_nudge(request.id, from, name, message, state).await
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "coworker.asking" => {
            let params = request.params.as_ref();
            let name = params.and_then(|p| p.get("name")).and_then(|v| v.as_str());
            let question = params
                .and_then(|p| p.get("question"))
                .and_then(|v| v.as_str());

            match (name, question) {
                (Some(name), Some(question)) => {
                    handle_coworker_asking(request.id, name, question, state).await
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "status" => handle_status(request.id, state).await,

        "kanban.data" => handle_kanban_data(request.id, state).await,

        "channel.post" => {
            let params = request.params.as_ref();
            let message = params
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str());
            let from = params
                .and_then(|p| p.get("from"))
                .and_then(|v| v.as_str())
                .unwrap_or("lead");
            let channel = params
                .and_then(|p| p.get("channel"))
                .and_then(|v| v.as_str());

            match message {
                Some(msg) => handle_channel_post(request.id, from, msg, channel, state).await,
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "channel.read" => {
            let all = request
                .params
                .as_ref()
                .and_then(|p| p.get("all"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            handle_channel_read(request.id, all, state)
        }

        "reminder.create" => {
            let params = request.params.as_ref();
            let trigger = params
                .and_then(|p| p.get("trigger"))
                .and_then(|v| v.as_str());
            let message = params
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str());

            match (trigger, message) {
                (Some("all-work-merged"), Some(msg)) => {
                    handle_reminder_create(request.id, msg, state).await
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "reminder.list" => handle_reminder_list(request.id, state).await,

        "reminder.cancel" => {
            let id = request
                .params
                .as_ref()
                .and_then(|p| p.get("id"))
                .and_then(|v| v.as_str());

            match id {
                Some(id) => handle_reminder_cancel(request.id, id, state).await,
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "daemon.check-pending" => {
            info!("Check-pending triggered via RPC");
            let snap = snapshot::collect_world_snapshot(state).await;
            let pending_effects = super::dispatch::spawn_for_pending_tasks(&snap, state);
            // Mark in-flight tasks BEFORE executing effects to prevent race conditions.
            // Without this, a TaskDispatchTick firing while effects execute would see
            // the task as still pending and generate a duplicate AssignAndSpawn.
            state.mark_in_flight_spawns_from_effects(&pending_effects);
            effects::execute_effects(pending_effects, state).await;
            Response::success(request.id, serde_json::json!({"status": "ok"}))
        }

        "task.create" => {
            let params = request.params.as_ref();
            let subject = params
                .and_then(|p| p.get("subject"))
                .and_then(|v| v.as_str());
            let description = params
                .and_then(|p| p.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let blocked_by: Option<Vec<String>> = params
                .and_then(|p| p.get("blocked_by"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                });
            let channel = params
                .and_then(|p| p.get("channel"))
                .and_then(|v| v.as_str());
            let model = params.and_then(|p| p.get("model")).and_then(|v| v.as_str());

            match subject {
                Some(subject) => {
                    handle_task_create(
                        request.id,
                        subject,
                        description,
                        blocked_by.as_deref(),
                        channel,
                        model,
                        state,
                    )
                    .await
                }
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "task.update" => {
            let params = request.params.as_ref();
            let id = params.and_then(|p| p.get("id")).and_then(|v| v.as_str());

            match id {
                Some(id) => {
                    let owner = params.and_then(|p| p.get("owner")).and_then(|v| v.as_str());
                    let status = params
                        .and_then(|p| p.get("status"))
                        .and_then(|v| v.as_str());
                    let description = params
                        .and_then(|p| p.get("description"))
                        .and_then(|v| v.as_str());
                    let blocked_by: Option<Vec<String>> = params
                        .and_then(|p| p.get("blocked_by"))
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        });
                    let channel = params
                        .and_then(|p| p.get("channel"))
                        .and_then(|v| v.as_str());
                    let model = params.and_then(|p| p.get("model")).and_then(|v| v.as_str());

                    handle_task_update(
                        request.id,
                        id,
                        owner,
                        status,
                        description,
                        blocked_by.as_deref(),
                        channel,
                        model,
                        state,
                    )
                }
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "task.done" => {
            let params = request.params.as_ref();
            let id = params.and_then(|p| p.get("id")).and_then(|v| v.as_str());

            match id {
                Some(id) => handle_task_done(request.id, id, state),
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "task.metadata" => {
            let params = request.params.as_ref();
            let id = params.and_then(|p| p.get("id")).and_then(|v| v.as_str());

            match id {
                Some(id) => handle_task_metadata(request.id, id, state),
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "task.request" => {
            let params = request.params.as_ref();
            let message = params
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str());
            let from = params
                .and_then(|p| p.get("from"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            match message {
                Some(msg) => handle_task_request(request.id, from, msg, state).await,
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "task.claim" => {
            let params = request.params.as_ref();
            let id = params.and_then(|p| p.get("id")).and_then(|v| v.as_str());
            let from = params
                .and_then(|p| p.get("from"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            match id {
                Some(id) => handle_task_claim(request.id, id, from, state),
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "auth.switch" => {
            let params = request.params.as_ref();
            let profile = params
                .and_then(|p| p.get("profile"))
                .and_then(|v| v.as_str());
            let all = params
                .and_then(|p| p.get("all"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
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
            let params = request.params.as_ref();
            let agent = params.and_then(|p| p.get("agent")).and_then(|v| v.as_str());
            let insight = params
                .and_then(|p| p.get("insight"))
                .and_then(|v| v.as_str());
            let channel = params
                .and_then(|p| p.get("channel"))
                .and_then(|v| v.as_str());

            match (agent, insight) {
                (Some(agent), Some(insight)) => {
                    handle_insight_report(request.id, agent, insight, channel, state).await
                }
                _ => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "headless.execute" => {
            let params = request.params.as_ref();
            let prompt = params
                .and_then(|p| p.get("prompt"))
                .and_then(|v| v.as_str());

            match prompt {
                Some(prompt) => {
                    let config = crate::headless::HeadlessConfig {
                        model: params
                            .and_then(|p| p.get("model"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("sonnet")
                            .to_string(),
                        system_prompt: params
                            .and_then(|p| p.get("system_prompt"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        json_schema: params.and_then(|p| p.get("json_schema")).cloned(),
                        max_budget_usd: params
                            .and_then(|p| p.get("max_budget_usd"))
                            .and_then(|v| v.as_f64()),
                        allow_tools: params
                            .and_then(|p| p.get("allow_tools"))
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
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
            // Collect and return the full WorldSnapshot for debugging/testing.
            // Debug context (channel messages, daemon logs) is only populated here,
            // not during normal tick collection, to avoid I/O overhead on the hot path.
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

        "session.attach" => {
            let target = request
                .params
                .as_ref()
                .and_then(|p| p.get("target"))
                .and_then(|v| v.as_str());

            match target {
                Some(target) => handle_session_attach(request.id, target, state).await,
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "session.detach" => {
            let name = request
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str());

            match name {
                Some(name) => handle_session_detach(request.id, name, state).await,
                None => Response::error(request.id, RpcError::invalid_params()),
            }
        }

        "session.list" => handle_session_list(request.id, state).await,

        _ => {
            warn!("Unknown method: {}", request.method);
            Response::error(request.id, RpcError::method_not_found())
        }
    };

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

// ============================================================================
// Individual RPC handlers
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
///
/// Switches the active auth profile.
///
/// For Claude, also re-launches active sessions:
/// 1. Validates and switches the profile on disk (project or global)
/// 2. Shuts down all running coworkers (daemon will re-spawn for pending tasks)
/// 3. Re-launches the lead window with the new credentials
async fn handle_auth_switch(
    id: RequestId,
    profile: &str,
    all: bool,
    provider: crate::auth::AuthProvider,
    state: &DaemonState,
) -> Response {
    // Validate the profile name format (defense-in-depth — CLI also validates)
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
        // For per-project switch, check the project config's auth_profile (not the effective profile)
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
        // Global switch: update global current profile and clear per-project overrides.
        // Even when the global profile already matches, we must still clear overrides
        // so projects stop shadowing the global setting.
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
        // Per-project switch: update this project's config
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
                // Only clear records on successful shutdown
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

    // Capture reviewer assignments before relaunch so reviewer coworkers can
    // be re-spawned with the reviewer role/prompt.
    let reviewer_pr_by_name: HashMap<String, u64> = {
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

    // Re-launch lead only when switching the provider backing the interactive
    // lead session. Today lead is Claude-backed; other providers leave lead
    // untouched instead of reporting a relaunch failure.
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

    // Re-launch all sessions for this provider using the updated auth profile.
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

/// Handle headless.execute RPC method.
///
/// Spawns a headless Claude Code session and runs a one-shot prompt. Returns
/// the final result with cost and duration. The session uses JSON streaming
/// internally but this RPC endpoint blocks until the result is available.
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

    // Default timeout of 5 minutes for RPC-invoked headless sessions
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

/// Handle insight.report RPC method.
///
/// Called by the insight PostToolUse hook when a coworker or lead generates
/// an insight block. Deduplicates via in-memory hash set, posts the insight
/// to the channel, and spawns a headless architect session to optionally
/// generate a Mermaid diagram.
///
/// The optional `channel` parameter specifies which channel to post the insight to.
/// If None, defaults to the main channel. Architect diagrams are only posted when
/// `channel` is Some (topic channel) — diagrams are skipped for the main channel
/// to avoid noise.
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
    // For coworkers, use their worktree; for lead, use the main repo dir.
    let cwd = if is_coworker_sender(agent) {
        let worktree = crate::paths::coworkers_dir_for_repo(&state.repo_name).join(agent);
        if worktree.exists() {
            worktree
        } else {
            // Worktree gone — fall back to main repo dir
            state.all_repo_paths.first().cloned().unwrap_or_default()
        }
    } else {
        state.all_repo_paths.first().cloned().unwrap_or_default()
    };

    // Spawn the architect task asynchronously - pass channel so diagram routes to same channel as insight
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
///
/// Normalizes text (trim, collapse whitespace, lowercase) before hashing
/// to prevent duplicates from minor formatting variations.
fn hash_insight(insight: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let normalized: String = insight
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    hasher.finish()
}

/// Extract PR number from a `[Review Note] PR #123: ...` message.
///
/// Returns `Some(pr_number)` if the message contains the review note pattern,
/// `None` otherwise. Used for per-reviewer per-PR deduplication.
fn extract_review_note_pr(message: &str) -> Option<u64> {
    // Match "[Review Note]" followed by "PR #" and a number
    let review_note_idx = message.find("[Review Note]")?;
    let after = &message[review_note_idx..];
    let pr_hash_idx = after.find("PR #").or_else(|| after.find("pr #"))?;
    let after_hash = &after[pr_hash_idx + 4..];
    let num_str: String = after_hash
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    num_str.parse().ok()
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
/// the JSONL log file. This enables `midtown coworker view` to work with
/// headless coworkers.
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
/// tmux tab display. The daemon is the single authority for coworker state.
///
/// When a coworker reports `Idle`, they are immediately sent on break.
/// This eliminates the race between idle detection (daemon tick) and stuck
/// detection (pane unchanged), which could cause idle coworkers to be
/// incorrectly restarted as "stuck".
async fn handle_coworker_report_state(
    id: RequestId,
    name: &str,
    phase_str: &str,
    task_id: Option<u32>,
    state: &DaemonState,
) -> Response {
    // Parse the phase string into a WorkflowPhase enum
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

    // For Completed phase, sync the shared task list so the daemon stops
    // reassigning the task. Coworker completions write to their isolated list,
    // not the shared list the daemon reads. This closes the loop by updating the
    // shared list when a coworker reports completion via RPC.
    //
    // IMPORTANT: Tasks that produce PRs complete on MERGE, not on coworker phase
    // transition. Only auto-complete tasks that DON'T have associated open PRs.
    // This keeps coworkers alive through the review cycle so feedback can be
    // delivered to the same session (tier 1 routing).
    //
    // Uses the existing Effect::CompleteTask and Effect::ClearBlockedBy variants
    // to stay consistent with the effect-based architecture and avoid duplicating
    // cleanup logic (e.g., clear_task_assignment_by_task is handled by the effect
    // executor in effects.rs).
    if phase == crate::coworker_state::WorkflowPhase::Completed {
        // Determine the task to complete: use the explicitly provided task_id,
        // or fall back to the task tracked in the daemon's in-memory assignment map.
        let effective_task_id: Option<String> = task_id.map(|id| id.to_string()).or_else(|| {
            let assignments = state.coworker_task_assignments.lock().unwrap();
            assignments
                .get(&name.to_lowercase())
                .map(|a| a.task_id.clone())
        });

        if let Some(ref tid) = effective_task_id {
            // Check if the task has an associated open PR. If it does, defer
            // completion to the PR merge path (dispatch.rs).
            let has_open_pr = task_has_open_pr(tid, state).await;

            if has_open_pr {
                debug!(
                    "Task !{} has open PR, deferring completion to merge path",
                    tid
                );
            } else {
                // No open PR — the coworker reported Completed prematurely
                // (before opening a PR). Don't complete the task; nudge the
                // coworker to open a PR and go idle. The daemon will complete
                // the task when the PR merges.
                //
                // Non-PR tasks (reviews, investigations) should use
                // `midtown task done <id>` directly instead of reporting
                // WorkflowPhase::Completed.
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
///
/// Sends the nudge directly to the coworker's tmux window without posting to the channel,
/// to avoid the chat monitor seeing the @mention and creating a duplicate nudge.
async fn handle_coworker_nudge(
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

/// Handle task.request RPC — a coworker surfaces work that should be a separate task.
///
/// Posts a formatted message to the channel so the lead can see the request
/// and decide whether to create a task for it.
async fn handle_task_request(
    id: RequestId,
    from: &str,
    message: &str,
    state: &DaemonState,
) -> Response {
    let channel_message = format!("@lead [Task Request] from {}: \"{}\"", from, message);

    let msg = Message::new("midtown", channel_message.clone(), MessageType::Text);

    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        warn!("Failed to post task request to channel: {}", e);
        return Response::error(id, RpcError::new(-32603, format!("Failed to post: {}", e)));
    }

    info!("Task request from {}: {}", from, message);
    Response::success(
        id,
        serde_json::json!({
            "posted": true,
            "from": from,
        }),
    )
}

/// Handle task.create RPC — daemon creates a task directly in shared storage.
///
/// Only performs I/O (write task, post to channel). Dispatch for the new task
/// happens on the next `TaskDispatchTick` via the canonical event loop pipeline.
///
/// If no channel is provided, invokes the clusterer to assign one automatically.
async fn handle_task_create(
    id: RequestId,
    subject: &str,
    description: &str,
    blocked_by: Option<&[String]>,
    channel: Option<&str>,
    model: Option<&str>,
    state: &DaemonState,
) -> Response {
    let repo_name = state.repo_name.clone();

    // Generate active_form (present continuous) from subject for task UI spinner
    let active_form = generate_active_form(subject);

    // If no channel was provided, invoke clusterer to assign one
    let assigned_channel = if channel.is_none() {
        match invoke_clusterer_for_task(subject, description, state).await {
            Ok(ch) => Some(ch),
            Err(e) => {
                warn!(
                    "Clusterer failed to assign channel: {} — using 'midtown' as fallback",
                    e
                );
                Some("midtown".to_string())
            }
        }
    } else {
        channel.map(String::from)
    };

    match crate::tasks::create_task_for_repo(
        subject,
        description,
        &active_form,
        "",
        &repo_name,
        blocked_by,
        assigned_channel.as_deref(),
    ) {
        Ok(task_id) => {
            // Update daemon-side task-to-channel and task-to-model mappings if provided
            {
                let mut ps = state.persistent_state.lock().await;
                let mut needs_save = false;

                // Apply channel mapping
                if apply_task_channel_mapping(
                    &mut ps.task_channel,
                    &task_id.to_string(),
                    channel,
                    false,
                ) {
                    needs_save = true;
                }

                // Apply model mapping
                match apply_task_model_mapping(
                    &mut ps.task_model,
                    &task_id.to_string(),
                    model,
                    false,
                ) {
                    Ok(changed) => {
                        if changed {
                            needs_save = true;
                        }
                    }
                    Err(e) => {
                        // Model format validation failed - return error
                        return Response::error(id, RpcError::new(-32602, e));
                    }
                }

                // Save if any mapping changed
                if needs_save && let Err(e) = ps.save_for_repo(&repo_name) {
                    warn!("Failed to save task mappings: {}", e);
                }
            }

            // Post to channel so team is aware
            let msg = Message::text("lead", format!("created task: {}", subject));
            if let Err(e) = state.send_and_broadcast_async(&msg).await {
                warn!("Failed to post task creation to channel: {}", e);
            }

            info!("Created task !{}: {}", task_id, subject);
            Response::success(
                id,
                serde_json::json!({
                    "type": "message",
                    "message": format!("Task !{} created: {}", task_id, subject),
                }),
            )
        }
        Err(e) => Response::error(
            id,
            RpcError::new(-32603, format!("Failed to create task: {}", e)),
        ),
    }
}

/// Generate a present-continuous `activeForm` from a task subject.
///
/// Converts imperative subjects like "Fix auth bug" → "Fixing auth bug".
/// Falls back to "Working on: <subject>" for unrecognized patterns.
fn generate_active_form(subject: &str) -> String {
    let trimmed = subject.trim();
    let first_word = trimmed.split_whitespace().next().unwrap_or("");
    let rest = trimmed.strip_prefix(first_word).unwrap_or("").trim_start();

    // Common imperative verbs → present continuous
    let continuous = match first_word.to_lowercase().as_str() {
        "add" => "Adding",
        "fix" => "Fixing",
        "update" => "Updating",
        "remove" => "Removing",
        "implement" => "Implementing",
        "refactor" => "Refactoring",
        "create" => "Creating",
        "build" => "Building",
        "review" => "Reviewing",
        "address" => "Addressing",
        "debug" => "Debugging",
        "test" => "Testing",
        "move" => "Moving",
        "rename" => "Renaming",
        "delete" => "Deleting",
        "replace" => "Replacing",
        "revert" => "Reverting",
        "migrate" => "Migrating",
        "upgrade" => "Upgrading",
        "clean" => "Cleaning",
        "configure" => "Configuring",
        "enable" => "Enabling",
        "disable" => "Disabling",
        _ => return format!("Working on: {}", trimmed),
    };

    if rest.is_empty() {
        continuous.to_string()
    } else {
        format!("{} {}", continuous, rest)
    }
}

/// Validate model format: must be "provider/model" with exactly one slash.
///
/// Valid examples: "claude/opus", "claude/sonnet", "codex/o3", "codex/o4-mini"
/// Invalid: "claude-opus" (no slash), "claude/opus/extra" (multiple slashes),
///          "/opus" (empty provider), "claude/" (empty model)
fn validate_model_format(model: &str) -> Result<(), String> {
    let parts: Vec<&str> = model.split('/').collect();
    if parts.len() != 2 {
        return Err(format!(
            "Invalid model format '{}': must be '<provider>/<model>' (e.g., claude/opus)",
            model
        ));
    }
    if parts[0].is_empty() {
        return Err(format!(
            "Invalid model format '{}': provider cannot be empty",
            model
        ));
    }
    if parts[1].is_empty() {
        return Err(format!(
            "Invalid model format '{}': model cannot be empty",
            model
        ));
    }
    Ok(())
}

/// Apply a task-to-model mapping update to persistent state.
///
/// On `task.create`: pass `model` from the RPC params. Valid non-empty values are stored;
/// `None` or empty strings are ignored. Invalid formats return an error.
///
/// On `task.update`: pass `model` from the RPC params. Valid non-empty values set/overwrite
/// the mapping; an empty string clears it; `None` means no change.
///
/// Returns `Ok(true)` if the mapping was modified (caller should save persistent state).
/// Returns `Ok(false)` if no change was made.
/// Returns `Err` if the model format is invalid.
fn apply_task_model_mapping(
    task_model: &mut HashMap<String, String>,
    task_id: &str,
    model: Option<&str>,
    allow_clear: bool,
) -> Result<bool, String> {
    match model {
        Some(m) if m.is_empty() && allow_clear => {
            // Empty string means clear the mapping (only on update, not create)
            // Returns true only if a mapping was actually removed
            Ok(task_model.remove(task_id).is_some())
        }
        Some(m) if !m.is_empty() => {
            // Validate format before storing
            validate_model_format(m)?;
            task_model.insert(task_id.to_string(), m.to_string());
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Apply a task-to-channel mapping update to persistent state.
///
/// On `task.create`: pass `channel` from the RPC params. Non-empty values are stored;
/// `None` or empty strings are ignored.
///
/// On `task.update`: pass `channel` from the RPC params. Non-empty values set/overwrite
/// the mapping; an empty string clears it; `None` means no change.
///
/// Returns `true` if the mapping was modified (caller should save persistent state).
fn apply_task_channel_mapping(
    task_channel: &mut HashMap<String, String>,
    task_id: &str,
    channel: Option<&str>,
    allow_clear: bool,
) -> bool {
    match channel {
        Some(ch) if ch.is_empty() && allow_clear => {
            // Empty string means clear the mapping (only on update, not create)
            // Returns true only if a mapping was actually removed
            task_channel.remove(task_id).is_some()
        }
        Some(ch) if !ch.is_empty() => {
            task_channel.insert(task_id.to_string(), ch.to_string());
            true
        }
        _ => false,
    }
}

/// Handle task.update RPC — update specific fields on a task directly.
#[allow(clippy::too_many_arguments)]
fn handle_task_update(
    id: RequestId,
    task_id: &str,
    owner: Option<&str>,
    status: Option<&str>,
    description: Option<&str>,
    blocked_by: Option<&[String]>,
    channel: Option<&str>,
    model: Option<&str>,
    state: &DaemonState,
) -> Response {
    // Validate status if provided
    if let Some(s) = status
        && !["pending", "in_progress", "completed"].contains(&s)
    {
        return Response::error(id, RpcError::new(-32602, format!("Invalid status: {}", s)));
    }

    let repo_name = state.repo_name.clone();

    if let Err(e) = crate::tasks::update_task_fields_for_repo(
        task_id,
        &repo_name,
        owner,
        status,
        description,
        blocked_by,
        channel,
    ) {
        return Response::error(
            id,
            RpcError::new(-32603, format!("Failed to update task: {}", e)),
        );
    }

    // Update in-memory assignment tracking
    if let Some(new_owner) = owner {
        // Clear old assignment before recording new one (prevents stale entries
        // when a task is reassigned from coworker A to coworker B)
        state.clear_task_assignment_by_task(task_id);
        state.record_task_assignment(new_owner, task_id);
    }

    // Clear assignment when task is completed or reset to pending
    if matches!(status, Some("completed") | Some("pending")) {
        state.clear_task_assignment_by_task(task_id);
    }

    // Update daemon-side task-to-channel and task-to-model mappings
    {
        let mut ps = state.persistent_state.blocking_lock();
        let mut needs_save = false;

        // Apply channel mapping
        if apply_task_channel_mapping(&mut ps.task_channel, task_id, channel, true) {
            needs_save = true;
        }

        // Apply model mapping
        match apply_task_model_mapping(&mut ps.task_model, task_id, model, true) {
            Ok(changed) => {
                if changed {
                    needs_save = true;
                }
            }
            Err(e) => {
                // Model format validation failed - return error
                return Response::error(id, RpcError::new(-32602, e));
            }
        }

        // Save if any mapping changed
        if needs_save && let Err(e) = ps.save_for_repo(&repo_name) {
            warn!("Failed to save task mappings: {}", e);
        }
    }

    info!("Updated task !{}", task_id);
    let response = Response::success(
        id,
        serde_json::json!({
            "type": "message",
            "message": format!("Task !{} updated", task_id),
        }),
    );
    debug!("Returning response: {:?}", response);
    response
}

/// Handle task.done RPC — mark a task as completed directly.
fn handle_task_done(id: RequestId, task_id: &str, state: &DaemonState) -> Response {
    let repo_name = state.repo_name.clone();

    if let Err(e) = crate::tasks::complete_task_for_repo(task_id, &repo_name) {
        return Response::error(
            id,
            RpcError::new(-32603, format!("Failed to complete task: {}", e)),
        );
    }

    // Clear in-memory tracking
    state.clear_task_assignment_by_task(task_id);

    // Unblock dependent tasks
    if let Err(e) = crate::tasks::clear_blocked_by_for_repo(task_id, &repo_name) {
        warn!("Failed to clear blockedBy for task !{}: {}", task_id, e);
    }

    info!("Completed task !{}", task_id);
    Response::success(
        id,
        serde_json::json!({
            "type": "message",
            "message": format!("Task !{} completed", task_id),
        }),
    )
}

/// Handle task.metadata RPC — return daemon-side metadata for a task.
///
/// Returns channel and model mappings stored in DaemonPersistentState.
/// These are stored separately from Claude Code's native task storage.
fn handle_task_metadata(id: RequestId, task_id: &str, state: &DaemonState) -> Response {
    let ps = state.persistent_state.blocking_lock();
    let channel = ps.task_channel.get(task_id).cloned();
    let model = ps.task_model.get(task_id).cloned();

    Response::success(
        id,
        serde_json::json!({
            "channel": channel,
            "model": model,
        }),
    )
}

/// Handle task.claim RPC — a coworker claims a task by writing directly to disk.
///
/// Validates the task exists and is pending, then sets owner and status to in_progress
/// directly. No Lead proxy needed.
fn handle_task_claim(id: RequestId, task_id: &str, from: &str, state: &DaemonState) -> Response {
    let tasks = crate::tasks::read_tasks();
    let task = tasks.iter().find(|t| t.id == task_id);

    let Some(task) = task else {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Task !{} not found", task_id)),
        );
    };

    if task.status != crate::tasks::TaskStatus::Pending {
        return Response::error(
            id,
            RpcError::new(
                -32602,
                format!(
                    "Task !{} is not pending (status: {:?})",
                    task_id, task.status
                ),
            ),
        );
    }

    let repo_name = state.repo_name.clone();

    // Write owner and status directly to disk (with retry on transient failures).
    // Disk write happens BEFORE in-memory recording so that a failure leaves
    // no stale in-memory state. Without reconcile_stale_claims, consistency
    // depends on this ordering.
    let mut last_err = None;
    for attempt in 0..3 {
        match crate::tasks::update_task_fields_for_repo(
            task_id,
            &repo_name,
            Some(from),
            Some("in_progress"),
            None,
            None,
            None,
        ) {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => {
                warn!(
                    "Task claim disk write attempt {} failed for task !{}: {}",
                    attempt + 1,
                    task_id,
                    e
                );
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    if let Some(e) = last_err {
        return Response::error(
            id,
            RpcError::new(-32603, format!("Failed to claim task after retries: {}", e)),
        );
    }

    // Record in-memory assignment for busy tracking (only after disk write succeeds)
    state.record_task_assignment(from, task_id);

    info!("Task claim: {} claimed task !{} directly", from, task_id);

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!("Claimed task !{}", task_id),
        }),
    )
}

/// Remove shell escaping artifacts from channel messages.
///
/// When Claude Code posts messages via its Bash tool, the LLM often escapes `!`
/// as `\!` (to avoid bash history expansion). Since the Bash tool runs in
/// non-interactive mode where history expansion is disabled, the backslash passes
/// through literally. This function cleans up such artifacts.
fn unescape_shell_artifacts(s: &str) -> String {
    s.replace("\\!", "!")
}

/// Handle channel.post RPC method.
///
/// Supports IRC-style `/me` actions. If the message starts with `/me `,
/// the prefix is stripped and the message is stored as an Action type.
/// For coworkers, the action text is also reflected in their tmux tab name.
///
/// Also detects feedback requests from coworkers and nudges the Lead.
///
/// Accepts an optional `channel` parameter to post to topic channels.
/// If not provided, defaults to the main channel.
pub(super) async fn handle_channel_post(
    id: RequestId,
    from: &str,
    message: &str,
    channel: Option<&str>,
    state: &DaemonState,
) -> Response {
    // Clean up shell escaping artifacts (e.g. "\!" from bash history expansion escaping)
    let message = unescape_shell_artifacts(message);

    // Check for /me prefix (IRC-style action)
    let (content, msg_type) = if let Some(action) = message.strip_prefix("/me ") {
        (action.to_string(), MessageType::Action)
    } else {
        (message.to_string(), MessageType::Text)
    };

    // Deduplicate [Review Note] messages: suppress rapid-fire notes from the same
    // reviewer for the same PR (within 60s cooldown). Notes after the cooldown
    // (e.g., corrections or follow-ups) are allowed through.
    if let Some(pr_num) = extract_review_note_pr(&content) {
        let key = (from.to_lowercase(), pr_num);
        let now = std::time::Instant::now();
        let cooldown = std::time::Duration::from_secs(60);
        let mut tracker = state.review_note_tracker.lock().unwrap();
        if tracker
            .get(&key)
            .is_some_and(|first_seen| now.duration_since(*first_seen) < cooldown)
        {
            debug!(
                "channel.post: suppressing duplicate [Review Note] from {} for PR #{} (within {}s cooldown)",
                from,
                pr_num,
                cooldown.as_secs()
            );
            return Response::success(
                id,
                serde_json::json!({
                    "posted": false,
                    "reason": "duplicate_review_note",
                }),
            );
        }
        // Record or refresh the timestamp
        tracker.insert(key, now);
    }

    // Use provided channel or default to main channel
    let channel_name = channel.unwrap_or_else(|| state.channel_router.default_channel_name());
    let msg = Message::for_channel(channel_name, from, content.clone(), msg_type.clone());

    // Use async version to avoid blocking the runtime during file lock acquisition
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        error!("Failed to write to channel: {}", e);
        return Response::error(id, RpcError::new(-32603, e.to_string()));
    }

    info!("Channel post from {}: {}", from, message);

    // Track last activity time for coworker (used for silent coworker detection)
    if is_coworker_sender(from) {
        let mut records = state.coworker_records.write().await;
        records
            .entry(from.to_string())
            .or_insert_with(crate::rules::CoworkerRecord::new_spawn)
            .last_activity = Some(Instant::now());
        drop(records); // Release write lock before acquiring read lock
    }

    // Update tmux tab for coworkers when they post /me actions.
    // Prefer structured state from daemon memory (reported via RPC) over
    // parsing the freeform /me message text with keyword matching.
    //
    // Run tmux operations in spawn_blocking to avoid blocking the async
    // runtime. This prevents RPC timeouts when tmux commands are slow.
    if msg_type == MessageType::Action {
        let display_status = {
            let records = state.coworker_records.read().await;
            records.get(from).and_then(|record| record.display_status())
        };

        let coworkers = state.coworkers.clone();
        let from_clone = from.to_string();
        let content_clone = content.clone();

        tokio::task::spawn_blocking(move || {
            if let Some(display) = display_status {
                if let Err(e) = coworkers.update_status_formatted(&from_clone, &display) {
                    debug!("Failed to update tmux tab for {}: {}", from_clone, e);
                }
            } else {
                // Fallback: parse /me message text with keyword matching
                if let Err(e) = coworkers.update_status_display(&from_clone, Some(&content_clone)) {
                    debug!("Failed to update tmux tab for {}: {}", from_clone, e);
                }
            }
        });
    }

    // Nudge lead when user messages arrive (from web UI or TUI input)
    if state.is_user_sender(from) {
        // Check if user is @mentioning specific coworkers or @all
        let has_coworker_mentions =
            !extract_mentions(&content).is_empty() || contains_at_all(&content);
        let has_lead_mention = content.to_lowercase().contains("@lead");

        // Route @mentions in user messages directly to coworkers
        super::chat::route_mentions(state, &msg).await;

        // Only nudge lead if there are no coworker @mentions (regular
        // message for the lead) or if the user also @mentioned the lead.
        // This lets users talk directly to coworkers without the lead
        // acting as a middleman.
        if !has_coworker_mentions || has_lead_mention {
            let nudge_msg = format!("user: {}", content);
            info!("Nudging Lead about user message");
            // Run in spawn_blocking to avoid blocking the async runtime
            let coworkers = state.coworkers.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(e) = coworkers.nudge_lead(&nudge_msg) {
                    warn!("Failed to nudge Lead about user message: {}", e);
                }
            });
        } else {
            info!("Skipping Lead nudge — user message routed directly to mentioned coworker(s)");
        }
    }

    // Nudge the Lead when a coworker explicitly mentions @lead
    let content_lower = content.to_lowercase();
    if is_coworker_sender(from) && content_lower.contains("@lead") {
        // Use CooldownTracker to avoid duplicate nudges (expires after 1 hour)
        let should_nudge = {
            let cooldowns = state.cooldowns.lock().unwrap();
            cooldowns.check("lead_mention", &msg.id, Duration::from_secs(3600))
        };

        if should_nudge {
            // Record that we're nudging for this message
            {
                let mut cooldowns = state.cooldowns.lock().unwrap();
                cooldowns.record("lead_mention", &msg.id);
            }

            // Truncate message for nudge (max 100 chars)
            let summary = truncate_str(&content, 100);

            let nudge_msg = format!("{} mentioned @lead: {}", from, summary);
            info!("Nudging Lead about @lead mention from {}", from);

            // Nudge the Lead window (spawn_blocking to avoid blocking async runtime)
            let coworkers = state.coworkers.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(e) = coworkers.nudge_lead(&nudge_msg) {
                    warn!("Failed to nudge Lead about @lead mention: {}", e);
                }
            });

            // Send push notification to mobile PWA
            state.send_push_notification(&format!("@lead from {}", from), &summary, "mention");
        }
    }

    // Send bell notification and push notification for @user mentions
    // Also recognize @<display_name> if configured (e.g., @Ben)
    let has_user_mention = content_lower.contains("@user")
        || state
            .user_display_name
            .as_ref()
            .is_some_and(|dn| content_lower.contains(&format!("@{}", dn.to_lowercase())));
    if has_user_mention && !state.is_user_sender(from) {
        info!("Bell notification: @user mentioned by {}", from);
        // Run in spawn_blocking to avoid blocking the async runtime
        let coworkers = state.coworkers.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = coworkers.notify_user() {
                warn!("Failed to send bell notification for @user mention: {}", e);
            }
        });
        let display = state.user_display_name.as_deref().unwrap_or("user");
        let summary = truncate_str(&content, 100);
        state.send_push_notification(&format!("@{} from {}", display, from), &summary, "mention");
    }

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": "Message posted to channel",
        }),
    )
}

/// Handle channel.read RPC method.
fn handle_channel_read(id: RequestId, all: bool, state: &DaemonState) -> Response {
    // Read from the default (main) channel
    let default_channel = match state.channel_router.default_channel() {
        Ok(ch) => ch,
        Err(e) => {
            error!("Failed to get default channel: {}", e);
            return Response::error(id, RpcError::new(-32603, e.to_string()));
        }
    };

    let messages = if all {
        // Read all messages
        match default_channel.read_all() {
            Ok(msgs) => msgs,
            Err(e) => {
                error!("Failed to read channel: {}", e);
                return Response::error(id, RpcError::new(-32603, e.to_string()));
            }
        }
    } else {
        // Read recent messages (last 20)
        match default_channel.read_all() {
            Ok(msgs) => {
                let total = msgs.len();
                if total > 20 {
                    msgs.into_iter().skip(total - 20).collect()
                } else {
                    msgs
                }
            }
            Err(e) => {
                error!("Failed to read channel: {}", e);
                return Response::error(id, RpcError::new(-32603, e.to_string()));
            }
        }
    };

    let messages_json: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "from": m.from,
                "message": m.content,
                "timestamp": m.timestamp.to_rfc3339(),
            })
        })
        .collect();

    Response::success(
        id,
        serde_json::json!({
            "messages": messages_json,
        }),
    )
}

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
// Status & Kanban handlers
// ============================================================================

/// Handle status RPC method.
///
/// This handler runs blocking operations (gh CLI, file I/O) in spawn_blocking
/// to avoid blocking the async runtime and causing RPC timeouts.
async fn handle_status(id: RequestId, state: &DaemonState) -> Response {
    // Build a map of coworker name -> task subject from in_progress tasks
    // This is the source of truth for what each coworker is working on
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

    // Get coworkers with their details, looking up current task from task storage
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

    // Get cached PR data from the daemon's periodic polling (every 30s for open PRs,
    // every 5 minutes for merged PRs). This avoids synchronous gh CLI calls that can
    // timeout under GitHub API rate limiting.
    //
    // During daemon startup (before the first PR poll completes), return empty arrays
    // rather than stale data. The first open PR poll completes within ~5 seconds, so
    // this window is brief.
    let (pull_requests, merged_prs) = {
        let cache = state.pr_coworker_cache.read().unwrap();
        if cache.pr_poll_initialized {
            (cache.open_prs_data.clone(), cache.merged_prs_data.clone())
        } else {
            // PR poll hasn't completed yet - return empty arrays during startup
            (Vec::new(), Vec::new())
        }
    };

    // Run blocking file I/O operations in spawn_blocking.
    // Note: get_all_tasks reads from Claude Code task storage (local filesystem),
    // not GitHub API, so it's fast and doesn't cause rate limit timeouts.
    let (tasks, recent_activity) = match tokio::task::spawn_blocking(move || {
        let tasks = get_all_tasks();
        let recent_activity = get_recent_channel_activity();
        (tasks, recent_activity)
    })
    .await
    {
        Ok(result) => result,
        Err(e) => {
            error!("spawn_blocking panic in status handler: {}", e);
            return Response::error(id, RpcError::new(-32603, "Internal error".to_string()));
        }
    };

    let pending_count = tasks
        .iter()
        .filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("pending"))
        .count();

    // Get GitHub API rate limit state
    let rate_limit = {
        let ps = state.persistent_state.lock().await;
        ps.github.rate_limit.clone()
    };

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "daemon_running": true,
            "active_coworkers": state.coworkers.count(),
            "max_coworkers": state.max_coworkers,
            "max_dev_coworkers": state.max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1),
            "pending_tasks": pending_count,
            "socket_path": state.socket_path.to_string_lossy(),
            "coworkers": coworkers,
            "tasks": tasks,
            "pull_requests": pull_requests,
            "merged_prs": merged_prs,
            "recent_activity": recent_activity,
            "github_rate_limit": {
                "graphql": {
                    "remaining": rate_limit.graphql.remaining,
                    "limit": rate_limit.graphql.limit,
                    "used": rate_limit.graphql.used,
                    "reset": rate_limit.graphql.reset,
                    "remaining_pct": (rate_limit.graphql.remaining_pct() * 100.0) as u32,
                },
                "rest": {
                    "remaining": rate_limit.core.remaining,
                    "limit": rate_limit.core.limit,
                    "used": rate_limit.core.used,
                    "reset": rate_limit.core.reset,
                    "remaining_pct": (rate_limit.core.remaining_pct() * 100.0) as u32,
                },
                "summary": rate_limit.summary(),
            },
        }),
    )
}

// REMOVED: get_open_prs() and format_pr_status()
// These functions made synchronous gh CLI calls on every RPC, causing timeouts under
// GitHub API rate limiting. Now handle_status uses cached PR data from the daemon's
// periodic polling (see pr_coworker_cache in daemon/mod.rs and poll_prs_for_issues in
// daemon/pr.rs). The formatting logic moved to format_pr_status_for_rpc() in pr.rs.

/// Get all tasks from Claude Code task storage with their status.
fn get_all_tasks() -> Vec<serde_json::Value> {
    crate::tasks::read_tasks()
        .into_iter()
        .map(|task| {
            let status = match task.status {
                crate::tasks::TaskStatus::Pending => "pending",
                crate::tasks::TaskStatus::InProgress => "in_progress",
                crate::tasks::TaskStatus::Completed => "completed",
            };
            serde_json::json!({
                "id": task.id,
                "subject": task.subject,
                "status": status,
                "assignee": task.owner,
            })
        })
        .collect()
}

/// Handle kanban.data RPC method - returns PR data for the kanban board.
///
/// Returns open PRs with author, reviewer, CI status, and timestamps,
/// plus recently merged PRs for the Done column.
///
/// Runs blocking GraphQL operations in spawn_blocking to avoid blocking
/// the async runtime and causing RPC timeouts.
///
/// Uses a 30s TTL cache (via `DaemonState::kanban_cache`) to avoid expensive
/// GraphQL queries on every call and reduce GitHub API usage.
async fn handle_kanban_data(id: RequestId, state: &DaemonState) -> Response {
    // Clone data needed for cache key computation
    let all_repo_paths = state.all_repo_paths.clone();

    // Compute a hash of all repo paths for cache keying
    let mut hasher = DefaultHasher::new();
    for path in &all_repo_paths {
        path.hash(&mut hasher);
    }
    let repo_hash = hasher.finish();

    // Check cache first
    if let Some(cached) = state.kanban_cache.get(repo_hash) {
        debug!(
            "Returning cached kanban data (TTL: {}s)",
            KANBAN_CACHE_TTL.as_secs()
        );
        return Response::success(id, cached);
    }

    // Cache miss - fetch fresh data
    debug!("Cache miss, fetching fresh kanban data");

    // Get reviewer assignments from GitHubState (best-effort via try_lock)
    let reviewer_assignments: HashMap<u64, crate::github_state::PrReviewerAssignment> = state
        .persistent_state
        .try_lock()
        .map(|ps| ps.github.active_assignments())
        .unwrap_or_default();

    let is_multi_repo = all_repo_paths.len() > 1;

    // Pre-resolve repo full names (this uses caching and is fast)
    let repo_data: Vec<(std::path::PathBuf, String, String)> = all_repo_paths
        .iter()
        .map(|repo_path| {
            let full_name = state.get_repo_full_name(repo_path);
            let label = repo_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            (repo_path.clone(), full_name, label)
        })
        .collect();

    // Run blocking GraphQL operations in spawn_blocking
    let (prs, merged_prs, repos) = match tokio::task::spawn_blocking(move || {
        let mut prs = Vec::new();
        let mut merged_prs = Vec::new();
        let mut repos = Vec::new();

        for (repo_path, full_name, label) in repo_data {
            let repo_label = if is_multi_repo {
                repo_path.file_name().and_then(|s| s.to_str())
            } else {
                None
            };

            repos.push(serde_json::json!({
                "label": label,
                "full_name": full_name,
            }));

            let (open, merged) =
                fetch_kanban_all_prs(&reviewer_assignments, &full_name, &repo_path, repo_label);
            prs.extend(open);
            merged_prs.extend(merged);
        }

        (prs, merged_prs, repos)
    })
    .await
    {
        Ok(result) => result,
        Err(e) => {
            error!("spawn_blocking panic in kanban_data handler: {}", e);
            return Response::error(id, RpcError::new(-32603, "Internal error".to_string()));
        }
    };

    // Collect coworker data from daemon state
    let coworkers_data = {
        let active_coworkers = state.coworkers.list();
        let coworker_records = state.coworker_records.read().await;
        let headless_health = state.headless_health.read().unwrap();
        let prs_by_task_id = build_pr_task_map(&prs);

        active_coworkers
            .iter()
            .filter_map(|cw| {
                // Get coworker's workflow state from records
                let record = coworker_records.get(&cw.name);
                let workflow_phase = record.and_then(|r| r.workflow_phase);
                let task_id = record.and_then(|r| r.task_id);

                // Skip idle coworkers (phase = Idle or Completed)
                if matches!(
                    workflow_phase,
                    Some(crate::coworker_state::WorkflowPhase::Idle)
                        | Some(crate::coworker_state::WorkflowPhase::Completed)
                ) {
                    return None;
                }

                // Get health status
                let health = headless_health.get(&cw.name);
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

                // Find PR number for this task
                let pr_number = task_id.and_then(|tid| prs_by_task_id.get(&tid).copied());

                Some(serde_json::json!({
                    "name": cw.name,
                    "task_id": task_id,
                    "phase": workflow_phase.map(|p| p.abbreviation()),
                    "pr_number": pr_number,
                    "health": health_color,
                }))
            })
            .collect::<Vec<_>>()
    };

    // Build response and cache it
    let response_data = serde_json::json!({
        "prs": prs,
        "merged_prs": merged_prs,
        "repos": repos,
        "coworkers": coworkers_data,
    });

    state.kanban_cache.set(response_data.clone(), repo_hash);

    Response::success(id, response_data)
}

// ============================================================================
// Kanban / PR data helpers
// ============================================================================

/// TTL for kanban data cache (30 seconds, matching web server's CACHE_TTL).
const KANBAN_CACHE_TTL: Duration = Duration::from_secs(30);

/// Build a map of task_id -> pr_number from PR data.
///
/// Extracts task IDs from PR titles (e.g., "[Midtown !1234]") and maps them
/// to their PR numbers for coworker status display.
fn build_pr_task_map(prs: &[serde_json::Value]) -> HashMap<u32, u64> {
    prs.iter()
        .filter_map(|pr| {
            let title = pr.get("title")?.as_str()?;
            let pr_number = pr.get("number")?.as_u64()?;
            let task_id = crate::tasks::extract_task_id_from_pr_title(title)?;
            // extract_task_id_from_pr_title returns u64, but task_id in CoworkerRecord is u32
            let task_id_u32 = u32::try_from(task_id).ok()?;
            Some((task_id_u32, pr_number))
        })
        .collect()
}

/// Thread-safe TTL cache for kanban GraphQL data.
///
/// Stores the full kanban response (PRs, merged PRs, repos) keyed by a hash
/// of the repo paths. The cache expires after KANBAN_CACHE_TTL and avoids
/// expensive GraphQL queries on every RPC call.
///
/// Lives in `DaemonState` so the daemon can inspect and clean it up alongside
/// other caches (see `DaemonState::cleanup_rpc_response_cache`).
pub(crate) struct KanbanCache {
    inner: std::sync::Mutex<Option<(Instant, serde_json::Value, u64)>>,
}

impl KanbanCache {
    pub(crate) fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(None),
        }
    }

    /// Return cached value if it exists, is younger than TTL, and matches the repo_hash.
    fn get(&self, repo_hash: u64) -> Option<serde_json::Value> {
        let guard = self.inner.lock().ok()?;
        guard
            .as_ref()
            .filter(|(ts, _, hash)| ts.elapsed() < KANBAN_CACHE_TTL && *hash == repo_hash)
            .map(|(_, v, _)| v.clone())
    }

    /// Store a new value with the current timestamp and repo_hash.
    fn set(&self, value: serde_json::Value, repo_hash: u64) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = Some((Instant::now(), value, repo_hash));
        }
    }

    /// Remove expired entries. Called by `DaemonState::cleanup_rpc_response_cache`.
    pub(crate) fn cleanup(&self) {
        if let Ok(mut guard) = self.inner.lock()
            && guard
                .as_ref()
                .is_some_and(|(ts, _, _)| ts.elapsed() >= KANBAN_CACHE_TTL)
        {
            *guard = None;
        }
    }
}

/// GraphQL query that fetches both open and recently merged PRs in a single call.
///
/// This replaces two separate `gh pr list` CLI calls with one GraphQL request,
/// cutting API usage in half for the kanban board.
///
/// Query cost optimizations:
/// - contexts(first: 20) instead of 100 — CI status is enough with top 20 checks
/// - comments(first: 10) instead of 100 — kanban board only needs recent activity
///
/// These changes reduce query cost ~25x while preserving UI functionality.
const KANBAN_GRAPHQL_QUERY: &str = r#"
query($owner: String!, $repo: String!) {
  repository(owner: $owner, name: $repo) {
    openPrs: pullRequests(states: OPEN, first: 100, orderBy: {field: CREATED_AT, direction: DESC}) {
      nodes {
        number
        title
        author { login }
        createdAt
        body
        commits(last: 1) {
          nodes {
            commit {
              statusCheckRollup {
                contexts(first: 20) {
                  nodes {
                    __typename
                    ... on CheckRun {
                      status
                      conclusion
                    }
                    ... on StatusContext {
                      state
                    }
                  }
                }
              }
            }
          }
        }
        comments(first: 10) {
          nodes {
            body
            createdAt
          }
        }
      }
    }
    mergedPrs: pullRequests(states: MERGED, first: 10, orderBy: {field: UPDATED_AT, direction: DESC}) {
      nodes {
        number
        title
        mergedAt
      }
    }
  }
}
"#;

/// Fetch both open and merged PRs for a repo using a single GraphQL call.
///
/// `name_with_owner` should be `"owner/repo"` (e.g. `"anthropics/midtown"`).
/// Returns `(open_prs, merged_prs)` formatted for the kanban board.
/// Falls back to empty vectors on failure.
fn fetch_kanban_all_prs(
    reviewer_assignments: &HashMap<u64, crate::github_state::PrReviewerAssignment>,
    name_with_owner: &str,
    repo_path: &std::path::Path,
    repo_label: Option<&str>,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let parts: Vec<&str> = name_with_owner.splitn(2, '/').collect();
    if parts.len() != 2 {
        debug!("Unexpected nameWithOwner format: {}", name_with_owner);
        return (Vec::new(), Vec::new());
    }
    let (owner, repo_name) = (parts[0], parts[1]);

    // Execute the batched GraphQL query
    let graphql_output = std::process::Command::new("gh")
        .current_dir(repo_path)
        .args([
            "api",
            "graphql",
            "-F",
            &format!("owner={}", owner),
            "-F",
            &format!("repo={}", repo_name),
            "-f",
            &format!("query={}", KANBAN_GRAPHQL_QUERY),
        ])
        .output();

    let data = match graphql_output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            match serde_json::from_str::<serde_json::Value>(&stdout) {
                Ok(v) => v,
                Err(e) => {
                    warn!("Failed to parse kanban GraphQL response: {}", e);
                    return (Vec::new(), Vec::new());
                }
            }
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warn!(
                "GitHub API query failed for {}: {}",
                name_with_owner,
                stderr.trim()
            );
            return (Vec::new(), Vec::new());
        }
        Err(e) => {
            warn!("Failed to execute gh command: {}", e);
            return (Vec::new(), Vec::new());
        }
    };

    let repository = match data.pointer("/data/repository") {
        Some(r) => r,
        None => {
            debug!("No repository data in kanban GraphQL response");
            return (Vec::new(), Vec::new());
        }
    };

    // Process open PRs
    let open_prs = repository
        .pointer("/openPrs/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|pr| {
                    let number = pr.get("number").and_then(|v| v.as_u64())?;

                    let title = pr
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let github_author = pr
                        .pointer("/author/login")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();

                    let body = pr.get("body").and_then(|v| v.as_str()).unwrap_or("");
                    let author = extract_coworker_from_pr_body(body).unwrap_or(github_author);

                    let created_at = pr.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");

                    // Extract CI status from the last commit's statusCheckRollup
                    let check_contexts: Vec<serde_json::Value> = pr
                        .pointer("/commits/nodes")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.last())
                        .and_then(|node| node.pointer("/commit/statusCheckRollup/contexts/nodes"))
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let ci_status = kanban_ci_status(&check_contexts);

                    // Extract reviewer from comments
                    let comments: Vec<serde_json::Value> = pr
                        .pointer("/comments/nodes")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let (comment_reviewer, reviewed_at) =
                        extract_reviewer_from_pr_comments(&comments);

                    // Use comment reviewer, or fall back to assigned reviewer.
                    // Track whether the review was actually posted (vs just assigned).
                    let (reviewer, reviewer_assigned_at, review_posted) =
                        if let Some(reviewer) = comment_reviewer {
                            (Some(reviewer), reviewed_at, true)
                        } else if let Some(assignment) = reviewer_assignments.get(&number) {
                            (
                                Some(assignment.reviewer.clone()),
                                Some(assignment.assigned_at.to_rfc3339()),
                                false,
                            )
                        } else {
                            (None, None, false)
                        };

                    Some(serde_json::json!({
                        "number": number,
                        "title": title,
                        "author": author,
                        "created_at": created_at,
                        "ci_status": ci_status,
                        "reviewer": reviewer,
                        "reviewed_at": reviewer_assigned_at,
                        "review_posted": review_posted,
                        "repo": repo_label,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    // Process merged PRs
    let merged_prs = repository
        .pointer("/mergedPrs/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|pr| {
                    let number = pr.get("number").and_then(|v| v.as_u64())?;
                    let title = pr
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let merged_at = pr
                        .get("mergedAt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(serde_json::json!({
                        "number": number,
                        "title": title,
                        "merged_at": merged_at,
                        "repo": repo_label,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    (open_prs, merged_prs)
}

/// Extract coworker name from PR body frontmatter (<!-- midtown: name -->).
fn extract_coworker_from_pr_body(body: &str) -> Option<String> {
    let marker = "midtown:";
    let marker_pos = body.find(marker)?;
    let before = &body[..marker_pos];
    if !before.contains("<!--") {
        return None;
    }
    let after_marker = &body[marker_pos + marker.len()..];
    let end = after_marker.find("-->")?;
    let name = after_marker[..end].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Extract reviewer name and timestamp from PR comments.
fn extract_reviewer_from_pr_comments(
    comments: &[serde_json::Value],
) -> (Option<String>, Option<String>) {
    for comment in comments {
        let body = comment.get("body").and_then(|v| v.as_str()).unwrap_or("");
        if !body.contains("Code Review") && !body.contains("Code review") {
            continue;
        }

        // Try frontmatter first
        let reviewer = extract_coworker_from_pr_body(body).or_else(|| {
            // Fall back to "Code Review by {name}" header
            for line in body.lines() {
                let trimmed = line.trim().trim_start_matches('#').trim();
                if let Some(rest) = trimmed
                    .strip_prefix("Code Review by ")
                    .or_else(|| trimmed.strip_prefix("Code review by "))
                {
                    let name = rest.trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
            None
        });

        if let Some(name) = reviewer {
            let created_at = comment
                .get("createdAt")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            return (Some(name), created_at);
        }
    }
    (None, None)
}

/// Compute CI status string from statusCheckRollup array.
fn kanban_ci_status(checks: &[serde_json::Value]) -> &'static str {
    if checks.is_empty() {
        return "unknown";
    }

    let mut has_running = false;
    let mut has_failed = false;
    let mut has_passed = false;

    for check in checks {
        let status = check.get("status").and_then(|v| v.as_str());
        let conclusion = check.get("conclusion").and_then(|v| v.as_str());
        let state = check.get("state").and_then(|v| v.as_str());

        if let Some(status) = status {
            match status {
                "IN_PROGRESS" | "QUEUED" | "WAITING" | "PENDING" => has_running = true,
                "COMPLETED" => match conclusion {
                    Some("SUCCESS") => has_passed = true,
                    Some("FAILURE") | Some("CANCELLED") | Some("TIMED_OUT") => has_failed = true,
                    _ => {}
                },
                _ => {}
            }
        }

        if let Some(state) = state {
            match state {
                "PENDING" => has_running = true,
                "SUCCESS" => has_passed = true,
                "FAILURE" | "ERROR" => has_failed = true,
                _ => {}
            }
        }
    }

    if has_failed {
        "failed"
    } else if has_running {
        "running"
    } else if has_passed {
        "passed"
    } else {
        "unknown"
    }
}

// REMOVED: get_merged_prs()
// This function made synchronous gh CLI calls on every RPC, causing timeouts under
// GitHub API rate limiting. Now handle_status uses cached merged PR data from the
// daemon's periodic polling (see pr_coworker_cache and get_coworkers_with_merged_prs
// in daemon/pr.rs, which polls every 5 minutes).

/// Get recent channel activity.
fn get_recent_channel_activity() -> Vec<serde_json::Value> {
    // Try to read from the default channel location
    let channel_file = crate::paths::channel_file_for_repo("default");

    if !channel_file.exists() {
        return Vec::new();
    }

    // Read the last few messages from the channel
    match std::fs::read_to_string(&channel_file) {
        Ok(content) => {
            let messages: Vec<serde_json::Value> = content
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();

            // Get the last 5 messages, most recent last
            messages
                .into_iter()
                .rev()
                .take(5)
                .map(|msg| {
                    serde_json::json!({
                        "timestamp": msg.get("timestamp")
                            .and_then(|t| t.as_str())
                            .map(|t| {
                                // Format timestamp for display (just time portion)
                                if t.len() > 11 {
                                    t[11..16].to_string()
                                } else {
                                    t.to_string()
                                }
                            })
                            .unwrap_or_default(),
                        "from": msg.get("from").and_then(|f| f.as_str()).unwrap_or("unknown"),
                        "summary": truncate_message(
                            msg.get("content").and_then(|c| c.as_str()).unwrap_or(""),
                            60
                        ),
                    })
                })
                .collect()
        }
        Err(_) => Vec::new(),
    }
}

// ============================================================================
// Session attach/detach handlers
// ============================================================================

/// Parsed attach target from a "type:value" string.
#[derive(Debug, PartialEq)]
enum AttachTarget {
    Name(String),
    Task(u32),
    Pr(u64),
}

/// Parse an attach target string into a typed enum.
///
/// Pure function — no state access. Validates format and types.
fn parse_attach_target(target: &str) -> Result<AttachTarget, String> {
    if let Some(name) = target.strip_prefix("name:") {
        if name.is_empty() {
            return Err("Coworker name cannot be empty".to_string());
        }
        return Ok(AttachTarget::Name(name.to_lowercase()));
    }

    if let Some(id_str) = target.strip_prefix("task:") {
        let id: u32 = id_str
            .parse()
            .map_err(|_| format!("Invalid task ID: {}", id_str))?;
        return Ok(AttachTarget::Task(id));
    }

    if let Some(pr_str) = target.strip_prefix("pr:") {
        let pr_num: u64 = pr_str
            .parse()
            .map_err(|_| format!("Invalid PR number: {}", pr_str))?;
        return Ok(AttachTarget::Pr(pr_num));
    }

    Err(format!(
        "Invalid target format: '{}'. Use name:<name>, task:<id>, or pr:<number>",
        target
    ))
}

/// Resolve an attach target to a coworker name using daemon state.
async fn resolve_attach_target(target: &str, state: &DaemonState) -> Result<String, String> {
    let parsed = parse_attach_target(target)?;

    match parsed {
        AttachTarget::Name(name) => Ok(name),
        AttachTarget::Task(id) => {
            let id_str = id.to_string();
            let assignments = state.coworker_task_assignments.lock().unwrap();
            for (coworker, assignment) in assignments.iter() {
                if assignment.task_id == id_str {
                    return Ok(coworker.clone());
                }
            }
            Err(format!("No coworker is assigned to task !{}", id))
        }
        AttachTarget::Pr(pr_num) => {
            // Check reviewer assignments
            let persistent = state.persistent_state.lock().await;
            if let Some(reviewer) = persistent.github.get_reviewer(pr_num) {
                return Ok(reviewer.to_lowercase());
            }
            drop(persistent);
            // Fall back to branch-name-based mapping via coworker list
            let coworkers = state.coworkers.list();
            for cw in &coworkers {
                if cw
                    .current_task
                    .as_ref()
                    .is_some_and(|t| t.contains(&format!("PR #{}", pr_num)))
                {
                    return Ok(cw.name.to_lowercase());
                }
            }
            Err(format!("No coworker is working on PR #{}", pr_num))
        }
    }
}

/// Handle session.attach RPC method.
///
/// Pauses the headless coworker process and returns session info so the CLI
/// can create a tmux window with `claude --resume <session-id>`.
async fn handle_session_attach(id: RequestId, target: &str, state: &DaemonState) -> Response {
    let name = match resolve_attach_target(target, state).await {
        Ok(n) => n,
        Err(e) => return Response::error(id, RpcError::new(-32602, e)),
    };

    // Verify the coworker is running
    if state.coworkers.get(&name).is_none() {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Coworker '{}' is not running", name)),
        );
    }

    // Guard against double-attach
    {
        let attached = state.attached_coworkers.lock().unwrap();
        if attached.contains(&name.to_lowercase()) {
            return Response::error(
                id,
                RpcError::new(-32602, format!("Coworker '{}' is already attached", name)),
            );
        }
    }

    // Get the session ID from persistent state
    let session_id = {
        let persistent = state.persistent_state.lock().await;
        persistent
            .headless_sessions
            .get(&name)
            .map(|info| info.session_id.clone())
    };

    let session_id = match session_id {
        Some(sid) => sid,
        None => {
            return Response::error(
                id,
                RpcError::new(
                    -32603,
                    format!(
                        "No session ID found for coworker '{}'. \
                         They may not be running in headless mode.",
                        name
                    ),
                ),
            );
        }
    };

    // Get working directory before shutting down
    let cwd = state
        .coworkers
        .get(&name)
        .map(|cw| cw.working_dir.clone())
        .unwrap_or_default();

    // Shut down the headless coworker (kills the process but session persists on disk)
    state.broadcast_coworker_update(&name, "attaching", None);
    if let Err(e) = state.coworkers.shutdown(&name) {
        return Response::error(
            id,
            RpcError::new(
                -32603,
                format!("Failed to pause coworker '{}': {}", name, e),
            ),
        );
    }
    // Record stop time to prevent false orphan recovery during the grace period
    // (see #874). The attached_coworkers set provides the long-term exemption.
    state.record_coworker_stop_time(&name);

    // Mark as attached so stuck detection and orphan recovery skip this coworker
    {
        let mut attached = state.attached_coworkers.lock().unwrap();
        attached.insert(name.to_lowercase());
    }

    info!(
        "Paused headless coworker '{}' for attach (session={})",
        name, session_id
    );

    // Post to channel
    let _ = state
        .send_and_broadcast_async(&Message::system(format!(
            "Attached to {} — headless paused, interactive tmux session active",
            name
        )))
        .await;

    Response::success(
        id,
        serde_json::json!({
            "session_id": session_id,
            "cwd": cwd,
            "name": name,
        }),
    )
}

/// Handle session.detach RPC method.
///
/// Resumes headless execution for a coworker that was previously attached.
/// Idempotent: if the coworker is already running (e.g., a previous detach
/// succeeded), returns success without spawning a duplicate.
async fn handle_session_detach(id: RequestId, name: &str, state: &DaemonState) -> Response {
    let name = name.to_lowercase();

    // Clear attached state first (idempotent — safe to call even if not attached)
    {
        let mut attached = state.attached_coworkers.lock().unwrap();
        attached.remove(&name);
    }

    // Idempotency guard: if the coworker is already running, skip re-spawn.
    // This prevents the race between manual detach and background auto-detach
    // from spawning duplicate processes.
    if state.coworkers.get(&name).is_some() {
        info!("Coworker '{}' already running — detach is a no-op", name);
        return Response::success(
            id,
            serde_json::json!({
                "success": true,
                "message": format!("Coworker {} is already running", name),
            }),
        );
    }

    // Get session ID from persistent state
    let session_id = {
        let persistent = state.persistent_state.lock().await;
        persistent
            .headless_sessions
            .get(&name)
            .map(|info| info.session_id.clone())
    };

    let session_id = match session_id {
        Some(sid) => sid,
        None => {
            return Response::error(
                id,
                RpcError::new(
                    -32603,
                    format!("No session ID found for coworker '{}' to resume", name),
                ),
            );
        }
    };

    // Re-spawn the coworker with the resumed session
    let config = crate::launch::LaunchConfig::coworker(
        &name,
        &state.repo_name,
        crate::launch::SessionMode::ResumeSession(session_id.clone()),
        Some("You were previously running headless. The Lead attached to your session interactively and has now detached. Continue where you left off — read the channel for any updates.".to_string()),
    );

    match state.spawn_coworker(&config).await {
        Ok(()) => {
            info!(
                "Resumed headless coworker '{}' after detach (session={})",
                name, session_id
            );

            let _ = state
                .send_and_broadcast_async(&Message::system(format!(
                    "Detached from {} — headless session resumed",
                    name
                )))
                .await;

            state.broadcast_coworker_update(&name, "running", None);

            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Resumed headless session for {}", name),
                }),
            )
        }
        Err(e) => {
            warn!("Failed to resume coworker '{}' after detach: {}", name, e);
            Response::error(
                id,
                RpcError::new(
                    -32603,
                    format!("Failed to resume headless session for '{}': {}", name, e),
                ),
            )
        }
    }
}

/// Handle session.list RPC method.
///
/// Returns a list of headless sessions with their status.
async fn handle_session_list(id: RequestId, state: &DaemonState) -> Response {
    let persistent = state.persistent_state.lock().await;
    let running_coworkers: std::collections::HashSet<String> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| cw.name.to_lowercase())
        .collect();
    let attached = state.attached_coworkers.lock().unwrap().clone();

    let sessions: Vec<serde_json::Value> = persistent
        .headless_sessions
        .iter()
        .map(|(name, info)| {
            let status = if attached.contains(&name.to_lowercase()) {
                "attached"
            } else if running_coworkers.contains(&name.to_lowercase()) {
                "running"
            } else {
                "paused"
            };

            // Look up task assignment
            let task = {
                let assignments = state.coworker_task_assignments.lock().unwrap();
                assignments.get(name).map(|a| a.task_id.clone())
            };

            serde_json::json!({
                "name": name,
                "session_id": info.session_id,
                "status": status,
                "purpose": info.purpose,
                "last_active": info.last_active.to_rfc3339(),
                "task": task,
            })
        })
        .collect();

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "sessions": sessions,
        }),
    )
}

/// Invoke the clusterer to assign a channel for a new task.
///
/// Builds a ClustererRequest with minimal information (for MVP) and invokes
/// the clusterer headless session. The clusterer accumulates context across
/// invocations via session resume.
///
/// Returns the assigned channel name or an error.
async fn invoke_clusterer_for_task(
    subject: &str,
    description: &str,
    state: &DaemonState,
) -> Result<String, String> {
    use crate::daemon::clusterer::{
        ChannelInfo, ClustererRequest, CompletedTaskInfo, assign_channel,
    };
    use crate::tasks::TaskStatus;

    // Collect channel information: list all channels and their active task counts
    let base_dir = crate::paths::projects_dir_for_repo(&state.repo_name);
    let channel_names = crate::channel::Channel::list(&base_dir).unwrap_or_else(|e| {
        warn!("Failed to list channels for clusterer: {}", e);
        vec!["midtown".to_string()]
    });

    // Read all tasks to compute per-channel stats and recent completions
    let all_tasks = crate::tasks::read_tasks_for_repo(Some(&state.repo_name));

    // Build map of task_id -> channel from persistent state
    let task_channel_map = {
        let ps = state.persistent_state.lock().await;
        ps.task_channel.clone()
    };

    // Group tasks by channel and collect stats
    let mut channel_info_map: std::collections::HashMap<String, ChannelInfo> = channel_names
        .iter()
        .map(|name| {
            (
                name.clone(),
                ChannelInfo {
                    name: name.clone(),
                    active_task_count: 0,
                    recent_tasks: vec![],
                },
            )
        })
        .collect();

    // Track recently completed tasks (last 10)
    let mut recent_completions = vec![];

    for task in &all_tasks {
        let task_channel = task
            .channel
            .as_ref()
            .or_else(|| task_channel_map.get(&task.id))
            .map(|s| s.as_str())
            .unwrap_or("midtown");

        match task.status {
            TaskStatus::Completed => {
                // Collect completed tasks for context
                if recent_completions.len() < 10 {
                    recent_completions.push(CompletedTaskInfo {
                        subject: task.subject.clone(),
                        channel: Some(task_channel.to_string()),
                    });
                }
            }
            TaskStatus::InProgress | TaskStatus::Pending => {
                // Count active tasks per channel and track recent subjects
                if let Some(info) = channel_info_map.get_mut(task_channel) {
                    info.active_task_count += 1;
                    if info.recent_tasks.len() < 3 {
                        info.recent_tasks.push(task.subject.clone());
                    }
                }
            }
        }
    }

    let channels: Vec<ChannelInfo> = channel_info_map.into_values().collect();

    let request = ClustererRequest {
        task_subject: subject.to_string(),
        task_description: description.to_string(),
        channels,
        recent_completions,
    };

    // Get working directory (use primary repo path)
    let cwd = state
        .all_repo_paths
        .first()
        .ok_or("No repo paths configured")?
        .clone();

    // Lock persistent state to pass to clusterer
    let mut ps = state.persistent_state.lock().await;

    // Invoke clusterer
    let response = assign_channel(request, cwd, &mut ps).await?;

    // Save persistent state with updated session ID
    if let Err(e) = ps.save_for_repo(&state.repo_name) {
        warn!("Failed to save clusterer session ID: {}", e);
    }

    Ok(response.channel)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unescape_shell_artifacts_exclamation() {
        assert_eq!(
            unescape_shell_artifacts("Game time\\! Let's go"),
            "Game time! Let's go"
        );
    }

    #[test]
    fn test_unescape_shell_artifacts_multiple_exclamations() {
        assert_eq!(
            unescape_shell_artifacts("Wow\\! Amazing\\! Done\\!"),
            "Wow! Amazing! Done!"
        );
    }

    #[test]
    fn test_unescape_shell_artifacts_no_escapes() {
        assert_eq!(
            unescape_shell_artifacts("Normal message with ! marks"),
            "Normal message with ! marks"
        );
    }

    #[test]
    fn test_unescape_shell_artifacts_preserves_other_backslashes() {
        assert_eq!(
            unescape_shell_artifacts("path\\to\\file and \\!"),
            "path\\to\\file and !"
        );
    }

    #[test]
    fn test_extract_coworker_from_pr_body() {
        assert_eq!(
            extract_coworker_from_pr_body("<!-- midtown: york -->\n## Summary"),
            Some("york".to_string())
        );
        assert_eq!(
            extract_coworker_from_pr_body("<!--midtown:  park  -->\nDesc"),
            Some("park".to_string())
        );
        assert_eq!(extract_coworker_from_pr_body("no frontmatter here"), None);
        assert_eq!(extract_coworker_from_pr_body(""), None);
    }

    #[test]
    fn test_extract_reviewer_from_pr_comments() {
        let comments = vec![serde_json::json!({
            "body": "<!-- midtown: lexington -->\n\n### Code review\nNo issues.",
            "createdAt": "2026-01-29T10:00:00Z"
        })];
        let (reviewer, at) = extract_reviewer_from_pr_comments(&comments);
        assert_eq!(reviewer, Some("lexington".to_string()));
        assert_eq!(at, Some("2026-01-29T10:00:00Z".to_string()));

        let comments = vec![serde_json::json!({
            "body": "## Code Review by vernon\nLGTM",
            "createdAt": "2026-01-29T11:00:00Z"
        })];
        let (reviewer, _) = extract_reviewer_from_pr_comments(&comments);
        assert_eq!(reviewer, Some("vernon".to_string()));

        let (reviewer, _) = extract_reviewer_from_pr_comments(&[]);
        assert_eq!(reviewer, None);
    }

    #[test]
    fn test_kanban_ci_status() {
        assert_eq!(kanban_ci_status(&[]), "unknown");
        assert_eq!(
            kanban_ci_status(&[
                serde_json::json!({"status": "COMPLETED", "conclusion": "SUCCESS"})
            ]),
            "passed"
        );
        assert_eq!(
            kanban_ci_status(&[
                serde_json::json!({"status": "COMPLETED", "conclusion": "FAILURE"})
            ]),
            "failed"
        );
        assert_eq!(
            kanban_ci_status(&[serde_json::json!({"status": "IN_PROGRESS"})]),
            "running"
        );
    }

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
    fn test_extract_review_note_pr_standard_format() {
        let msg = "@lead [Review Note] PR #708: The new is_ui_chrome() pattern for ctrl+ key hints is heuristic. Please determine if this warrants a follow-up task.";
        assert_eq!(extract_review_note_pr(msg), Some(708));
    }

    #[test]
    fn test_extract_review_note_pr_no_match() {
        assert_eq!(extract_review_note_pr("@lead some regular message"), None);
        assert_eq!(extract_review_note_pr("fixed PR #42"), None);
        assert_eq!(extract_review_note_pr("[Review Note] no PR ref"), None);
    }

    #[test]
    fn test_extract_review_note_pr_various_numbers() {
        assert_eq!(
            extract_review_note_pr("@lead [Review Note] PR #1: minor issue"),
            Some(1)
        );
        assert_eq!(
            extract_review_note_pr("@lead [Review Note] PR #9999: edge case"),
            Some(9999)
        );
    }

    // ---- Session attach target parsing tests ----

    #[test]
    fn test_parse_attach_target_name() {
        assert_eq!(
            parse_attach_target("name:park").unwrap(),
            AttachTarget::Name("park".to_string())
        );
        // Names are lowercased
        assert_eq!(
            parse_attach_target("name:Park").unwrap(),
            AttachTarget::Name("park".to_string())
        );
    }

    #[test]
    fn test_parse_attach_target_name_empty() {
        assert!(parse_attach_target("name:").is_err());
    }

    #[test]
    fn test_parse_attach_target_task() {
        assert_eq!(
            parse_attach_target("task:42").unwrap(),
            AttachTarget::Task(42)
        );
    }

    #[test]
    fn test_parse_attach_target_task_invalid() {
        assert!(parse_attach_target("task:abc").is_err());
        assert!(parse_attach_target("task:-1").is_err());
    }

    #[test]
    fn test_parse_attach_target_pr() {
        assert_eq!(
            parse_attach_target("pr:123").unwrap(),
            AttachTarget::Pr(123)
        );
    }

    #[test]
    fn test_parse_attach_target_pr_invalid() {
        assert!(parse_attach_target("pr:abc").is_err());
    }

    #[test]
    fn test_parse_attach_target_invalid_format() {
        assert!(parse_attach_target("invalid").is_err());
        assert!(parse_attach_target("unknown:value").is_err());
        assert!(parse_attach_target("").is_err());
    }

    // ---- RPC idempotency cache tests ----

    /// Verify that the cache lookup logic correctly skips expired entries.
    ///
    /// The cache in `handle_request` checks `now.duration_since(timestamp) < 60s`.
    /// An entry older than 60 seconds should be treated as a cache miss, allowing
    /// the request to re-execute (important for retries after transient failures).
    #[test]
    fn test_rpc_cache_ttl_expiration() {
        use crate::rpc::{RequestId, Response};

        let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();
        let request_id = RequestId::String("test-ttl-123".to_string());
        let cached_response =
            Response::success(request_id.clone(), serde_json::json!({"task_id": 42}));

        // Insert entry with a timestamp 61 seconds in the past
        let old_timestamp = Instant::now() - Duration::from_secs(61);
        cache.insert(request_id.clone(), (cached_response, old_timestamp));

        // Simulate the cache lookup from handle_request (lines 104-116)
        let now = Instant::now();
        let cache_hit = cache
            .get(&request_id)
            .filter(|(_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

        assert!(
            cache_hit.is_none(),
            "Entry older than 60 seconds should be a cache miss"
        );
    }

    /// Verify that cache entries within TTL are returned as hits.
    #[test]
    fn test_rpc_cache_within_ttl() {
        use crate::rpc::{RequestId, Response};

        let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();
        let request_id = RequestId::String("test-fresh-456".to_string());
        let cached_response =
            Response::success(request_id.clone(), serde_json::json!({"task_id": 99}));

        // Insert entry with current timestamp (within TTL)
        cache.insert(request_id.clone(), (cached_response, Instant::now()));

        let now = Instant::now();
        let cache_hit = cache
            .get(&request_id)
            .filter(|(_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

        assert!(cache_hit.is_some(), "Recent entry should be a cache hit");
    }

    /// Verify that cleanup_rpc_response_cache retains fresh entries and
    /// removes expired ones — preventing unbounded memory growth.
    #[test]
    fn test_rpc_cache_cleanup_removes_expired_entries() {
        use crate::rpc::{RequestId, Response};

        let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();

        // Add 100 expired entries
        let old_timestamp = Instant::now() - Duration::from_secs(120);
        for i in 0..100 {
            let id = RequestId::String(format!("expired-{}", i));
            let resp = Response::success(id.clone(), serde_json::json!({"i": i}));
            cache.insert(id, (resp, old_timestamp));
        }

        // Add 3 fresh entries
        let fresh_timestamp = Instant::now();
        for i in 0..3 {
            let id = RequestId::String(format!("fresh-{}", i));
            let resp = Response::success(id.clone(), serde_json::json!({"i": i}));
            cache.insert(id, (resp, fresh_timestamp));
        }

        assert_eq!(cache.len(), 103);

        // Simulate the cleanup logic from DaemonState::cleanup_rpc_response_cache
        let now = Instant::now();
        cache.retain(|_, (_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

        assert_eq!(
            cache.len(),
            3,
            "Cleanup should remove all 100 expired entries, keeping 3 fresh ones"
        );

        // Verify only fresh entries remain
        for i in 0..3 {
            let id = RequestId::String(format!("fresh-{}", i));
            assert!(
                cache.contains_key(&id),
                "Fresh entry {} should be retained",
                i
            );
        }
    }

    /// Verify that only successful responses are cached (error responses are excluded).
    ///
    /// This is important because caching errors would prevent retry-on-failure:
    /// if a request fails due to a transient issue, retrying with the same request ID
    /// should re-attempt the operation, not return the cached error.
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

        // Reproduce the cache-insertion guard from handle_request (line 547)
        assert!(!success.is_error(), "Success response should not be error");
        assert!(error.is_error(), "Error response should be error");

        // Simulate: only cache non-error responses
        let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();
        let responses = vec![success, error];

        for resp in &responses {
            // This mirrors the guard: `if !response.is_error()`
            if !resp.is_error() {
                cache.insert(
                    RequestId::String("test".to_string()),
                    (resp.clone(), Instant::now()),
                );
            }
        }

        assert_eq!(cache.len(), 1, "Only success response should be cached");
    }

    /// Verify that sequential numeric request IDs (as generated by the CLI)
    /// would collide in the cache when coming from separate processes.
    ///
    /// This is the regression test for the bug where `midtown task create`
    /// called twice in quick succession returned the first task's response
    /// both times, because both CLI processes sent `id: 1`.
    #[test]
    fn test_rpc_cache_numeric_id_collision() {
        use crate::rpc::{RequestId, Response};

        let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();

        // First CLI invocation sends id: 1, creates task !100
        let id_from_process_a = RequestId::Number(1);
        let response_a = Response::success(
            id_from_process_a.clone(),
            serde_json::json!({"task_id": 100}),
        );
        cache.insert(id_from_process_a.clone(), (response_a, Instant::now()));

        // Second CLI invocation also sends id: 1 (different process, counter restarted)
        let id_from_process_b = RequestId::Number(1);

        // This demonstrates the bug: same numeric ID = cache hit, wrong response
        let now = Instant::now();
        let cache_hit = cache
            .get(&id_from_process_b)
            .filter(|(_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

        // With numeric IDs, this DOES hit — which is the bug.
        // The fix is to use unique string IDs (pid-counter) so this can't happen.
        assert!(
            cache_hit.is_some(),
            "Numeric ID collision: same id=1 from different processes hits cache (this is the bug)"
        );

        // After fix: string IDs with PID prefix won't collide
        let id_with_pid_a = RequestId::String("12345-1".to_string());
        let id_with_pid_b = RequestId::String("12346-1".to_string()); // different PID

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

    #[test]
    fn test_apply_task_channel_mapping_sets_channel() {
        let mut map = HashMap::new();
        let changed = apply_task_channel_mapping(&mut map, "42", Some("auth"), false);
        assert!(changed);
        assert_eq!(map.get("42"), Some(&"auth".to_string()));
    }

    #[test]
    fn test_apply_task_channel_mapping_overwrites_existing() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "old-channel".to_string());
        let changed = apply_task_channel_mapping(&mut map, "42", Some("new-channel"), false);
        assert!(changed);
        assert_eq!(map.get("42"), Some(&"new-channel".to_string()));
    }

    #[test]
    fn test_apply_task_channel_mapping_ignores_none() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "auth".to_string());
        let changed = apply_task_channel_mapping(&mut map, "42", None, false);
        assert!(!changed);
        assert_eq!(map.get("42"), Some(&"auth".to_string()));
    }

    #[test]
    fn test_apply_task_channel_mapping_ignores_empty_without_clear() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "auth".to_string());
        // On create (allow_clear=false), empty string is ignored
        let changed = apply_task_channel_mapping(&mut map, "42", Some(""), false);
        assert!(!changed);
        assert_eq!(map.get("42"), Some(&"auth".to_string()));
    }

    #[test]
    fn test_apply_task_channel_mapping_clears_with_empty_on_update() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "auth".to_string());
        // On update (allow_clear=true), empty string clears the mapping
        let changed = apply_task_channel_mapping(&mut map, "42", Some(""), true);
        assert!(changed);
        assert!(!map.contains_key("42"));
    }

    #[test]
    fn test_apply_task_channel_mapping_clear_nonexistent_is_noop() {
        let mut map = HashMap::new();
        // Clearing a mapping that doesn't exist returns false (no state modification)
        let changed = apply_task_channel_mapping(&mut map, "99", Some(""), true);
        assert!(!changed);
        assert!(map.is_empty());
    }

    #[test]
    fn test_apply_task_channel_mapping_none_on_empty_map() {
        let mut map: HashMap<String, String> = HashMap::new();
        let changed = apply_task_channel_mapping(&mut map, "42", None, true);
        assert!(!changed);
        assert!(map.is_empty());
    }

    #[test]
    fn test_validate_model_format_valid() {
        assert!(validate_model_format("claude/opus").is_ok());
        assert!(validate_model_format("claude/sonnet").is_ok());
        assert!(validate_model_format("claude/haiku").is_ok());
        assert!(validate_model_format("codex/o3").is_ok());
        assert!(validate_model_format("codex/o4-mini").is_ok());
    }

    #[test]
    fn test_validate_model_format_invalid() {
        // Missing slash
        assert!(validate_model_format("claude-opus").is_err());
        // Multiple slashes
        assert!(validate_model_format("claude/opus/extra").is_err());
        // Empty string
        assert!(validate_model_format("").is_err());
        // Only slash
        assert!(validate_model_format("/").is_err());
        // Empty provider
        assert!(validate_model_format("/opus").is_err());
        // Empty model
        assert!(validate_model_format("claude/").is_err());
    }

    #[test]
    fn test_apply_task_model_mapping_sets_model() {
        let mut map = HashMap::new();
        let changed = apply_task_model_mapping(&mut map, "42", Some("claude/opus"), false);
        assert!(changed.is_ok());
        assert!(changed.unwrap());
        assert_eq!(map.get("42"), Some(&"claude/opus".to_string()));
    }

    #[test]
    fn test_apply_task_model_mapping_rejects_invalid_format() {
        let mut map = HashMap::new();
        let result = apply_task_model_mapping(&mut map, "42", Some("invalid-format"), false);
        assert!(result.is_err());
        assert!(map.is_empty());
    }

    #[test]
    fn test_apply_task_model_mapping_overwrites_existing() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "claude/opus".to_string());
        let changed =
            apply_task_model_mapping(&mut map, "42", Some("claude/sonnet"), false).unwrap();
        assert!(changed);
        assert_eq!(map.get("42"), Some(&"claude/sonnet".to_string()));
    }

    #[test]
    fn test_apply_task_model_mapping_ignores_none() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "claude/opus".to_string());
        let changed = apply_task_model_mapping(&mut map, "42", None, false).unwrap();
        assert!(!changed);
        assert_eq!(map.get("42"), Some(&"claude/opus".to_string()));
    }

    #[test]
    fn test_apply_task_model_mapping_ignores_empty_without_clear() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "claude/opus".to_string());
        // On create (allow_clear=false), empty string is ignored
        let changed = apply_task_model_mapping(&mut map, "42", Some(""), false).unwrap();
        assert!(!changed);
        assert_eq!(map.get("42"), Some(&"claude/opus".to_string()));
    }

    #[test]
    fn test_apply_task_model_mapping_clears_with_empty_on_update() {
        let mut map = HashMap::new();
        map.insert("42".to_string(), "claude/opus".to_string());
        // On update (allow_clear=true), empty string clears the mapping
        let changed = apply_task_model_mapping(&mut map, "42", Some(""), true).unwrap();
        assert!(changed);
        assert!(!map.contains_key("42"));
    }

    #[test]
    fn test_apply_task_model_mapping_clear_nonexistent_is_noop() {
        let mut map = HashMap::new();
        // Clearing a mapping that doesn't exist returns false (no state modification)
        let changed = apply_task_model_mapping(&mut map, "99", Some(""), true).unwrap();
        assert!(!changed);
        assert!(map.is_empty());
    }

    #[test]
    fn test_apply_task_model_mapping_none_on_empty_map() {
        let mut map: HashMap<String, String> = HashMap::new();
        let changed = apply_task_model_mapping(&mut map, "42", None, true).unwrap();
        assert!(!changed);
        assert!(map.is_empty());
    }
}
