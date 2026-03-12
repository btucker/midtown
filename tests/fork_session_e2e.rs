//! End-to-end tests for the fork session lifecycle.
//!
//! These tests exercise the `session.fork` and `session.fork_thread` RPC
//! endpoints through a running daemon, covering:
//!
//! 1. **Parameter validation**: UUID format, required fields
//! 2. **Spawn failure handling**: error shape, sentinel cleanup, retryability
//! 3. **Fork launch path**: valid params traverse validation → session lookup → spawn
//! 4. **Fork thread binding**: `session.fork_thread` endpoint dispatches correctly
//! 5. **Name derivation**: `name_hint` and `initial_message` params reach the handler
//!
//! Without a real Claude binary, fork spawns fail at the process level. These
//! tests verify everything up to (and including) the spawn attempt, plus the
//! daemon's error recovery after spawn failure.
//!
//! Run with: `cargo test --test fork_session_e2e -- --ignored --test-threads=1`

use std::path::PathBuf;

mod common;
use common::{DaemonHarnessOptions, DaemonTestHarness};

/// Create a DaemonTestHarness configured for fork session E2E tests.
///
/// Uses a short XDG_STATE_HOME under /tmp to avoid UNIX socket path-length
/// limits (SUN_LEN) on systems with long temporary directory prefixes.
fn create_fork_fixture() -> Option<DaemonTestHarness> {
    let prefix = "fork-e2e-test";
    let state_dir = PathBuf::from("/tmp").join("ms-fork-e2e");

    DaemonTestHarness::new(
        prefix,
        DaemonHarnessOptions {
            custom_state_dir: Some(state_dir),
            ..Default::default()
        },
    )
}

// ── session.fork validation tests ───────────────────────────────────

/// session.fork rejects Claude API message IDs (non-UUID thread_parent_id).
///
/// The daemon validates that thread_parent_id is a UUID to prevent forks
/// bound to non-existent threads. Claude API IDs like "msg_01abc123" must
/// be rejected with error code -32602.
#[test]
#[ignore]
fn test_session_fork_rejects_non_uuid_thread_parent_id() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "thread_parent_id": "msg_01abc123",
            "calling_session_id": "test-session-id"
        })),
    );

    let response = response.expect("Should receive response");
    let error = response["error"]
        .as_object()
        .expect("Should be an error response");
    let code = error["code"].as_i64().unwrap();
    assert_eq!(code, -32602, "Should return invalid params error code");
    let message = error["message"].as_str().unwrap_or("");
    assert!(
        message.contains("Invalid thread_parent_id"),
        "Error message should mention Invalid thread_parent_id, got: {}",
        message
    );
}

/// session.fork rejects empty string thread_parent_id.
#[test]
#[ignore]
fn test_session_fork_rejects_empty_thread_parent_id() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "thread_parent_id": "",
            "calling_session_id": "test-session-id"
        })),
    );

    let response = response.expect("Should receive response");
    let error = response["error"]
        .as_object()
        .expect("Should be an error response");
    let code = error["code"].as_i64().unwrap();
    assert_eq!(code, -32602, "Should return invalid params error code");
}

/// session.fork requires calling_session_id parameter.
///
/// When calling_session_id is missing, the require_str! macro returns
/// a -32602 invalid params error before the handler logic runs.
#[test]
#[ignore]
fn test_session_fork_requires_calling_session_id() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "thread_parent_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
        })),
    );

    let response = response.expect("Should receive response");
    let error = response["error"]
        .as_object()
        .expect("Should be an error response");
    let code = error["code"].as_i64().unwrap();
    assert_eq!(
        code, -32602,
        "Missing calling_session_id should return invalid params error"
    );
}

/// session.fork requires thread_parent_id parameter.
///
/// When thread_parent_id is missing, the require_str! macro returns
/// a -32602 invalid params error before the handler logic runs.
#[test]
#[ignore]
fn test_session_fork_requires_thread_parent_id() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "calling_session_id": "test-session-id"
        })),
    );

    let response = response.expect("Should receive response");
    let error = response["error"]
        .as_object()
        .expect("Should be an error response");
    let code = error["code"].as_i64().unwrap();
    assert_eq!(
        code, -32602,
        "Missing thread_parent_id should return invalid params error"
    );
}

