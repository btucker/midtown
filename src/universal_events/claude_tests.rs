use super::*;
use crate::headless::StreamEvent;
use chrono::{TimeZone, Utc};
use serde_json::json;

// ── semantic_header tests ────────────────────────────────────────────

#[test]
fn test_semantic_header_bash() {
    let input = json!({"command": "git status"});
    assert_eq!(semantic_header("Bash", &input), "$ git status");
}

#[test]
fn test_semantic_header_bash_truncation() {
    // A command longer than 60 chars should be truncated with ellipsis.
    let long_cmd = "a".repeat(70);
    let input = json!({"command": long_cmd});
    let header = semantic_header("Bash", &input);
    // Should start with "$ " and be at most "$ " + 59 chars + "…"
    assert!(header.starts_with("$ "));
    // The visible part after "$ " should end with "…"
    assert!(
        header.ends_with('\u{2026}'),
        "Expected ellipsis at end, got: {header}"
    );
    // Total char count: "$ " (2) + 59 'a's + "…" (1) = 62
    let char_count = header.chars().count();
    assert_eq!(char_count, 62, "Expected 62 chars total, got {char_count}");
}

#[test]
fn test_semantic_header_bash_exactly_60_chars_no_truncation() {
    let cmd = "b".repeat(60);
    let input = json!({"command": cmd});
    let header = semantic_header("Bash", &input);
    assert_eq!(header, format!("$ {cmd}"));
    assert!(!header.ends_with('\u{2026}'));
}

#[test]
fn test_semantic_header_edit() {
    let input = json!({"file_path": "src/main.rs", "old_string": "foo", "new_string": "bar"});
    assert_eq!(semantic_header("Edit", &input), "edit src/main.rs");
}

#[test]
fn test_semantic_header_write() {
    let input = json!({"file_path": "src/foo.rs", "content": "hello"});
    assert_eq!(semantic_header("Write", &input), "write src/foo.rs");
}

#[test]
fn test_semantic_header_read() {
    let input = json!({"file_path": "src/lib.rs"});
    assert_eq!(semantic_header("Read", &input), "read src/lib.rs");
}

#[test]
fn test_semantic_header_glob() {
    let input = json!({"pattern": "**/*.rs"});
    assert_eq!(semantic_header("Glob", &input), "glob **/*.rs");
}

#[test]
fn test_semantic_header_grep() {
    let input = json!({"pattern": "fn main"});
    assert_eq!(semantic_header("Grep", &input), "grep /fn main/");
}

#[test]
fn test_semantic_header_task() {
    let input = json!({"description": "explore codebase"});
    assert_eq!(semantic_header("Task", &input), "task: explore codebase");
}

#[test]
fn test_semantic_header_task_truncation() {
    let long_desc = "x".repeat(50);
    let input = json!({"description": long_desc});
    let header = semantic_header("Task", &input);
    assert!(header.starts_with("task: "));
    // 40 chars of description max
    let desc_part = &header["task: ".len()..];
    assert_eq!(desc_part.chars().count(), 40);
}

#[test]
fn test_semantic_header_notebook_edit() {
    let input = json!({"notebook_path": "analysis.ipynb"});
    assert_eq!(
        semantic_header("NotebookEdit", &input),
        "notebook edit analysis.ipynb"
    );
}

#[test]
fn test_semantic_header_web_fetch_extracts_host() {
    let input = json!({"url": "https://example.com/path/to/page"});
    assert_eq!(semantic_header("WebFetch", &input), "fetch example.com");
}

#[test]
fn test_semantic_header_web_fetch_with_port() {
    let input = json!({"url": "http://localhost:8080/api"});
    assert_eq!(semantic_header("WebFetch", &input), "fetch localhost");
}

#[test]
fn test_semantic_header_web_search() {
    let input = json!({"query": "rust async"});
    assert_eq!(
        semantic_header("WebSearch", &input),
        "search \"rust async\""
    );
}

#[test]
fn test_semantic_header_todo_write() {
    let input = json!({"todos": []});
    assert_eq!(semantic_header("TodoWrite", &input), "todo: update");
}

#[test]
fn test_semantic_header_exit_plan_mode() {
    assert_eq!(
        semantic_header("ExitPlanMode", &json!({})),
        "exit plan mode"
    );
}

#[test]
fn test_semantic_header_multi_edit() {
    let input = json!({"file_path": "src/lib.rs", "edits": []});
    assert_eq!(
        semantic_header("MultiEdit", &input),
        "multi-edit src/lib.rs"
    );
}

#[test]
fn test_semantic_header_default_lowercases_name() {
    let input = json!({});
    assert_eq!(semantic_header("UnknownTool", &input), "unknowntool");
}

