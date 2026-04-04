use super::*;
use crate::channel::Channel;
use crate::headless::StreamEvent;
use tempfile::TempDir;

/// Normal (non-error) assistant text should be posted.
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

/// Error suppression is handled by the drain loop (session_errored flag),
/// not by flush_auto_output. Verify flush_auto_output itself is a pure
/// "extract and post" function — it posts even when is_error is true,
/// because the drain loop is expected to skip the call entirely.
#[test]
fn flush_auto_output_does_not_filter_errors_itself() {
    let dir = TempDir::new().unwrap();
    let (event_tx, _rx) = tokio::sync::broadcast::channel(16);

    let mut events = vec![
        StreamEvent::Assistant {
            message: serde_json::json!({
                "content": [{"type": "text", "text": "Failed to authenticate. API Error: 401"}]
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

    // flush_auto_output is now a pure post function — the drain loop
    // is responsible for NOT calling it when session_errored is true.
    let ch = Channel::new(dir.path(), "daemon-core").unwrap();
    let msgs = ch.read_all().unwrap();
    assert_eq!(
        msgs.len(),
        1,
        "flush_auto_output posts unconditionally — drain loop gates the call"
    );
}
