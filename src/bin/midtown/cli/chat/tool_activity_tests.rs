use super::extract_tool_activity_headers;
use serde_json::json;

fn tool_call_item(call_id: &str, semantic_header: &str) -> serde_json::Value {
    json!({
        "content": [{
            "ToolCall": {
                "call_id": call_id,
                "name": "some_tool",
                "semantic_header": semantic_header
            }
        }]
    })
}

fn tool_result_item(call_id: &str, is_error: bool) -> serde_json::Value {
    json!({
        "content": [{
            "ToolResult": {
                "call_id": call_id,
                "is_error": is_error
            }
        }]
    })
}

#[test]
fn empty_items_returns_empty() {
    let items: Vec<serde_json::Value> = vec![];
    assert_eq!(extract_tool_activity_headers(&items), Vec::<String>::new());
}

#[test]
fn in_progress_call_gets_chevron_prefix() {
    let items = vec![tool_call_item("c1", "Read foo.rs")];
    let result = extract_tool_activity_headers(&items);
    assert_eq!(result, vec!["\u{203a} Read foo.rs"]); // › Read foo.rs
}

#[test]
fn successful_result_gets_checkmark_prefix() {
    let items = vec![
        tool_call_item("c1", "Read foo.rs"),
        tool_result_item("c1", false),
    ];
    let result = extract_tool_activity_headers(&items);
    assert_eq!(result, vec!["\u{2713} Read foo.rs"]); // ✓ Read foo.rs
}

#[test]
fn error_result_gets_cross_prefix() {
    let items = vec![
        tool_call_item("c1", "Write bar.rs"),
        tool_result_item("c1", true),
    ];
    let result = extract_tool_activity_headers(&items);
    assert_eq!(result, vec!["\u{2717} Write bar.rs"]); // ✗ Write bar.rs
}

#[test]
fn multiple_calls_with_mixed_status() {
    let items = vec![
        tool_call_item("c1", "Read foo.rs"),
        tool_result_item("c1", false),
        tool_call_item("c2", "Write bar.rs"),
        tool_result_item("c2", true),
        tool_call_item("c3", "Grep pattern"),
    ];
    let result = extract_tool_activity_headers(&items);
    assert_eq!(
        result,
        vec![
            "\u{2713} Read foo.rs",  // ✓ success
            "\u{2717} Write bar.rs", // ✗ error
            "\u{203a} Grep pattern", // › in-progress
        ]
    );
}

#[test]
fn result_before_call_still_matches() {
    // Result appears before its call in the list — should still match
    let items = vec![
        tool_result_item("c1", false),
        tool_call_item("c1", "Read foo.rs"),
    ];
    let result = extract_tool_activity_headers(&items);
    assert_eq!(result, vec!["\u{2713} Read foo.rs"]);
}

#[test]
fn falls_back_to_name_when_no_semantic_header() {
    let items = vec![json!({
        "content": [{
            "ToolCall": {
                "call_id": "c1",
                "name": "Bash"
            }
        }]
    })];
    let result = extract_tool_activity_headers(&items);
    assert_eq!(result, vec!["\u{203a} Bash"]);
}

#[test]
fn falls_back_to_question_mark_when_no_name_or_header() {
    let items = vec![json!({
        "content": [{
            "ToolCall": {
                "call_id": "c1"
            }
        }]
    })];
    let result = extract_tool_activity_headers(&items);
    assert_eq!(result, vec!["\u{203a} ?"]);
}

#[test]
fn items_without_tool_content_are_skipped() {
    let items = vec![
        json!({"content": [{"Text": "Hello"}]}),
        tool_call_item("c1", "Read foo.rs"),
        json!({"content": [{"Text": "World"}]}),
    ];
    let result = extract_tool_activity_headers(&items);
    assert_eq!(result, vec!["\u{203a} Read foo.rs"]);
}

#[test]
fn tool_result_only_items_produce_no_output() {
    let items = vec![tool_result_item("c1", false)];
    let result = extract_tool_activity_headers(&items);
    assert_eq!(result, Vec::<String>::new());
}
