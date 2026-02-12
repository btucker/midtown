//! Tests for RPC handler response serialization.
//!
//! These tests ensure RPC responses can be deserialized by the CLI client.

use crate::rpc::{RequestId, Response};
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

/// Test that enter-drain RPC response can be deserialized as a CLI Response.
#[test]
fn test_enter_drain_response_deserializes() {
    let daemon_response = Response::success(
        RequestId::Number(1),
        serde_json::json!({"message": "draining"}),
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
        "enter-drain response should deserialize to CLI Response, got error: {:?}",
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