// ── session.fork spawn failure tests ────────────────────────────────

/// session.fork spawn failure returns an error response.
///
/// In a test environment with no real Claude binary, the fork spawn will
/// fail. The daemon should return an error (not a success) in this case.
#[test]
#[ignore]
fn test_session_fork_spawn_failure_returns_error() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "thread_parent_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
            "calling_session_id": "fake-session-that-does-not-exist"
        })),
    );

    let response = response.expect("Should receive response");
    // The spawn will fail because there's no real session to fork from.
    // This should be an error, not a success.
    assert!(
        response["error"].is_object(),
        "Spawn failure should return an error response, got: {}",
        response
    );
}

/// session.fork cleans up the "pending" sentinel after spawn failure.
///
/// When a fork fails to spawn, the daemon inserts a sentinel into
/// topic_sessions and must clean it up on failure. Otherwise, a retry
/// with the same thread_parent_id would get a "fork in progress" error
/// instead of being allowed to try again.
#[test]
#[ignore]
fn test_session_fork_spawn_failure_cleans_sentinel() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let thread_id = "b2c3d4e5-f6a7-8901-bcde-f12345678901";

    // First attempt: fork will fail (no Claude binary / no real session)
    let response1 = fixture.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "thread_parent_id": thread_id,
            "calling_session_id": "fake-session-that-does-not-exist"
        })),
    );
    let response1 = response1.expect("Should receive first response");
    // First call should fail with an error (spawn failure)
    assert!(
        response1["error"].is_object(),
        "First fork attempt should fail, got: {}",
        response1
    );

    // Second attempt with the SAME thread_parent_id: if the sentinel was
    // cleaned up properly, this should NOT return "fork in progress". It
    // should fail the same way (spawn failure) again.
    let response2 = fixture.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "thread_parent_id": thread_id,
            "calling_session_id": "fake-session-that-does-not-exist"
        })),
    );
    let response2 = response2.expect("Should receive second response");

    // If sentinel was NOT cleaned up, we'd get a success with "pending: true"
    // and "fork in progress" message. That would be a bug.
    if response2["result"].is_object() {
        let pending = response2["result"]["pending"].as_bool().unwrap_or(false);
        assert!(
            !pending,
            "Second fork attempt should NOT return 'fork in progress' — sentinel was not cleaned up"
        );
    }
    // The second attempt should either be an error (same spawn failure)
    // or a success with already_exists=true (if somehow the first succeeded).
    // It must NOT be a pending/in-progress response.
}

// ── session.fork_thread validation tests ────────────────────────────

/// session.fork_thread requires the channel parameter.
///
/// When channel is missing, the require_str! macro returns a -32602 error.
#[test]
#[ignore]
fn test_session_fork_thread_requires_channel() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.fork_thread",
        Some(serde_json::json!({
            "thread_parent_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
        })),
    );

    let response = response.expect("Should receive response");
    let error = response["error"]
        .as_object()
        .expect("Missing channel should return error");
    let code = error["code"].as_i64().unwrap();
    assert_eq!(
        code, -32602,
        "Missing channel should return invalid params error"
    );
}

/// session.fork_thread requires thread_parent_id parameter.
///
/// When thread_parent_id is missing, the require_str! macro returns
/// a -32602 invalid params error.
#[test]
#[ignore]
fn test_session_fork_thread_requires_thread_parent_id() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.fork_thread",
        Some(serde_json::json!({
            "channel": "general"
        })),
    );

    let response = response.expect("Should receive response");
    let error = response["error"]
        .as_object()
        .expect("Missing thread_parent_id should return error");
    let code = error["code"].as_i64().unwrap();
    assert_eq!(
        code, -32602,
        "Missing thread_parent_id should return invalid params error"
    );
}

