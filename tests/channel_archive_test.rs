//! Integration test for channel archiving when all tasks complete.

use midtown::{Channel, Message};
use tempfile::TempDir;

#[test]
fn test_archive_channel_renames_file_correctly() {
    // Setup: Create a temporary directory for the channel
    let temp_dir = TempDir::new().unwrap();
    let base_dir = temp_dir.path();

    // Create a topic channel with some messages
    let channel = Channel::new(base_dir, "test-topic").unwrap();
    let msg = Message::text("test", "Hello world");
    channel.send(&msg).unwrap();

    // Verify the channel file exists
    let channel_file = base_dir.join("channels").join("test-topic.jsonl");
    assert!(channel_file.exists(), "Channel file should exist");

    // Archive the channel
    channel.archive().unwrap();

    // Verify the channel file was renamed
    assert!(
        !channel_file.exists(),
        "Original channel file should be gone"
    );
    let archived_file = base_dir.join("channels").join("test-topic.archived.jsonl");
    assert!(
        archived_file.exists(),
        "Archived channel file should exist at {:?}",
        archived_file
    );

    // Verify archived channels are not listed
    let channels = Channel::list(base_dir).unwrap();
    assert!(
        !channels.contains(&"test-topic".to_string()),
        "Archived channel should not appear in list"
    );
}

#[test]
fn test_archive_channel_preserves_messages() {
    // Setup: Create a temporary directory for the channel
    let temp_dir = TempDir::new().unwrap();
    let base_dir = temp_dir.path();

    // Create a topic channel with multiple messages
    let channel = Channel::new(base_dir, "test-topic").unwrap();
    let msg1 = Message::text("user1", "First message");
    let msg2 = Message::text("user2", "Second message");
    channel.send(&msg1).unwrap();
    channel.send(&msg2).unwrap();

    // Archive the channel
    channel.archive().unwrap();

    // Read the archived file directly to verify messages are preserved
    let archived_file = base_dir.join("channels").join("test-topic.archived.jsonl");
    let content = std::fs::read_to_string(archived_file).unwrap();
    assert!(
        content.contains("First message"),
        "Archived channel should preserve first message"
    );
    assert!(
        content.contains("Second message"),
        "Archived channel should preserve second message"
    );
}

#[test]
fn test_cannot_archive_midtown_channel() {
    // Setup: Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let base_dir = temp_dir.path();

    // Try to archive the midtown channel
    let channel = Channel::new(base_dir, "midtown").unwrap();
    let result = channel.archive();

    // Should fail with an error
    assert!(result.is_err(), "Archiving the midtown channel should fail");
}

// Note: test_auto_archive_effects_integration removed because daemon::auto_archive
// and daemon::effects are private modules. The functionality is tested via unit tests
// in src/daemon/auto_archive.rs

#[test]
fn test_list_archived_channels() {
    // Setup: Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let base_dir = temp_dir.path();

    // Create and archive two channels
    let channel1 = Channel::new(base_dir, "test1").unwrap();
    channel1.send(&Message::text("agent", "msg1")).unwrap();
    channel1.archive().unwrap();

    let channel2 = Channel::new(base_dir, "test2").unwrap();
    channel2.send(&Message::text("agent", "msg2")).unwrap();
    channel2.archive().unwrap();

    // Create one non-archived channel
    let _channel3 = Channel::new(base_dir, "test3").unwrap();

    // List archived channels
    let archived = Channel::list_archived(base_dir).unwrap();
    assert_eq!(archived.len(), 2, "Should find 2 archived channels");
    assert!(archived.contains(&"test1".to_string()));
    assert!(archived.contains(&"test2".to_string()));
    assert!(!archived.contains(&"test3".to_string()));

    // Verify regular list() still excludes archived channels
    let active = Channel::list(base_dir).unwrap();
    assert!(!active.contains(&"test1".to_string()));
    assert!(!active.contains(&"test2".to_string()));
    assert!(active.contains(&"test3".to_string()));
}

#[test]
fn test_open_archived_channel_for_reading() {
    // Setup: Create and archive a channel
    let temp_dir = TempDir::new().unwrap();
    let base_dir = temp_dir.path();

    let channel = Channel::new(base_dir, "feature-complete").unwrap();
    channel
        .send(&Message::text("park", "Completed feature"))
        .unwrap();
    channel
        .send(&Message::text("madison", "Tests passing"))
        .unwrap();
    channel.archive().unwrap();

    // Should be able to open and read the archived channel
    let archived = Channel::open_archived(base_dir, "feature-complete").unwrap();
    let messages = archived.read_all().unwrap();

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].content, "Completed feature");
    assert_eq!(messages[1].content, "Tests passing");
}
