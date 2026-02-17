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