// ── Fork launch path tests ──────────────────────────────────────────
//
// These tests verify that session.fork with valid parameters traverses
// the full code path: RPC dispatch → UUID validation → session lookup →
// config construction → spawn attempt. In the test environment (no Claude
// binary), spawn fails — but the error proves the request reached the
// spawn step rather than being rejected at validation.

/// session.fork with valid UUID params reaches the spawn step.
///
/// The error response should be a -32603 internal error (spawn failure),
/// NOT a -32602 validation error. This proves the request passed UUID
/// validation, session lookup (with fallback defaults), fork config
/// construction, and reached the actual spawn_fork call.
#[test]
#[ignore]
fn test_session_fork_valid_params_reaches_spawn() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "thread_parent_id": "c3d4e5f6-a7b8-9012-cdef-345678901234",
            "calling_session_id": "nonexistent-but-valid-session"
        })),
    );

    let response = response.expect("Should receive response");
    let error = response["error"]
        .as_object()
        .expect("Should be an error (spawn failure in test env)");
    let code = error["code"].as_i64().unwrap();
    assert_eq!(
        code, -32603,
        "Spawn failure should return internal error (-32603), not validation error (-32602)"
    );
    let message = error["message"].as_str().unwrap_or("");
    assert!(
        message.contains("Failed to fork session")
            || message.contains("Failed to spawn fork session"),
        "Error message should mention fork session failure, got: {}",
        message
    );
}

/// session.fork passes the name_hint parameter through to config construction.
///
/// When name_hint is provided, the derived fork name should include the
/// slugified hint. The spawn error message includes the fork name, so we
/// can verify the hint was used in name derivation.
#[test]
#[ignore]
fn test_session_fork_with_name_hint() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "thread_parent_id": "d4e5f6a7-b8c9-0123-defa-456789012345",
            "calling_session_id": "nonexistent-session",
            "name": "investigate auth bug"
        })),
    );

    let response = response.expect("Should receive response");
    let error = response["error"]
        .as_object()
        .expect("Should be an error (spawn failure in test env)");
    // The error message from spawn_fork includes the fork name, which should
    // contain the slugified hint (e.g., "investigate-auth-bug" or similar).
    let message = error["message"].as_str().unwrap_or("");
    assert!(
        message.contains("investigate") || message.contains("auth"),
        "Error message should include the slugified name hint in the fork name, got: {}",
        message
    );
}

/// session.fork accepts the initial_message parameter without error.
///
/// The initial_message is used to set the fork's first nudge content.
/// In the test environment, spawn fails before the nudge is sent, but
/// the parameter should be accepted without breaking the RPC call.
#[test]
#[ignore]
fn test_session_fork_with_initial_message() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "thread_parent_id": "e5f6a7b8-c9d0-1234-efab-567890123456",
            "calling_session_id": "nonexistent-session",
            "initial_message": "Investigate the auth token expiry issue"
        })),
    );

    let response = response.expect("Should receive response");
    // Should fail at spawn, not at parameter parsing
    let error = response["error"]
        .as_object()
        .expect("Should be an error (spawn failure in test env)");
    let code = error["code"].as_i64().unwrap();
    assert_eq!(
        code, -32603,
        "initial_message param should not cause validation error, got code: {}",
        code
    );
}

/// session.fork retry after spawn failure uses the same code path.
///
/// After a spawn failure, the sentinel is cleaned up and the same
/// thread_parent_id can be retried. Both attempts should fail with
/// the same -32603 spawn error (not -32602 or "fork in progress").
#[test]
#[ignore]
fn test_session_fork_retry_after_failure_same_error() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let thread_id = "f6a7b8c9-d0e1-2345-fabc-678901234567";

    let response1 = fixture.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "thread_parent_id": thread_id,
            "calling_session_id": "nonexistent-session"
        })),
    );
    let r1 = response1.expect("Should receive first response");
    let code1 = r1["error"]["code"].as_i64().unwrap();

    let response2 = fixture.rpc_call(
        "session.fork",
        Some(serde_json::json!({
            "thread_parent_id": thread_id,
            "calling_session_id": "nonexistent-session"
        })),
    );
    let r2 = response2.expect("Should receive second response");
    let code2 = r2["error"]["code"].as_i64().unwrap();

    assert_eq!(
        code1, code2,
        "Both attempts should fail with the same error code (sentinel cleanup working)"
    );
    assert_eq!(code1, -32603, "Should be spawn failure, not validation");
}

