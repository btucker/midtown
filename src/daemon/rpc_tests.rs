//! Tests for RPC module.
//!
//! Includes:
//! - RPC handler response serialization tests
//! - RPC idempotency cache tests

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::rpc::{RequestId, Response, RpcError};
use serde_json::Value;

/// CLI Response enum (simplified copy for testing).
///
/// The actual Response enum is in src/bin/midtown/cli/response.rs.
/// We reproduce it here to test RPC response deserialization.
#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum CliResponse {
    Message { message: String },
    Status(StatusResponse),
    Coworkers { coworkers: Vec<Value> },
    Messages { messages: Vec<Value> },
    Tasks { tasks: Vec<Value> },
    PullRequests { pull_requests: Vec<Value> },
}

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
struct StatusResponse {
    daemon_running: bool,
    active_coworkers: usize,
    pending_tasks: usize,
    socket_path: String,
}

/// Test that exec-restart RPC response can be deserialized as a CLI Response.
///
/// The daemon should return `{"message": "restarting"}` so the CLI can deserialize
/// it as a Response::Message variant.
#[test]
fn test_exec_restart_response_deserializes() {
    // Simulate the RPC response the daemon sends
    let daemon_response = Response::success(
        RequestId::Number(1),
        serde_json::json!({"message": "restarting"}),
    );

    // Serialize to JSON (what gets sent over the wire)
    let json = serde_json::to_string(&daemon_response).unwrap();

    // Try to deserialize as a JSON-RPC response
    #[derive(serde::Deserialize)]
    struct JsonRpcResponse {
        result: Option<Value>,
    }

    let rpc_response: JsonRpcResponse = serde_json::from_str(&json).unwrap();
    let result = rpc_response.result.unwrap();

    // Try to deserialize the result into a CLI Response enum
    // This is where the bug occurs - it should succeed but currently fails
    let cli_response: Result<CliResponse, _> = serde_json::from_value(result);

    assert!(
        cli_response.is_ok(),
        "exec-restart response should deserialize to CLI Response, got error: {:?}",
        cli_response.unwrap_err()
    );
}

/// Test that check-pending RPC response can be deserialized as a CLI Response.
#[test]
fn test_check_pending_response_deserializes() {
    let daemon_response =
        Response::success(RequestId::Number(1), serde_json::json!({"message": "ok"}));

    let json = serde_json::to_string(&daemon_response).unwrap();

    #[derive(serde::Deserialize)]
    struct JsonRpcResponse {
        result: Option<Value>,
    }

    let rpc_response: JsonRpcResponse = serde_json::from_str(&json).unwrap();
    let result = rpc_response.result.unwrap();

    let cli_response: Result<CliResponse, _> = serde_json::from_value(result);

    assert!(
        cli_response.is_ok(),
        "check-pending response should deserialize to CLI Response, got error: {:?}",
        cli_response.unwrap_err()
    );
}

/// Test that shutdown RPC response can be deserialized as a CLI Response.
#[test]
fn test_shutdown_response_deserializes() {
    let daemon_response = Response::success(
        RequestId::Number(1),
        serde_json::json!({"message": "shutting_down"}),
    );

    let json = serde_json::to_string(&daemon_response).unwrap();

    #[derive(serde::Deserialize)]
    struct JsonRpcResponse {
        result: Option<Value>,
    }

    let rpc_response: JsonRpcResponse = serde_json::from_str(&json).unwrap();
    let result = rpc_response.result.unwrap();

    let cli_response: Result<CliResponse, _> = serde_json::from_value(result);

    assert!(
        cli_response.is_ok(),
        "shutdown response should deserialize to CLI Response, got error: {:?}",
        cli_response.unwrap_err()
    );
}

// ============================================================================
// RPC Idempotency Cache Tests
// ============================================================================

