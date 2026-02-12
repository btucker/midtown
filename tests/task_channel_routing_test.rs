//! Test that coworker messages are automatically routed to topic channels
//! based on their task assignment.

use tempfile::TempDir;

#[tokio::test]
async fn test_coworker_message_routes_to_task_channel() {
    // This test verifies the channel router can route messages to different topic channels.
    // The full RPC integration (CoworkerRecord → task_channel → ChannelRouter) is tested
    // via E2E tests since it requires a running daemon.

    // Setup: Create a temp directory for channel files
    let temp_dir = TempDir::new().unwrap();

    // Create a ChannelRouter
    let router = midtown::ChannelRouter::new(temp_dir.path(), "midtown");

    // Verify the router can create topic channels
    let auth_channel = router.get_channel("auth").unwrap();
    let frontend_channel = router.get_channel("frontend").unwrap();

    // Send messages to different channels
    let msg1 = midtown::Message::for_channel(
        "auth",
        "park",
        "Working on auth",
        midtown::MessageType::Text,
    );
    router.send(&msg1).unwrap();

    let msg2 = midtown::Message::for_channel(
        "frontend",
        "madison",
        "Working on UI",
        midtown::MessageType::Text,
    );
    router.send(&msg2).unwrap();

    // Verify messages went to the right channels
    let auth_messages = auth_channel.read_all().unwrap();
    assert_eq!(auth_messages.len(), 1);
    assert_eq!(auth_messages[0].from, "park");
    assert_eq!(auth_messages[0].channel_name(), "auth");

    let frontend_messages = frontend_channel.read_all().unwrap();
    assert_eq!(frontend_messages.len(), 1);
    assert_eq!(frontend_messages[0].from, "madison");
    assert_eq!(frontend_messages[0].channel_name(), "frontend");

    // Verify messages didn't leak to main channel
    let main_channel = router.default_channel().unwrap();
    let main_messages = main_channel.read_all().unwrap();
    assert_eq!(
        main_messages.len(),
        0,
        "Messages should not appear in main channel"
    );
}