// ── session.fork_thread handler tests ───────────────────────────────
//
// session.fork_thread is the web-UI-triggered fork path. It looks up
// the channel lead session for the given channel and delegates to
// create_fork_session. Without a registered channel lead, it returns
// a clear error.

/// session.fork_thread with valid params but no channel lead returns error.
///
/// The daemon starts with no channel lead sessions registered. When
/// fork_thread is called with a valid UUID and channel name, it should
/// return an error about the missing channel lead (not a validation error).
#[test]
#[ignore]
fn test_session_fork_thread_no_channel_lead() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.fork_thread",
        Some(serde_json::json!({
            "thread_parent_id": "a7b8c9d0-e1f2-3456-abcd-789012345678",
            "channel": "web"
        })),
    );

    let response = response.expect("Should receive response");
    let error = response["error"]
        .as_object()
        .expect("Should be an error (no channel lead registered)");
    let message = error["message"].as_str().unwrap_or("");
    // The error should mention channel lead or session, not UUID validation
    assert!(
        message.contains("channel lead") || message.contains("session"),
        "Error should mention missing channel lead, got: {}",
        message
    );
}

/// session.fork_thread rejects non-UUID thread_parent_id.
///
/// UUID validation applies to fork_thread the same as session.fork.
#[test]
#[ignore]
fn test_session_fork_thread_rejects_non_uuid() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.fork_thread",
        Some(serde_json::json!({
            "thread_parent_id": "msg_01NotAUUID",
            "channel": "web"
        })),
    );

    let response = response.expect("Should receive response");
    let error = response["error"]
        .as_object()
        .expect("Should reject non-UUID thread_parent_id");
    let code = error["code"].as_i64().unwrap();
    assert_eq!(code, -32602, "Non-UUID should be validation error -32602");
    let message = error["message"].as_str().unwrap_or("");
    assert!(
        message.contains("Invalid thread_parent_id"),
        "Error should mention Invalid thread_parent_id, got: {}",
        message
    );
}

// ── session.unfork_thread and thread_ownership tests ────────────────

/// session.unfork_thread returns error when no fork exists for the thread.
///
/// Without any active fork sessions, unfork_thread should return a clear
/// error rather than succeeding silently.
#[test]
#[ignore]
fn test_session_unfork_thread_no_fork_exists() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.unfork_thread",
        Some(serde_json::json!({
            "thread_parent_id": "b8c9d0e1-f2a3-4567-bcde-890123456789",
            "channel": "web"
        })),
    );

    let response = response.expect("Should receive response");
    assert!(
        response["error"].is_object(),
        "unfork_thread with no fork should return error, got: {}",
        response
    );
}

/// session.thread_ownership reports no dedicated session for unknown thread.
///
/// When no fork session is bound to a thread, the ownership query should
/// return has_dedicated_session=false.
#[test]
#[ignore]
fn test_session_thread_ownership_no_fork() {
    let mut fixture = match create_fork_fixture() {
        Some(f) => f,
        None => return,
    };
    assert!(fixture.start_daemon(), "Daemon should start");

    let response = fixture.rpc_call(
        "session.thread_ownership",
        Some(serde_json::json!({
            "thread_parent_id": "c9d0e1f2-a3b4-5678-cdef-901234567890",
            "channel": "web"
        })),
    );

    let response = response.expect("Should receive response");
    let result = &response["result"];
    assert!(
        result.is_object(),
        "thread_ownership should return a result, got: {}",
        response
    );
    assert_eq!(
        result["has_dedicated_session"], false,
        "No fork registered — should report has_dedicated_session=false"
    );
}
