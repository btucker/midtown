use std::collections::HashMap;

use midtown::{Message, MessageType};

use super::*;

fn test_message(content: &str) -> Message {
    Message {
        id: "1".to_string(),
        from: "park".to_string(),
        content: content.to_string(),
        timestamp: chrono::Utc::now(),
        message_type: MessageType::Text,
        channel: None,
        session_id: None,
        thread_parent_id: None,
        auto_output: false,
        nudge_type: None,
        tool_data: None,
        provider: None,
        tool_use_id: None,
    }
}

#[test]
fn test_render_message_tool_data_single_tool() {
    // When content is empty and tool_data has blocks, render_message should
    // generate a "[ToolName]" summary.
    let mut msg = test_message("");
    msg.tool_data = Some(vec![midtown::ToolBlock {
        tool_name: "Bash".to_string(),
        input: serde_json::json!({"command": "ls"}),
        output: None,
        error: false,
        call_id: None,
        parent_tool_use_id: None,
    }]);

    let tasks = HashMap::new();
    let lines = render_message(&msg, 80, None, &tasks, None, &[]);

    let all_text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        all_text.contains("[Bash]"),
        "Should render tool summary '[Bash]', got: {}",
        all_text
    );
}

#[test]
fn test_render_message_tool_data_multiple_tools() {
    // When content is empty and tool_data has multiple blocks, render_message
    // should generate a "[Bash, Read]" summary.
    let mut msg = test_message("");
    msg.tool_data = Some(vec![
        midtown::ToolBlock {
            tool_name: "Bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
            output: None,
            error: false,
            call_id: None,
            parent_tool_use_id: None,
        },
        midtown::ToolBlock {
            tool_name: "Read".to_string(),
            input: serde_json::json!({"file_path": "/tmp/foo"}),
            output: None,
            error: false,
            call_id: None,
            parent_tool_use_id: None,
        },
    ]);

    let tasks = HashMap::new();
    let lines = render_message(&msg, 80, None, &tasks, None, &[]);

    let all_text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        all_text.contains("[Bash, Read]"),
        "Should render tool summary '[Bash, Read]', got: {}",
        all_text
    );
}
