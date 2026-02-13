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
    let channels = Channel::list(base_dir, false).unwrap();
    assert!(
        !channels.iter().any(|c| c.name == "test-topic"),
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
    let active = Channel::list(base_dir, false).unwrap();
    assert!(!active.iter().any(|c| c.name == "test1"));
    assert!(!active.iter().any(|c| c.name == "test2"));
    assert!(active.iter().any(|c| c.name == "test3"));
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

#[test]
fn test_list_with_include_archived() {
    let temp_dir = TempDir::new().unwrap();

    // Create three channels: "active1", "active2", and "to-archive"
    let _channel1 = Channel::new(temp_dir.path(), "active1").unwrap();
    let _channel2 = Channel::new(temp_dir.path(), "active2").unwrap();
    let channel3 = Channel::new(temp_dir.path(), "to-archive").unwrap();

    // All three should appear when include_archived=false (they're not archived yet)
    let channels = Channel::list(temp_dir.path(), false).unwrap();
    assert_eq!(channels.len(), 3);
    assert!(channels.iter().any(|c| c.name == "active1"));
    assert!(channels.iter().any(|c| c.name == "active2"));
    assert!(channels.iter().any(|c| c.name == "to-archive"));

    // Archive one channel
    channel3.archive().unwrap();

    // With include_archived=false, should only see 2 active channels
    let channels_no_archived = Channel::list(temp_dir.path(), false).unwrap();
    assert_eq!(channels_no_archived.len(), 2);
    assert!(channels_no_archived.iter().any(|c| c.name == "active1"));
    assert!(channels_no_archived.iter().any(|c| c.name == "active2"));
    assert!(!channels_no_archived.iter().any(|c| c.name == "to-archive"));

    // With include_archived=true, should see all 3 channels (including archived one)
    let channels_with_archived = Channel::list(temp_dir.path(), true).unwrap();
    assert_eq!(channels_with_archived.len(), 3);
    assert!(
        channels_with_archived
            .iter()
            .any(|c| c.name == "active1" && !c.is_archived)
    );
    assert!(
        channels_with_archived
            .iter()
            .any(|c| c.name == "active2" && !c.is_archived)
    );
    assert!(
        channels_with_archived
            .iter()
            .any(|c| c.name == "to-archive" && c.is_archived)
    );
}

#[test]
fn test_list_archived_channels_no_ghost_files() {
    // Test that listing archived channels (with include_archived=true)
    // doesn't create ghost .jsonl files for archived channels.
    // This was a bug where Channel::list() would return "foo" for
    // "foo.archived.jsonl", then callers would use Channel::new("foo")
    // which created "foo.jsonl" even though the real file was "foo.archived.jsonl".
    let temp_dir = TempDir::new().unwrap();

    // Create and archive a channel
    let channel = Channel::new(temp_dir.path(), "archived-channel").unwrap();
    channel.archive().unwrap();

    // Verify the archived file exists
    let archived_path = temp_dir
        .path()
        .join("channels/archived-channel.archived.jsonl");
    assert!(archived_path.exists(), "Archived file should exist");

    // List channels with include_archived=true
    let channels = Channel::list(temp_dir.path(), true).unwrap();

    // Verify the archived channel appears in the list with is_archived=true
    let archived_info = channels
        .iter()
        .find(|c| c.name == "archived-channel")
        .expect("Archived channel should be in the list");
    assert!(
        archived_info.is_archived,
        "Channel should be marked as archived"
    );

    // Now try to "open" the channel using the pattern from refresh_unread_counts()
    // This simulates the corrected code path
    if archived_info.is_archived {
        // Correct: use open_archived()
        let _channel = Channel::open_archived(temp_dir.path(), &archived_info.name).unwrap();
    } else {
        // Would be buggy if we hit this path for archived channels
        let _channel = Channel::new(temp_dir.path(), &archived_info.name).unwrap();
    }

    // Verify no ghost file was created at channels/archived-channel.jsonl
    let ghost_path = temp_dir.path().join("channels/archived-channel.jsonl");
    assert!(
        !ghost_path.exists(),
        "Ghost .jsonl file should NOT be created for archived channels"
    );

    // Only the .archived.jsonl file should exist
    assert!(
        archived_path.exists(),
        "Original archived file should still exist"
    );
}
