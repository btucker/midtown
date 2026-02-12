//! RPC request dispatch for the daemon's Unix socket protocol.
//!
//! This module handles connection management and routes JSON-RPC methods to
//! domain-specific handler modules:
//!
//! - `rpc_channel`: channel.post, channel.read
//! - `rpc_coworker`: coworker.spawn/break/list/view/report-state/nudge/asking
//! - `rpc_tasks`: task.create/update/done/claim/request/metadata
//! - `rpc_sessions`: session.attach/detach/list
//! - `rpc_handlers`: status, reminders, insight, headless
//! - `rpc_auth`: auth.switch
//! - `rpc_kanban`: kanban.data

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
    let response = dispatch_method(request, state).await;

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

/// Route an RPC request to the appropriate domain handler.
async fn dispatch_method(request: Request, state: &DaemonState) -> Response {
    match request.method.as_str() {
        // --- Simple inline handlers ---
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

        "daemon.exec-restart" => {
            info!("Exec-restart requested via RPC — daemon will re-exec after shutdown");
            state
                .restart_requested
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = state.shutdown_tx.send(());
            Response::success(request.id, serde_json::json!({"status": "restarting"}))
        }

        "daemon.check-pending" => {
            info!("Check-pending triggered via RPC");
            let snap = snapshot::collect_world_snapshot(state).await;
            let pending_effects = super::dispatch::spawn_for_pending_tasks(&snap, state);
            state.mark_in_flight_spawns_from_effects(&pending_effects);
            effects::execute_effects(pending_effects, state).await;
            Response::success(request.id, serde_json::json!({"status": "ok"}))
        }

        "snapshot" => handle_snapshot(request.id, state).await,

        // --- Coworker handlers ---
        "coworker.spawn" => {
            let params = request.params.as_ref();
            let resume = params.bool_or("resume", false);
            let prompt = params.str_param("prompt").map(|s| s.to_string());
            let provider = match parse_provider_param(params) {
                Ok(provider) => provider,
                Err(msg) => return Response::error(request.id, RpcError::new(-32602, msg)),
            };
            super::rpc_coworker::handle_coworker_spawn(request.id, state, resume, prompt, provider)
                .await
        }

        "coworker.break" => {
            let name = require_str!(request.params.as_ref(), "name", request.id);
            super::rpc_coworker::handle_coworker_break(request.id, name, state).await
        }

        "coworker.list" => super::rpc_coworker::handle_coworker_list(request.id, state),

        "coworker.view" => {
            let name = require_str!(request.params.as_ref(), "name", request.id);
            super::rpc_coworker::handle_coworker_view(request.id, name, state).await
        }

        "coworker.report-state" => {
            let params = request.params.as_ref();
            let name = require_str!(params, "name", request.id);
            let phase = require_str!(params, "phase", request.id);
            let task_id = params.u64_param("task_id").map(|v| v as u32);
            super::rpc_coworker::handle_coworker_report_state(
                request.id, name, phase, task_id, state,
            )
            .await
        }

        "coworker.nudge" => {
            let params = request.params.as_ref();
            let name = require_str!(params, "name", request.id);
            let message = require_str!(params, "message", request.id);
            let from = params.str_or("from", "lead");
            super::rpc_coworker::handle_coworker_nudge(request.id, from, name, message, state).await
        }

        "coworker.asking" => {
            let params = request.params.as_ref();
            let name = require_str!(params, "name", request.id);
            let question = require_str!(params, "question", request.id);
            super::rpc_coworker::handle_coworker_asking(request.id, name, question, state).await
        }

        // --- Channel handlers ---
        "channel.post" => {
            let params = request.params.as_ref();
            let message = require_str!(params, "message", request.id);
            let from = params.str_or("from", "lead");
            let channel = params.str_param("channel");
            super::rpc_channel::handle_channel_post(request.id, from, message, channel, state).await
        }

        "channel.read" => {
            let all = request.params.as_ref().bool_or("all", false);
            super::rpc_channel::handle_channel_read(request.id, all, state)
        }

        // --- Task handlers ---
        "task.create" => {
            let params = request.params.as_ref();
            let subject = require_str!(params, "subject", request.id);
            let description = params.str_or("description", "");
            let blocked_by = params.str_array_param("blocked_by");
            let channel = params.str_param("channel");
            let model = params.str_param("model");
            let pr = params.u64_param("pr");
            super::rpc_tasks::handle_task_create(
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
            let params = request.params.as_ref();
            let task_id = require_str!(params, "id", request.id);
            let owner = params.str_param("owner");
            let status = params.str_param("status");
            let description = params.str_param("description");
            let blocked_by = params.str_array_param("blocked_by");
            let channel = params.str_param("channel");
            let model = params.str_param("model");
            let pr = params.u64_param("pr");
            super::rpc_tasks::handle_task_update(
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
            let task_id = require_str!(request.params.as_ref(), "id", request.id);
            super::rpc_tasks::handle_task_done(request.id, task_id, state).await
        }

        "task.metadata" => {
            let task_id = require_str!(request.params.as_ref(), "id", request.id);
            super::rpc_tasks::handle_task_metadata(request.id, task_id, state).await
        }

        "task.request" => {
            let params = request.params.as_ref();
            let message = require_str!(params, "message", request.id);
            let from = params.str_or("from", "unknown");
            super::rpc_tasks::handle_task_request(request.id, from, message, state).await
        }

        "task.claim" => {
            let params = request.params.as_ref();
            let task_id = require_str!(params, "id", request.id);
            let from = params.str_or("from", "unknown");
            super::rpc_tasks::handle_task_claim(request.id, task_id, from, state)
        }

        // --- Status & Kanban ---
        "status" => super::rpc_handlers::handle_status(request.id, state).await,

        "kanban.data" => super::rpc_kanban::handle_kanban_data(request.id, state).await,

        // --- Reminders ---
        "reminder.create" => {
            let params = request.params.as_ref();
            let trigger = require_str!(params, "trigger", request.id);
            let message = require_str!(params, "message", request.id);
            if trigger != "all-work-merged" {
                Response::error(request.id, RpcError::invalid_params())
            } else {
                super::rpc_handlers::handle_reminder_create(request.id, message, state).await
            }
        }

        "reminder.list" => super::rpc_handlers::handle_reminder_list(request.id, state).await,

        "reminder.cancel" => {
            let reminder_id = require_str!(request.params.as_ref(), "id", request.id);
            super::rpc_handlers::handle_reminder_cancel(request.id, reminder_id, state).await
        }

        // --- Insight ---
        "insight.report" => {
            let params = request.params.as_ref();
            let agent = require_str!(params, "agent", request.id);
            let insight = require_str!(params, "insight", request.id);
            let channel = params.str_param("channel");
            super::rpc_handlers::handle_insight_report(request.id, agent, insight, channel, state)
                .await
        }

        // --- Headless ---
        "headless.execute" => {
            let params = request.params.as_ref();
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
            super::rpc_handlers::handle_headless_execute(request.id, prompt, &config).await
        }

        // --- Sessions ---
        "session.attach" => {
            let target = require_str!(request.params.as_ref(), "target", request.id);
            super::rpc_sessions::handle_session_attach(request.id, target, state).await
        }

        "session.detach" => {
            let name = require_str!(request.params.as_ref(), "name", request.id);
            super::rpc_sessions::handle_session_detach(request.id, name, state).await
        }

        "session.list" => super::rpc_sessions::handle_session_list(request.id, state).await,

        // --- Auth ---
        "auth.switch" => {
            let params = request.params.as_ref();
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

        _ => {
            warn!("Unknown method: {}", request.method);
            Response::error(request.id, RpcError::method_not_found())
        }
    }
}

/// Handle snapshot RPC method — kept inline because it's small and specific to the daemon.
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use crate::rpc::{RequestId, Response, RpcError};

    // ---- RPC idempotency cache tests ----

    /// Verify that the cache lookup logic correctly skips expired entries.
    ///
    /// The cache in `handle_request` checks `now.duration_since(timestamp) < 60s`.
    /// An entry older than 60 seconds should be treated as a cache miss, allowing
    /// the request to re-execute (important for retries after transient failures).
    #[test]
    fn test_rpc_cache_ttl_expiration() {
        let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();
        let request_id = RequestId::String("test-ttl-123".to_string());
        let cached_response =
            Response::success(request_id.clone(), serde_json::json!({"task_id": 42}));

        // Insert entry with a timestamp 61 seconds in the past
        let old_timestamp = Instant::now() - Duration::from_secs(61);
        cache.insert(request_id.clone(), (cached_response, old_timestamp));

        // Simulate the cache lookup from handle_request
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
        let success = Response::success(
            RequestId::String("s1".to_string()),
            serde_json::json!({"ok": true}),
        );
        let error = Response::error(
            RequestId::String("e1".to_string()),
            RpcError::invalid_params(),
        );

        // Reproduce the cache-insertion guard from handle_request
        assert!(!success.is_error(), "Success response should not be error");
        assert!(error.is_error(), "Error response should be error");

        // Simulate: only cache non-error responses
        let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();
        let responses = vec![success, error];

        for resp in &responses {
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

    /// Reproduce the bug: `blocking_lock()` on a `tokio::Mutex` inside an async
    /// context panics with "Cannot block the current thread from within a runtime."
    #[tokio::test(flavor = "current_thread")]
    async fn test_blocking_lock_deadlock_reproduction() {
        use std::sync::Arc;

        let mutex = Arc::new(tokio::sync::Mutex::new(42u32));

        // === Part 1: blocking_lock() panics in async context ===
        let m1 = Arc::clone(&mutex);
        let blocker = tokio::spawn(async move {
            let _guard = m1.blocking_lock();
        });

        let blocker_result = blocker.await;
        assert!(
            blocker_result.is_err(),
            "Expected blocking_lock() to panic inside tokio runtime, but it succeeded"
        );
        let panic_msg = format!("{:?}", blocker_result.unwrap_err());
        assert!(
            panic_msg.contains("block the current thread"),
            "Expected 'Cannot block the current thread' panic, got: {}",
            panic_msg
        );

        // === Part 2: .lock().await works under contention (the fix) ===
        let m2 = Arc::clone(&mutex);
        let holder = tokio::spawn(async move {
            let _guard = m2.lock().await;
            tokio::task::yield_now().await;
        });

        let m3 = Arc::clone(&mutex);
        let awaiter = tokio::spawn(async move {
            let _guard = m3.lock().await;
        });

        let ok_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            holder.await.expect("holder panicked");
            awaiter.await.expect("awaiter panicked");
        })
        .await;

        assert!(
            ok_result.is_ok(),
            "Expected .lock().await to resolve without deadlock"
        );
    }

    /// Ensure no daemon code uses blocking_lock() on tokio::Mutex.
    #[test]
    fn no_blocking_lock_in_daemon_code() {
        let daemon_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/daemon");
        let mut violations = Vec::new();

        for entry in std::fs::read_dir(&daemon_dir).expect("read daemon dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "rs") {
                let content = std::fs::read_to_string(&path).expect("read file");
                let mut in_test_module = false;
                for (line_num, line) in content.lines().enumerate() {
                    if line.trim_start().starts_with("//") || line.trim_start().starts_with("///") {
                        continue;
                    }
                    if line.contains("#[cfg(test)]") {
                        in_test_module = true;
                        continue;
                    }
                    if in_test_module {
                        continue;
                    }
                    let needle = format!(".{}()", "blocking_lock");
                    if line.contains(&needle) {
                        violations.push(format!(
                            "{}:{}: {}",
                            path.file_name().unwrap().to_string_lossy(),
                            line_num + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }

        assert!(
            violations.is_empty(),
            "Found blocking_lock() calls in daemon code (use .lock().await instead):\n{}",
            violations.join("\n")
        );
    }
}
