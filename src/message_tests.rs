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