#[test]
fn test_semantic_header_read_uses_path_fallback() {
    // When file_path is absent, falls back to path field.
    let input = json!({"path": "/some/path"});
    assert_eq!(semantic_header("Read", &input), "read /some/path");
}

// ── extract_tool_events: tool call tests ─────────────────────────────

#[test]
fn test_extract_tool_events_single_tool_use() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [{
                "type": "tool_use",
                "id": "call_001",
                "name": "Read",
                "input": {"file_path": "/tmp/test.rs"}
            }]
        }),
        session_id: None,
        extra: json!(null),
    }];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_events(&events, timestamp);
    assert_eq!(items.len(), 1);

    let item = &items[0];
    assert_eq!(item.item_id, "call_001");
    assert!(matches!(item.kind, ItemKind::ToolCall));
    assert!(matches!(item.status, ItemStatus::Completed));
    assert_eq!(item.timestamp, timestamp);
    assert_eq!(item.content.len(), 1);

    match &item.content[0] {
        ContentPart::ToolCall {
            name,
            input,
            call_id,
            semantic_header: header,
        } => {
            assert_eq!(name, "Read");
            assert_eq!(input, &json!({"file_path": "/tmp/test.rs"}));
            assert_eq!(call_id, "call_001");
            assert_eq!(header, "read /tmp/test.rs");
        }
        _ => panic!("Expected ToolCall content part"),
    }
}

#[test]
fn test_extract_tool_events_multiple_tool_uses() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [
                {
                    "type": "tool_use",
                    "id": "call_001",
                    "name": "Read",
                    "input": {"file_path": "/tmp/a.rs"}
                },
                {
                    "type": "tool_use",
                    "id": "call_002",
                    "name": "Write",
                    "input": {"file_path": "/tmp/b.rs", "content": "hello"}
                }
            ]
        }),
        session_id: None,
        extra: json!(null),
    }];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_events(&events, timestamp);
    assert_eq!(items.len(), 2);

    assert_eq!(items[0].item_id, "call_001");
    assert_eq!(items[1].item_id, "call_002");

    match &items[0].content[0] {
        ContentPart::ToolCall {
            name,
            call_id,
            semantic_header: header,
            ..
        } => {
            assert_eq!(name, "Read");
            assert_eq!(call_id, "call_001");
            assert_eq!(header, "read /tmp/a.rs");
        }
        _ => panic!("Expected ToolCall"),
    }

    match &items[1].content[0] {
        ContentPart::ToolCall {
            name,
            call_id,
            semantic_header: header,
            ..
        } => {
            assert_eq!(name, "Write");
            assert_eq!(call_id, "call_002");
            assert_eq!(header, "write /tmp/b.rs");
        }
        _ => panic!("Expected ToolCall"),
    }
}

#[test]
fn test_extract_tool_events_text_only_no_items() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [
                {"type": "text", "text": "I will read the file now."},
                {"type": "text", "text": "Here is the result."}
            ]
        }),
        session_id: None,
        extra: json!(null),
    }];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_events(&events, timestamp);
    assert!(items.is_empty());
}

#[test]
fn test_extract_tool_events_mixed_content() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [
                {"type": "text", "text": "Let me read that file."},
                {
                    "type": "tool_use",
                    "id": "call_100",
                    "name": "Bash",
                    "input": {"command": "ls"}
                },
                {"type": "text", "text": "Done."}
            ]
        }),
        session_id: None,
        extra: json!(null),
    }];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_events(&events, timestamp);
    assert_eq!(items.len(), 1);

    assert_eq!(items[0].item_id, "call_100");
    match &items[0].content[0] {
        ContentPart::ToolCall {
            name,
            semantic_header: header,
            ..
        } => {
            assert_eq!(name, "Bash");
            assert_eq!(header, "$ ls");
        }
        _ => panic!("Expected ToolCall"),
    }
}

#[test]
fn test_extract_tool_events_non_assistant_non_user_events_skipped() {
    let events = vec![
        StreamEvent::System {
            subtype: "init".to_string(),
            session_id: Some("sess-1".to_string()),
            model: Some("sonnet".to_string()),
            extra: json!({}),
        },
        StreamEvent::Result {
            subtype: "success".to_string(),
            is_error: false,
            result: Some("ok".to_string()),
            duration_ms: Some(1000),
            total_cost_usd: Some(0.01),
            session_id: Some("sess-1".to_string()),
            extra: json!({}),
        },
    ];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_events(&events, timestamp);
    assert!(items.is_empty());
}

#[test]
fn test_extract_tool_events_empty_events() {
    let events: Vec<StreamEvent> = vec![];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_events(&events, timestamp);
    assert!(items.is_empty());
}

// ── extract_tool_events: tool result tests ───────────────────────────