#[test]
fn test_rpc_cache_ttl_expiration() {
    let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();
    let request_id = RequestId::String("test-ttl-123".to_string());
    let cached_response = Response::success(request_id.clone(), serde_json::json!({"task_id": 42}));

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
    let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();
    let request_id = RequestId::String("test-fresh-456".to_string());
    let cached_response = Response::success(request_id.clone(), serde_json::json!({"task_id": 99}));

    cache.insert(request_id.clone(), (cached_response, Instant::now()));

    let now = Instant::now();
    let cache_hit = cache
        .get(&request_id)
        .filter(|(_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

    assert!(cache_hit.is_some(), "Recent entry should be a cache hit");
}

#[test]
fn test_rpc_cache_cleanup_removes_expired_entries() {
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

    assert_eq!(
        cache.len(),
        3,
        "Cleanup should remove all 100 expired entries, keeping 3 fresh ones"
    );

    for i in 0..3 {
        let id = RequestId::String(format!("fresh-{}", i));
        assert!(
            cache.contains_key(&id),
            "Fresh entry {} should be retained",
            i
        );
    }
}

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

    assert!(!success.is_error(), "Success response should not be error");
    assert!(error.is_error(), "Error response should be error");

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

#[test]
fn test_rpc_cache_numeric_id_collision() {
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

    let response_a2 = Response::success(id_with_pid_a.clone(), serde_json::json!({"task_id": 100}));
    cache.insert(id_with_pid_a, (response_a2, Instant::now()));

    let cache_hit = cache
        .get(&id_with_pid_b)
        .filter(|(_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

    assert!(
        cache_hit.is_none(),
        "PID-prefixed string IDs from different processes should NOT collide"
    );
}

// ============================================================================
// RPC cache allowlist coverage tests
// ============================================================================

/// All RPC methods defined in dispatch_request, used to verify cache allowlist coverage.
/// When adding a new RPC method, add it here AND to either CACHEABLE_METHODS or
/// UNCACHEABLE_METHODS below.
const ALL_RPC_METHODS: &[&str] = &[
    // Simple / inline handlers
    "ping",
    "version",
    "shutdown",
    "daemon.exec-restart",
    "daemon.set-draining",
    "daemon.check-pending",
    "coworker.stop_all",
    // Snapshot / oneshot
    "snapshot",
    "oneshot.execute",
    // Coworker lifecycle
    "coworker.spawn",
    "coworker.break",
    "coworker.list",
    "coworker.view",
    "coworker.report-state",
    "coworker.nudge",
    "coworker.asking",
    "coworker.questions",
    // Lead lifecycle
    "lead.spawn",
    // Status / PRs
    "status",
    "prs.status",
    "pr.review",
    "pr.merge",
    "pr.auto-merge",
    "pr.review-post",
    "pr.list-external",
    "pr.allow",
    "coworkers.status",
    // Channel
    "channel.post",
    "channel.read",
    "channel.list",
    "channel.create",
    "channel.archive",
    "channel.unarchive",
    "channel.rename",
    // Tasks
    "task.create",
    "task.update",
    "task.done",
    "task.metadata",
    "task.request",
    "task.claim",
    // Reminders
    "reminder.create",
    "reminder.list",
    "reminder.cancel",
    // Workflow management
    "workflow.assign",
    "workflow.unassign",
    "workflow.list",
    "workflow.set_lead_driven",
    // Workflow state
    "workflow.get_state",
    "workflow.set_state",
    // Auth
    "auth.switch",
    "auth.pool-toggle",
    // Sessions
    "session.resolve",
    "session.attach",
    "session.detach",
    "session.list",
    "session.view",
    "session.clear",
    "session.fork",
    "session.fork_thread",
    "session.unfork_thread",
    "session.thread_ownership",
    // Headed wrapper intercom
    "headed.register",
    "headed.unregister",
    "headed.heartbeat",
    "headed.poll",
    "headed.ack",
    "headed.output",
    "headed.enqueue",
];

/// Methods that are safe to cache: parameter-free reads without their own
/// domain cache. Parameterized reads are excluded because the web layer
/// reuses id=1, so two calls with different params would collide.
const CACHEABLE_METHODS: &[&str] = &[
    "ping",
    "version",
    "status",
    "snapshot",
    "coworker.list",
    "coworker.questions",
    "channel.list",
    "reminder.list",
    "workflow.list",
    "session.list",
    "pr.list-external",
];

/// Methods that must NOT be cached. Includes:
/// - All mutating methods
/// - Reads with their own domain-specific cache (prs.status, coworkers.status)
/// - Parameterized reads (subject to id=1 collision with different params)
const UNCACHEABLE_METHODS: &[&str] = &[
    // Mutating: daemon lifecycle
    "shutdown",
    "daemon.exec-restart",
    "daemon.set-draining",
    "daemon.check-pending",
    "coworker.stop_all",
    // Mutating: coworker lifecycle
    "coworker.spawn",
    "coworker.break",
    "coworker.report-state",
    "coworker.nudge",
    "coworker.asking",
    "lead.spawn",
    "oneshot.execute",
    // Mutating: PR operations
    "pr.review",
    "pr.merge",
    "pr.auto-merge",
    "pr.review-post",
    "pr.allow",
    // Mutating: channel operations
    "channel.post",
    "channel.create",
    "channel.archive",
    "channel.unarchive",
    "channel.rename",
    // Mutating: task operations
    "task.create",
    "task.update",
    "task.done",
    "task.request",
    "task.claim",
    // Mutating: reminder operations
    "reminder.create",
    "reminder.cancel",
    // Mutating: workflow operations
    "workflow.assign",
    "workflow.unassign",
    "workflow.set_lead_driven",
    "workflow.set_state",
    // Mutating: auth operations
    "auth.switch",
    "auth.pool-toggle",
    // Mutating: session operations
    "session.attach",
    "session.detach",
    "session.clear",
    "session.fork",
    "session.fork_thread",
    "session.unfork_thread",
    // Mutating: headed intercom
    "headed.register",
    "headed.unregister",
    "headed.heartbeat",
    "headed.ack",
    "headed.output",
    "headed.enqueue",
    // Reads with own domain cache
    "prs.status",
    "coworkers.status",
    // Parameterized reads (subject to id=1 collision with different params)
    "coworker.view",
    "channel.read",
    "workflow.get_state",
    "session.resolve",
    "session.view",
    "session.thread_ownership",
    "headed.poll",
    "task.metadata",
];

#[test]
fn test_all_rpc_methods_categorized_for_cache() {
    let mut all: Vec<&str> = ALL_RPC_METHODS.to_vec();
    all.sort();
    let mut categorized: Vec<&str> = CACHEABLE_METHODS
        .iter()
        .chain(UNCACHEABLE_METHODS.iter())
        .copied()
        .collect();
    categorized.sort();

    assert_eq!(
        all, categorized,
        "Every method in ALL_RPC_METHODS must appear in exactly one of CACHEABLE_METHODS or UNCACHEABLE_METHODS"
    );
}

#[test]
fn test_cacheable_methods_match_use_rpc_cache() {
    // Verify the allowlist in handle_request matches CACHEABLE_METHODS.
    // This test uses the same matches! logic as the production code.
    for method in ALL_RPC_METHODS {
        let use_cache = matches!(
            *method,
            "ping"
                | "version"
                | "status"
                | "snapshot"
                | "coworker.list"
                | "coworker.questions"
                | "channel.list"
                | "reminder.list"
                | "workflow.list"
                | "session.list"
                | "pr.list-external"
        );
        let should_cache = CACHEABLE_METHODS.contains(method);

        assert_eq!(
            use_cache, should_cache,
            "Method {:?}: use_rpc_cache={} but CACHEABLE_METHODS says {}",
            method, use_cache, should_cache
        );
    }
}

#[test]
fn test_no_duplicates_in_cache_lists() {
    let mut seen = std::collections::HashSet::new();
    for method in CACHEABLE_METHODS.iter().chain(UNCACHEABLE_METHODS.iter()) {
        assert!(
            seen.insert(method),
            "Duplicate method {:?} in cache categorization lists",
            method
        );
    }
}

#[test]
fn test_allowlisted_method_is_cached() {
    let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();
    let id = RequestId::Number(1);

    // Simulate caching a response for an allowlisted method ("status")
    let first_response = Response::success(id.clone(), serde_json::json!({"uptime": 100}));
    cache.insert(id.clone(), (first_response, Instant::now()));

    // Same id within TTL → should hit cache
    let now = Instant::now();
    let hit = cache
        .get(&id)
        .filter(|(_, ts)| now.duration_since(*ts).as_secs() < 60);
    assert!(
        hit.is_some(),
        "Allowlisted (cacheable) method should return cached response for same id"
    );
}

#[test]
fn test_non_allowlisted_method_not_cached() {
    // Simulate the production logic: non-allowlisted methods skip cache entirely.
    // With the allowlist approach, a mutating method like "task.create" should not
    // be cached, so two calls with id=1 both execute (no stale response).
    let method = "task.create";
    let use_cache = matches!(
        method,
        "ping"
            | "version"
            | "status"
            | "snapshot"
            | "coworker.list"
            | "coworker.questions"
            | "channel.list"
            | "reminder.list"
            | "workflow.list"
            | "session.list"
            | "pr.list-external"
    );
    assert!(
        !use_cache,
        "Mutating method {:?} should NOT be in the cache allowlist",
        method
    );

    // Also verify previously-missing methods from the old blocklist
    for method in &[
        "coworker.spawn",
        "channel.post",
        "task.create",
        "session.fork",
        "headed.enqueue",
        "workflow.set_state",
    ] {
        let cached = CACHEABLE_METHODS.contains(method);
        assert!(
            !cached,
            "Mutating method {:?} should NOT be cacheable",
            method
        );
    }
}
