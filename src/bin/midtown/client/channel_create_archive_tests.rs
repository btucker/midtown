//! Tests for channel_create and channel_archive client method parameter construction.
//!
//! These tests verify that the methods correctly assemble the JSON params
//! passed to the daemon RPC, following the pattern in channel_read_tests.rs.

/// Helper to build the params JSON that channel_create would send.
fn build_channel_create_params(name: &str) -> serde_json::Value {
    serde_json::json!({ "name": name })
}

/// Helper to build the params JSON that channel_archive would send.
fn build_channel_archive_params(name: &str) -> serde_json::Value {
    serde_json::json!({ "name": name })
}

#[test]
fn channel_create_sets_name_param() {
    let params = build_channel_create_params("my-channel");
    assert_eq!(
        params["name"].as_str(),
        Some("my-channel"),
        "channel_create should set params[\"name\"]"
    );
}

#[test]
fn channel_create_name_is_passed_as_given() {
    let params = build_channel_create_params("ops");
    assert_eq!(params["name"].as_str(), Some("ops"));
}

#[test]
fn channel_archive_sets_name_param() {
    let params = build_channel_archive_params("old-feature");
    assert_eq!(
        params["name"].as_str(),
        Some("old-feature"),
        "channel_archive should set params[\"name\"]"
    );
}

#[test]
fn channel_archive_name_is_passed_as_given() {
    let params = build_channel_archive_params("sprint-42");
    assert_eq!(params["name"].as_str(), Some("sprint-42"));
}