#[test]
fn test_extract_tool_events_tool_result_string_content() {
    let events = vec![StreamEvent::User {
        message: json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call_001",
                "content": "file contents here..."
            }]
        }),
        extra: json!({}),
    }];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_events(&events, timestamp);
    assert_eq!(items.len(), 1);

    let item = &items[0];
    assert_eq!(item.item_id, "call_001");
    assert!(matches!(item.kind, ItemKind::ToolCall));
    assert!(matches!(item.status, ItemStatus::Completed));

    match &item.content[0] {
        ContentPart::ToolResult {
            call_id,
            output,
            is_error,
        } => {
            assert_eq!(call_id, "call_001");
            assert_eq!(output, "file contents here...");
            assert!(!is_error);
        }
        _ => panic!("Expected ToolResult content part"),
    }
}

#[test]
fn test_extract_tool_events_tool_result_array_content() {
    let events = vec![StreamEvent::User {
        message: json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call_002",
                "content": [
                    {"type": "text", "text": "Hello "},
                    {"type": "text", "text": "world"}
                ]
            }]
        }),
        extra: json!({}),
    }];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_events(&events, timestamp);
    assert_eq!(items.len(), 1);

    match &items[0].content[0] {
        ContentPart::ToolResult {
            call_id, output, ..
        } => {
            assert_eq!(call_id, "call_002");
            assert_eq!(output, "Hello world");
        }
        _ => panic!("Expected ToolResult"),
    }
}

#[test]
fn test_extract_tool_events_tool_result_missing_content() {
    let events = vec![StreamEvent::User {
        message: json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call_003"
                // no "content" field
            }]
        }),
        extra: json!({}),
    }];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_events(&events, timestamp);
    assert_eq!(items.len(), 1);

    match &items[0].content[0] {
        ContentPart::ToolResult { output, .. } => {
            assert_eq!(output, "");
        }
        _ => panic!("Expected ToolResult"),
    }
}

#[test]
fn test_extract_tool_events_tool_result_is_error_true() {
    let events = vec![StreamEvent::User {
        message: json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call_004",
                "content": "command failed",
                "is_error": true
            }]
        }),
        extra: json!({}),
    }];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_events(&events, timestamp);
    assert_eq!(items.len(), 1);

    match &items[0].content[0] {
        ContentPart::ToolResult {
            call_id,
            output,
            is_error,
        } => {
            assert_eq!(call_id, "call_004");
            assert_eq!(output, "command failed");
            assert!(is_error);
        }
        _ => panic!("Expected ToolResult"),
    }
}

#[test]
fn test_extract_tool_events_tool_result_multiple_results() {
    let events = vec![StreamEvent::User {
        message: json!({
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "call_005",
                    "content": "result one"
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "call_006",
                    "content": "result two"
                }
            ]
        }),
        extra: json!({}),
    }];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_events(&events, timestamp);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].item_id, "call_005");
    assert_eq!(items[1].item_id, "call_006");
}

#[test]
fn test_extract_tool_events_user_event_non_tool_result_skipped() {
    // User events whose content is not tool_result blocks should produce no items.
    let events = vec![StreamEvent::User {
        message: json!({"content": "do something"}),
        extra: json!({}),
    }];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_events(&events, timestamp);
    assert!(items.is_empty());
}

#[test]
fn test_extract_tool_events_mixed_assistant_and_user_events() {
    // Both a tool call and its result appear in the same event slice.
    let events = vec![
        StreamEvent::Assistant {
            message: json!({
                "content": [{
                    "type": "tool_use",
                    "id": "call_007",
                    "name": "Bash",
                    "input": {"command": "pwd"}
                }]
            }),
            session_id: None,
            extra: json!(null),
        },
        StreamEvent::User {
            message: json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call_007",
                    "content": "/home/user"
                }]
            }),
            extra: json!({}),
        },
    ];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_events(&events, timestamp);
    assert_eq!(items.len(), 2);

    // First item is the tool call
    assert!(matches!(&items[0].content[0], ContentPart::ToolCall { .. }));
    // Second item is the tool result
    match &items[1].content[0] {
        ContentPart::ToolResult {
            call_id, output, ..
        } => {
            assert_eq!(call_id, "call_007");
            assert_eq!(output, "/home/user");
        }
        _ => panic!("Expected ToolResult"),
    }
}

// ── backward-compatible alias tests ──────────────────────────────────

#[test]
fn test_extract_tool_calls_alias_works() {
    // The public alias should produce the same results as extract_tool_events.
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [{
                "type": "tool_use",
                "id": "call_alias",
                "name": "Read",
                "input": {"file_path": "/tmp/alias.rs"}
            }]
        }),
        session_id: None,
        extra: json!(null),
    }];

    let timestamp = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    let items = extract_tool_calls(&events, timestamp);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].item_id, "call_alias");
}
