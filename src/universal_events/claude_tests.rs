use super::*;
use crate::headless::StreamEvent;
use serde_json::json;

// ── extract_tool_calls tests ────────────────────────────────────────────

#[test]
fn test_extract_tool_calls_single_tool_use() {
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

    let items = extract_tool_calls(&events, "lexington");
    assert_eq!(items.len(), 1);

    let item = &items[0];
    assert!(matches!(item.kind, ItemKind::ToolCall));
    assert!(matches!(item.status, ItemStatus::InProgress));
    assert_eq!(item.content.len(), 1);

    match &item.content[0] {
        ContentPart::ToolCall {
            name,
            input,
            call_id,
        } => {
            assert_eq!(name, "Read");
            assert_eq!(input, &json!({"file_path": "/tmp/test.rs"}));
            assert_eq!(call_id, "call_001");
        }
        _ => panic!("Expected ToolCall content part"),
    }
}

#[test]
fn test_extract_tool_calls_multiple_tool_uses() {
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

    let items = extract_tool_calls(&events, "park");
    assert_eq!(items.len(), 2);

    match &items[0].content[0] {
        ContentPart::ToolCall { name, call_id, .. } => {
            assert_eq!(name, "Read");
            assert_eq!(call_id, "call_001");
        }
        _ => panic!("Expected ToolCall"),
    }

    match &items[1].content[0] {
        ContentPart::ToolCall { name, call_id, .. } => {
            assert_eq!(name, "Write");
            assert_eq!(call_id, "call_002");
        }
        _ => panic!("Expected ToolCall"),
    }
}

#[test]
fn test_extract_tool_calls_text_only_no_items() {
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

    let items = extract_tool_calls(&events, "madison");
    assert!(items.is_empty());
}

#[test]
fn test_extract_tool_calls_mixed_content() {
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

    let items = extract_tool_calls(&events, "broadway");
    assert_eq!(items.len(), 1);

    match &items[0].content[0] {
        ContentPart::ToolCall { name, .. } => {
            assert_eq!(name, "Bash");
        }
        _ => panic!("Expected ToolCall"),
    }
}

#[test]
fn test_extract_tool_calls_non_assistant_events() {
    let events = vec![
        StreamEvent::System {
            subtype: "init".to_string(),
            session_id: Some("sess-1".to_string()),
            model: Some("sonnet".to_string()),
            extra: json!({}),
        },
        StreamEvent::User {
            message: json!({"content": "do something"}),
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

    let items = extract_tool_calls(&events, "amsterdam");
    assert!(items.is_empty());
}

#[test]
fn test_extract_tool_calls_empty_events() {
    let events: Vec<StreamEvent> = vec![];

    let items = extract_tool_calls(&events, "columbus");
    assert!(items.is_empty());
}

#[test]
fn test_extract_tool_calls_agent_name_propagated() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [{
                "type": "tool_use",
                "id": "call_abc",
                "name": "Grep",
                "input": {"pattern": "fn main"}
            }]
        }),
        session_id: None,
        extra: json!(null),
    }];

    let items = extract_tool_calls(&events, "riverside");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].agent_name, "riverside");
}
