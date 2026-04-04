use super::*;
use crate::channel::Channel;
use crate::headless::StreamEvent;
use tempfile::TempDir;

/// When a session exits with is_error: true (e.g. expired OAuth token),
/// the error text should NOT be auto-posted to the channel.
#[test]
fn flush_auto_output_suppresses_error_results() {
    let dir = TempDir::new().unwrap();
    let (event_tx, _rx) = tokio::sync::broadcast::channel(16);

    let mut events = vec![
        StreamEvent::Assistant {
            message: serde_json::json!({
                "content": [{"type": "text", "text": "Failed to authenticate. API Error: 401 {\"type\":\"error\"}"}]
            }),
            session_id: None,
            extra: serde_json::Value::Null,
        },
        StreamEvent::Result {
            subtype: "error".into(),
            is_error: true,
            result: None,
            duration_ms: None,
            total_cost_usd: None,
            session_id: None,
            usage: None,
            extra: serde_json::json!({}),
        },
    ];

    flush_auto_output(
        "daemon-core",
        &Some("daemon-core".into()),
        None,
        dir.path(),
        &mut events,
        &event_tx,
    );

    // Error output should NOT appear as a channel message
    let ch = Channel::new(dir.path(), "daemon-core").unwrap();
    let msgs = ch.read_all().unwrap();
    assert!(
        msgs.is_empty(),
        "error output should not be posted to channel, but got {} messages",
        msgs.len()
    );
}

/// Normal (non-error) assistant text should still be posted.
#[test]
fn flush_auto_output_posts_normal_results() {
    let dir = TempDir::new().unwrap();
    let (event_tx, _rx) = tokio::sync::broadcast::channel(16);

    let mut events = vec![
        StreamEvent::Assistant {
            message: serde_json::json!({
                "content": [{"type": "text", "text": "Here is the status report."}]
            }),
            session_id: None,
            extra: serde_json::Value::Null,
        },
        StreamEvent::Result {
            subtype: "success".into(),
            is_error: false,
            result: None,
            duration_ms: None,
            total_cost_usd: None,
            session_id: None,
            usage: None,
            extra: serde_json::json!({}),
        },
    ];

    flush_auto_output(
        "daemon-core",
        &Some("daemon-core".into()),
        None,
        dir.path(),
        &mut events,
        &event_tx,
    );

    let ch = Channel::new(dir.path(), "daemon-core").unwrap();
    let msgs = ch.read_all().unwrap();
    assert_eq!(msgs.len(), 1, "normal output should be posted");
}
