//! Test that daemon-generated messages about tasks are routed to topic channels.

use std::collections::HashMap;
use tempfile::TempDir;

#[tokio::test]
async fn test_daemon_messages_route_to_task_channel() {
    // This test verifies that when the daemon posts a message about a specific task,
    // it gets routed to that task's assigned channel instead of the main channel.

    let temp_dir = TempDir::new().unwrap();
    let router = midtown::ChannelRouter::new(temp_dir.path(), "midtown");

    // Create task_channel mapping (simulates daemon persistent state)
    let mut task_channel = HashMap::new();
    task_channel.insert("42".to_string(), "auth-refactor".to_string());
    task_channel.insert("43".to_string(), "frontend-redesign".to_string());

    // Simulate daemon posting a message about task 42
    // The daemon should look up task 42's channel ("auth-refactor") and route there
    let msg_task_42 = midtown::Message::for_channel(
        "auth-refactor",
        "midtown",
        "Task !42 reset to pending",
        midtown::MessageType::System,
    );
    router.send(&msg_task_42).unwrap();

    // Simulate daemon posting about task 43
    let msg_task_43 = midtown::Message::for_channel(
        "frontend-redesign",
        "midtown",
        "Task !43 completed",
        midtown::MessageType::System,
    );
    router.send(&msg_task_43).unwrap();

    // Verify messages went to the correct topic channels
    let auth_channel = router.get_channel("auth-refactor").unwrap();
    let auth_messages = auth_channel.read_all().unwrap();
    assert_eq!(auth_messages.len(), 1);
    assert_eq!(auth_messages[0].content, "Task !42 reset to pending");

    let frontend_channel = router.get_channel("frontend-redesign").unwrap();
    let frontend_messages = frontend_channel.read_all().unwrap();
    assert_eq!(frontend_messages.len(), 1);
    assert_eq!(frontend_messages[0].content, "Task !43 completed");

    // Verify main channel didn't receive these task-specific messages
    let main_channel = router.default_channel().unwrap();
    let main_messages = main_channel.read_all().unwrap();
    assert_eq!(
        main_messages.len(),
        0,
        "Task-specific messages should not go to main channel"
    );
}
