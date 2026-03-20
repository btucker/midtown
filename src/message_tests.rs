use super::*;

#[test]
fn test_wire_name_matches_serde() {
    // Ensure wire_name() produces the same strings as serde's snake_case rename.
    let variants = [
        (MessageType::Text, "text"),
        (MessageType::System, "system"),
        (MessageType::Command, "command"),
        (MessageType::Status, "status"),
        (MessageType::Error, "error"),
        (MessageType::Action, "action"),
        (MessageType::Insight, "insight"),
        (MessageType::Nudge, "nudge"),
    ];
    for (variant, expected) in &variants {
        assert_eq!(
            variant.wire_name(),
            *expected,
            "wire_name mismatch for {:?}",
            variant
        );
        // Also verify it matches serde serialization
        let serde_str = serde_json::to_value(variant).unwrap();
        assert_eq!(
            serde_str.as_str().unwrap(),
            *expected,
            "serde mismatch for {:?}",
            variant
        );
    }
}

#[test]
fn test_nudge_type_serialization() {
    let mut msg = Message::new("midtown", "Task assigned", MessageType::Nudge);
    msg.nudge_type = Some("task_assigned".to_string());
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"nudge\""), "type should be nudge");
    assert!(
        json.contains("\"nudge_type\":\"task_assigned\""),
        "nudge_type should be serialized"
    );
    let parsed: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.message_type, MessageType::Nudge);
    assert_eq!(parsed.nudge_type, Some("task_assigned".to_string()));
}

#[test]
fn test_nudge_type_omitted_when_none() {
    let msg = Message::text("agent1", "Regular message");
    let json = serde_json::to_string(&msg).unwrap();
    assert!(
        !json.contains("nudge_type"),
        "nudge_type should be omitted when None"
    );
}

#[test]
fn test_nudge_type_backward_compat_deserialize() {
    // Old messages without nudge_type should deserialize with None
    let old_json = r#"{
        "id": "test-id",
        "timestamp": "2026-01-01T00:00:00Z",
        "from": "midtown",
        "content": "Task assigned",
        "type": "text"
    }"#;
    let msg: Message = serde_json::from_str(old_json).unwrap();
    assert_eq!(msg.nudge_type, None);
}

#[test]
fn test_channel_name_fallback_is_not_hardcoded_midtown() {
    // Regression: channel_name() used to return "midtown" when channel was None,
    // which silently worked for the "midtown" project but broke all others.
    // It should now return "unknown" to make missing channels obvious.
    let msg = Message::new("agent1", "test", MessageType::Text);
    assert!(msg.channel.is_none());
    assert_eq!(msg.channel_name(), "unknown");
    assert_ne!(msg.channel_name(), "midtown");
}

#[test]
fn test_for_channel_sets_channel_explicitly() {
    // Verify that for_channel() always produces a message with an explicit channel,
    // avoiding the fallback path entirely.
    let msg = Message::for_channel("my-project", "agent1", "test", MessageType::Text);
    assert_eq!(msg.channel, Some("my-project".to_string()));
    assert_eq!(msg.channel_name(), "my-project");
}

#[test]
fn test_to_json_includes_core_fields() {
    let msg = Message::for_channel("test-channel", "alice", "Hello world", MessageType::Text);
    let json = msg.to_json();
    assert_eq!(json["id"].as_str().unwrap(), msg.id);
    assert_eq!(json["from"], "alice");
    assert_eq!(json["message"], "Hello world");
    assert_eq!(json["msg_type"], "text");
    assert_eq!(json["channel"], "test-channel");
    // thread_parent_id should be absent for top-level messages
    assert!(json.get("thread_parent_id").is_none());
}

#[test]
fn test_to_json_includes_optional_fields() {
    let mut msg = Message::thread_reply(
        "test-channel",
        "bob",
        "Reply",
        "parent-123",
        MessageType::Text,
    );
    msg.nudge_type = Some("mention".to_string());
    msg.provider = Some("claude".to_string());
    msg.tool_use_id = Some("tool-456".to_string());
    msg.auto_output = true;

    let json = msg.to_json();
    assert_eq!(json["thread_parent_id"], "parent-123");
    assert_eq!(json["nudge_type"], "mention");
    assert_eq!(json["provider"], "claude");
    assert_eq!(json["tool_use_id"], "tool-456");
    assert_eq!(json["auto_output"], true);
}

#[test]
fn test_to_json_omits_false_auto_output() {
    let msg = Message::text("alice", "Regular");
    let json = msg.to_json();
    assert!(
        json.get("auto_output").is_none(),
        "auto_output should be omitted when false"
    );
}

#[test]
fn test_compute_reply_meta_basic() {
    let parent = Message::for_channel("ch", "alice", "Thread start", MessageType::Text);
    let parent_id = parent.id.clone();

    let reply1 = Message::thread_reply("ch", "bob", "Reply 1", &parent_id, MessageType::Text);
    let reply2 = Message::thread_reply("ch", "carol", "Reply 2", &parent_id, MessageType::Text);
    let reply3 = Message::thread_reply("ch", "bob", "Reply 3", &parent_id, MessageType::Text);

    let meta = compute_reply_meta(&[parent, reply1, reply2, reply3]);
    let rm = meta.get(&parent_id).expect("should have reply meta");
    assert_eq!(rm.count, 3);
    assert_eq!(rm.last_from, "bob");
    assert_eq!(rm.participants.len(), 2);
    assert!(rm.participants.contains(&"bob".to_string()));
    assert!(rm.participants.contains(&"carol".to_string()));
}

#[test]
fn test_compute_reply_meta_excludes_tool_only() {
    let parent = Message::for_channel("ch", "alice", "Start", MessageType::Text);
    let parent_id = parent.id.clone();

    let text_reply =
        Message::thread_reply("ch", "bob", "Visible reply", &parent_id, MessageType::Text);
    let mut tool_only = Message::thread_reply("ch", "bob", "", &parent_id, MessageType::Text);
    tool_only.tool_data = Some(vec![ToolBlock {
        tool_name: "Bash".to_string(),
        input: serde_json::json!({"command": "ls"}),
        output: None,
        error: false,
        call_id: None,
        parent_tool_use_id: None,
    }]);

    let meta = compute_reply_meta(&[parent, text_reply, tool_only]);
    let rm = meta.get(&parent_id).expect("should have reply meta");
    assert_eq!(rm.count, 1, "tool-only reply should be excluded");
}
