//! E2E tests for channel RPC routing — specifically that channel.read respects
//! the `channel` parameter so that channel leads read from their topic channel
//! instead of the main channel.
//!
//! Regression tests for: user messages sent to topic channels from the web UI
//! not being visible to channel leads because channel.read always read from the
//! main channel.
//!
//! Run with `cargo test --test channel_rpc_routing_test -- --ignored` as these
//! spawn a real daemon.

mod common;

use common::{DaemonHarnessOptions, DaemonTestHarness};

/// Verify that channel.read with a channel parameter returns messages from
/// the topic channel, not the main channel.
///
/// Regression test: channel leads calling `midtown channel read` always got
/// main channel messages instead of their topic channel messages, causing them
/// to miss user messages posted to their topic channel from the web UI.
#[test]
#[ignore] // E2E test - requires daemon
fn test_channel_read_topic_channel_routing() {
    let mut fixture = DaemonTestHarness::new("channel-rpc-test", DaemonHarnessOptions::default())
        .expect("Failed to create test fixture");

    assert!(fixture.start_daemon(), "Failed to start daemon");

    // Post a message to the topic channel (simulating a web UI user message)
    let post_topic = fixture
        .rpc_call(
            "channel.post",
            Some(serde_json::json!({
                "message": "hello from topic channel",
                "from": "user",
                "channel": "auth"
            })),
        )
        .expect("channel.post to topic channel should succeed");
    assert!(
        post_topic.get("error").is_none(),
        "channel.post to topic channel should succeed: {:?}",
        post_topic
    );

    // Post a different message to the main channel
    let post_main = fixture
        .rpc_call(
            "channel.post",
            Some(serde_json::json!({
                "message": "hello from main channel",
                "from": "user"
            })),
        )
        .expect("channel.post to main channel should succeed");
    assert!(
        post_main.get("error").is_none(),
        "channel.post to main channel should succeed: {:?}",
        post_main
    );

    // Read from the topic channel — should return only the topic message
    let read_topic = fixture
        .rpc_call(
            "channel.read",
            Some(serde_json::json!({
                "all": true,
                "channel": "auth"
            })),
        )
        .expect("channel.read from topic channel should succeed");
    assert!(
        read_topic.get("error").is_none(),
        "channel.read from topic channel should succeed: {:?}",
        read_topic
    );

    let topic_messages = read_topic["result"]["messages"]
        .as_array()
        .expect("messages should be an array");
    assert_eq!(
        topic_messages.len(),
        1,
        "Topic channel should have exactly 1 message, got: {:?}",
        topic_messages
    );
    assert!(
        topic_messages[0]["message"]
            .as_str()
            .unwrap_or("")
            .contains("hello from topic channel"),
        "Topic channel message should be the one posted to 'auth', got: {:?}",
        topic_messages[0]
    );

    // Read from the main channel (no channel param) — should return only the main message
    let read_main = fixture
        .rpc_call(
            "channel.read",
            Some(serde_json::json!({
                "all": true
            })),
        )
        .expect("channel.read from main channel should succeed");
    assert!(
        read_main.get("error").is_none(),
        "channel.read from main channel should succeed: {:?}",
        read_main
    );

    let main_messages = read_main["result"]["messages"]
        .as_array()
        .expect("messages should be an array");
    assert_eq!(
        main_messages.len(),
        1,
        "Main channel should have exactly 1 message, got: {:?}",
        main_messages
    );
    assert!(
        main_messages[0]["message"]
            .as_str()
            .unwrap_or("")
            .contains("hello from main channel"),
        "Main channel message should be the one posted without channel param, got: {:?}",
        main_messages[0]
    );
}
