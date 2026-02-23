//! Tests for session_render — ANSI rendering and per-event rendering.

use super::*;

#[test]
fn test_render_ansi_plain_text() {
    let input = "Hello world";
    let output = render_ansi(input);
    assert!(output.contains("Hello world"));
}

#[test]
fn test_render_ansi_bold_header() {
    let input = "## My Header";
    let output = render_ansi(input);
    // Should have bold escape code
    assert!(output.contains("\x1b[1m"));
    assert!(output.contains("My Header"));
}

#[test]
fn test_render_ansi_code_fence() {
    let input = "Before\n```bash\nls -la\n```\nAfter";
    let output = render_ansi(input);
    assert!(output.contains("Before"));
    assert!(output.contains("After"));
    // Code block should have ANSI color sequences
    assert!(output.contains("\x1b["));
}

#[test]
fn test_render_ansi_tool_header() {
    let input = "**[Bash]**";
    let output = render_ansi(input);
    assert!(output.contains("[Bash]"));
    // Should be bold + green
    assert!(output.contains("\x1b[1m"));
}

#[test]
fn test_render_ansi_inline_code() {
    let input = "Use `cargo build` to compile";
    let output = render_ansi(input);
    assert!(output.contains("cargo build"));
    assert!(output.contains("\x1b["));
}

#[test]
fn test_render_event_line_text() {
    let event = serde_json::json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello from assistant"}]
        }
    });
    let line = serde_json::to_string(&event).unwrap();
    let result = render_event_line(&line);
    assert!(result.is_some());
    assert!(result.unwrap().contains("Hello from assistant"));
}

#[test]
fn test_render_event_line_system_returns_none() {
    let event = serde_json::json!({
        "type": "system",
        "subtype": "init",
        "session_id": "abc123"
    });
    let line = serde_json::to_string(&event).unwrap();
    let result = render_event_line(&line);
    assert!(result.is_none());
}
