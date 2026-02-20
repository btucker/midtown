use super::*;
use crate::message::MessageType;
use crate::test_utils::retry_with_backoff;
use std::thread;
use tempfile::TempDir;

fn read_all_with_retry(channel: &Channel, max_attempts: u32) -> Result<Vec<Message>> {
    retry_with_backoff(max_attempts, || channel.read_all())
}

fn message_count_with_retry(channel: &Channel, max_attempts: u32) -> Result<usize> {
    retry_with_backoff(max_attempts, || channel.message_count())
}

fn read_last_n_with_retry(
    channel: &Channel,
    n: usize,
    max_attempts: u32,
) -> Result<(Vec<Message>, u64)> {
    retry_with_backoff(max_attempts, || channel.read_last_n_messages(n))
}

fn read_before_pos_with_retry(
    channel: &Channel,
    pos: u64,
    n: usize,
    max_attempts: u32,
) -> Result<(Vec<Message>, u64)> {
    retry_with_backoff(max_attempts, || {
        channel.read_messages_before_position(pos, n)
    })
}

fn read_since_cursor_with_retry(
    channel: &Channel,
    agent: &str,
    max_attempts: u32,
) -> Result<Vec<Message>> {
    // Use agent name as session_id since cursors are now keyed by session_id only.
    // Different agents need different session IDs to get independent cursor state.
    retry_with_backoff(max_attempts, || {
        channel.read_since_cursor(agent, &format!("test-session-{agent}"))
    })
}

#[test]
fn test_channel_creation() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();
    assert!(
        temp_dir
            .path()
            .join("channels")
            .join("midtown")
            .join("cursors")
            .exists()
    );
    // Channel file should exist (for tailf) but be empty (no messages)
    assert!(channel.exists());
    assert_eq!(message_count_with_retry(&channel, 5).unwrap(), 0);
}

#[test]
fn test_channel_name_rejects_spaces() {
    let temp_dir = TempDir::new().unwrap();
    assert!(Channel::new(temp_dir.path(), "has space").is_err());
}

#[test]
fn test_channel_name_rejects_newlines() {
    let temp_dir = TempDir::new().unwrap();
    assert!(Channel::new(temp_dir.path(), "test\nextra text").is_err());
}

#[test]
fn test_channel_name_rejects_empty() {
    let temp_dir = TempDir::new().unwrap();
    assert!(Channel::new(temp_dir.path(), "").is_err());
}

#[test]
fn test_channel_name_allows_valid_names() {
    let temp_dir = TempDir::new().unwrap();
    assert!(Channel::new(temp_dir.path(), "my-channel").is_ok());
    assert!(Channel::new(temp_dir.path(), "feature_123").is_ok());
    assert!(Channel::new(temp_dir.path(), "midtown").is_ok());
}

#[test]
fn test_channel_name_rejects_coworker_avenue_names() {
    // These names are reserved for coworker sessions; creating a channel with
    // the same name would collide with the channel lead session for that coworker.
    let temp_dir = TempDir::new().unwrap();
    for name in [
        "lexington",
        "park",
        "madison",
        "broadway",
        "amsterdam",
        "columbus",
        "riverside",
        "york",
        "pleasant",
        "vernon",
    ] {
        assert!(
            Channel::new(temp_dir.path(), name).is_err(),
            "Channel name '{}' should be rejected as a reserved coworker avenue name",
            name
        );
    }
}

#[test]
fn test_channel_file_exists_for_tailf() {
    // The channel.jsonl file must exist after Channel::new() for tailf to work.
    // tailf wraps `tail -f` which fails on non-existent files.
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // The channel file should exist (even if empty) so tailf can watch it
    assert!(
        channel.channel_file_path().exists(),
        "channel.jsonl must exist after Channel::new() for tailf compatibility"
    );
}

#[test]
fn test_send_and_read() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    let msg1 = Message::text("agent1", "Hello");
    let msg2 = Message::text("agent2", "World");

    channel.send(&msg1).unwrap();
    channel.send(&msg2).unwrap();

    // Use retry helper to handle transient lock contention in CI
    let messages = read_all_with_retry(&channel, 5).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].content, "Hello");
    assert_eq!(messages[1].content, "World");
}

#[test]
fn test_cursor_based_reading() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Send first batch
    channel.send(&Message::text("agent1", "Message 1")).unwrap();
    channel.send(&Message::text("agent1", "Message 2")).unwrap();

    // Agent reads all messages (retry to handle transient lock contention in CI)
    let messages = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();
    assert_eq!(messages.len(), 2);

    // Send more messages
    channel.send(&Message::text("agent1", "Message 3")).unwrap();

    // Agent should only see new message
    let new_messages = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();
    assert_eq!(new_messages.len(), 1);
    assert_eq!(new_messages[0].content, "Message 3");

    // Another agent sees all messages
    let all_messages = read_since_cursor_with_retry(&channel, "other_reader", 5).unwrap();
    assert_eq!(all_messages.len(), 3);
}

#[test]
fn test_cursor_reset() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    channel.send(&Message::text("agent1", "Message")).unwrap();

    // Read once (retry to handle transient lock contention in CI)
    let _ = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();

    // Reset cursor (session ID must match read_since_cursor_with_retry's format)
    channel
        .reset_cursor("reader", "test-session-reader")
        .unwrap();

    // Should see message again
    let messages = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();
    assert_eq!(messages.len(), 1);
}

#[test]
fn test_message_count() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Use retry helper to handle transient lock contention in CI
    assert_eq!(message_count_with_retry(&channel, 5).unwrap(), 0);

    channel.send(&Message::text("agent1", "1")).unwrap();
    channel.send(&Message::text("agent1", "2")).unwrap();
    channel.send(&Message::text("agent1", "3")).unwrap();

    assert_eq!(message_count_with_retry(&channel, 5).unwrap(), 3);
}

#[test]
fn test_file_size_increases_with_messages() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Empty channel has size 0
    assert_eq!(channel.file_size(), 0);

    // Sending a message increases file size
    channel.send(&Message::text("agent1", "Hello")).unwrap();
    let size1 = channel.file_size();
    assert!(size1 > 0);

    // Sending another message increases it further
    channel.send(&Message::text("agent1", "World")).unwrap();
    let size2 = channel.file_size();
    assert!(size2 > size1);

    // Size stays the same when no messages are added
    assert_eq!(channel.file_size(), size2);
}

#[test]
fn test_different_message_types() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    channel.send(&Message::text("agent1", "text")).unwrap();
    channel
        .send(&Message::system("system notification"))
        .unwrap();
    channel
        .send(&Message::command("agent1", "run test"))
        .unwrap();
    channel.send(&Message::status("agent1", "working")).unwrap();
    channel.send(&Message::error("agent1", "failed")).unwrap();

    // Use retry helper to handle transient lock contention in CI
    let messages = read_all_with_retry(&channel, 5).unwrap();
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[0].message_type, MessageType::Text);
    assert_eq!(messages[1].message_type, MessageType::System);
    assert_eq!(messages[2].message_type, MessageType::Command);
    assert_eq!(messages[3].message_type, MessageType::Status);
    assert_eq!(messages[4].message_type, MessageType::Error);
}

#[test]
fn test_empty_channel_read() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Reading from empty channel should return empty vec
    let messages = read_all_with_retry(&channel, 5).unwrap();
    assert!(messages.is_empty());

    let messages = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();
    assert!(messages.is_empty());
}

#[test]
fn test_messages_sorted_by_timestamp() {
    use chrono::{Duration, Utc};
    use std::io::Write;

    let temp_dir = TempDir::new().unwrap();

    // Create messages with out-of-order timestamps
    // Simulate: msg written at T+40min has timestamp T (old message arrived late)
    let now = Utc::now();
    let old_time = now - Duration::minutes(40);

    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Write messages directly to file in wrong order (simulating delayed write)
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(channel.channel_file_path())
        .unwrap();

    // First: write a "new" message (timestamp = now)
    let new_msg = Message {
        id: "new".to_string(),
        timestamp: now,
        from: "agent1".to_string(),
        content: "New message".to_string(),
        message_type: MessageType::Text,
        channel: None,
        source_channel: None,
        session_id: None,
        thread_parent_id: None,
    };
    writeln!(file, "{}", serde_json::to_string(&new_msg).unwrap()).unwrap();

    // Second: write an "old" message that arrived late (timestamp = 40 min ago)
    let old_msg = Message {
        id: "old".to_string(),
        timestamp: old_time,
        from: "agent2".to_string(),
        content: "Old message (delayed)".to_string(),
        message_type: MessageType::Text,
        channel: None,
        source_channel: None,
        session_id: None,
        thread_parent_id: None,
    };
    writeln!(file, "{}", serde_json::to_string(&old_msg).unwrap()).unwrap();

    drop(file);

    // Read messages - they should be sorted by timestamp (oldest first)
    let messages = read_all_with_retry(&channel, 5).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].content, "Old message (delayed)",
        "Older message should appear first (sorted by timestamp)"
    );
    assert_eq!(
        messages[1].content, "New message",
        "Newer message should appear second (sorted by timestamp)"
    );
}

#[test]
fn test_read_last_n_messages_small_channel() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Send 5 messages
    for i in 1..=5 {
        channel
            .send(&Message::text("agent1", format!("Message {}", i)))
            .unwrap();
    }

    // Request last 10 messages, but only 5 exist
    let (messages, start_pos) = read_last_n_with_retry(&channel, 10, 5).unwrap();
    assert_eq!(messages.len(), 5);
    assert_eq!(start_pos, 0, "start_pos should be 0 when all messages fit");
    assert_eq!(messages[0].content, "Message 1");
    assert_eq!(messages[4].content, "Message 5");
}

#[test]
fn test_read_last_n_messages_large_channel() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Send 50 messages
    for i in 1..=50 {
        channel
            .send(&Message::text("agent1", format!("Message {}", i)))
            .unwrap();
    }

    // Request last 10 messages
    let (messages, _start_pos) = read_last_n_with_retry(&channel, 10, 5).unwrap();
    assert_eq!(messages.len(), 10);
    // Should have messages 41-50 (last 10)
    assert_eq!(messages[0].content, "Message 41");
    assert_eq!(messages[9].content, "Message 50");
}

#[test]
fn test_read_last_n_messages_empty_channel() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    let (messages, start_pos) = read_last_n_with_retry(&channel, 10, 5).unwrap();
    assert!(messages.is_empty());
    assert_eq!(start_pos, 0);
}

#[test]
fn test_read_messages_before_position() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Send 30 messages
    for i in 1..=30 {
        channel
            .send(&Message::text("agent1", format!("Message {}", i)))
            .unwrap();
    }

    // First, get the last 10 messages to establish a position
    let (recent_msgs, start_pos) = read_last_n_with_retry(&channel, 10, 5).unwrap();
    assert_eq!(recent_msgs.len(), 10);
    assert_eq!(recent_msgs[0].content, "Message 21");

    // Now load 10 more messages before that position
    if start_pos > 0 {
        let (older_msgs, _) = read_before_pos_with_retry(&channel, start_pos, 10, 5).unwrap();
        assert!(!older_msgs.is_empty());
        // Should have some messages with lower numbers
        for msg in &older_msgs {
            // Extract number from "Message N" and verify it's < 21
            let num: i32 = msg
                .content
                .strip_prefix("Message ")
                .unwrap()
                .parse()
                .unwrap();
            assert!(num < 21, "Older messages should have numbers < 21");
        }
    }
}

#[test]
fn test_read_messages_before_position_zero() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    channel.send(&Message::text("agent1", "Message 1")).unwrap();

    // Position 0 means no more history
    let (messages, start_pos) = read_before_pos_with_retry(&channel, 0, 10, 5).unwrap();
    assert!(messages.is_empty());
    assert_eq!(start_pos, 0);
}

#[test]
fn test_rotate_empty_channel() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Rotating an empty channel should be a no-op
    let archived = channel.rotate(60).unwrap();
    assert_eq!(archived, 0);
}

#[test]
fn test_rotate_all_recent_messages() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Send messages that are all very recent (within last 60 min)
    channel.send(&Message::text("agent1", "Recent 1")).unwrap();
    channel.send(&Message::text("agent1", "Recent 2")).unwrap();

    let archived = channel.rotate(60).unwrap();
    assert_eq!(archived, 0, "No messages should be archived");

    // All messages still present
    let messages = read_all_with_retry(&channel, 5).unwrap();
    assert_eq!(messages.len(), 2);
}

#[test]
fn test_rotate_archives_old_messages() {
    use chrono::{Duration, Utc};
    use std::io::Write;

    let temp_dir = TempDir::new().unwrap();

    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Write old messages directly with timestamps > 60 min ago
    let now = Utc::now();
    let old_time = now - Duration::hours(3);

    {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(channel.channel_file_path())
            .unwrap();

        // Write 3 old messages
        for i in 1..=3 {
            let msg = Message {
                id: format!("old-{}", i),
                timestamp: old_time + Duration::minutes(i as i64),
                from: "agent1".to_string(),
                content: format!("Old message {}", i),
                message_type: MessageType::Text,
                channel: None,
                source_channel: None,
                session_id: None,
                thread_parent_id: None,
            };
            writeln!(file, "{}", serde_json::to_string(&msg).unwrap()).unwrap();
        }

        // Write 2 recent messages
        for i in 1..=2 {
            let msg = Message {
                id: format!("new-{}", i),
                timestamp: now - Duration::minutes(i as i64),
                from: "agent1".to_string(),
                content: format!("Recent message {}", i),
                message_type: MessageType::Text,
                channel: None,
                source_channel: None,
                session_id: None,
                thread_parent_id: None,
            };
            writeln!(file, "{}", serde_json::to_string(&msg).unwrap()).unwrap();
        }
    }

    // Rotate with 60 min retention
    let archived = channel.rotate(60).unwrap();
    assert_eq!(archived, 3, "3 old messages should be archived");

    // Only recent messages remain in channel
    let remaining = read_all_with_retry(&channel, 5).unwrap();
    assert_eq!(remaining.len(), 2);
    assert!(remaining[0].content.starts_with("Recent"));
    assert!(remaining[1].content.starts_with("Recent"));

    // Archive file should exist with old messages
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let archive_path = temp_dir
        .path()
        .join("channels")
        .join("midtown")
        .join("history")
        .join(format!("{}.jsonl", today));
    assert!(archive_path.exists(), "Archive file should exist");

    // Read archive and verify
    let archive_content = std::fs::read_to_string(&archive_path).unwrap();
    let archive_msgs: Vec<Message> = archive_content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(archive_msgs.len(), 3);
    assert!(archive_msgs[0].content.starts_with("Old"));
}

#[test]
fn test_rotate_resets_cursors() {
    use chrono::{Duration, Utc};
    use std::io::Write;

    let temp_dir = TempDir::new().unwrap();

    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Write old + recent messages
    let now = Utc::now();
    let old_time = now - Duration::hours(3);

    {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(channel.channel_file_path())
            .unwrap();

        let old_msg = Message {
            id: "old-1".to_string(),
            timestamp: old_time,
            from: "agent1".to_string(),
            content: "Old".to_string(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
            thread_parent_id: None,
        };
        writeln!(file, "{}", serde_json::to_string(&old_msg).unwrap()).unwrap();
    }

    // Send a recent one normally
    channel.send(&Message::text("agent1", "Recent")).unwrap();

    // Agent reads to establish a cursor at some byte offset
    let _ = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();
    let cursor_before = channel.get_cursor("reader", "test-session-reader").unwrap();
    assert!(cursor_before.position > 0, "Cursor should be past 0");

    // Rotate
    let archived = channel.rotate(60).unwrap();
    assert_eq!(archived, 1);

    // Cursor should be reset to 0
    let cursor_after = channel.get_cursor("reader", "test-session-reader").unwrap();
    assert_eq!(
        cursor_after.position, 0,
        "Cursor should be reset after rotation"
    );
}

#[test]
fn test_needs_rotation() {
    use chrono::{Duration, Utc};
    use std::io::Write;

    let temp_dir = TempDir::new().unwrap();

    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();
    let channel_file = channel.channel_file_path().to_path_buf();

    // Empty channel doesn't need rotation
    assert!(!channel.needs_rotation(24));

    // Channel with only recent messages doesn't need rotation
    channel.send(&Message::text("agent1", "Recent")).unwrap();
    assert!(!channel.needs_rotation(24));

    // Channel with old messages needs rotation
    let old_time = Utc::now() - Duration::hours(25);
    {
        // Prepend an old message by rewriting the file
        let existing = std::fs::read_to_string(&channel_file).unwrap();
        let mut file = std::fs::File::create(&channel_file).unwrap();
        let old_msg = Message {
            id: "old-1".to_string(),
            timestamp: old_time,
            from: "agent1".to_string(),
            content: "Very old".to_string(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
            thread_parent_id: None,
        };
        writeln!(file, "{}", serde_json::to_string(&old_msg).unwrap()).unwrap();
        file.write_all(existing.as_bytes()).unwrap();
    }

    // Retry: needs_rotation uses try_lock_shared internally and returns false on WouldBlock
    let mut rotation_needed = false;
    for _ in 0..5 {
        if channel.needs_rotation(24) {
            rotation_needed = true;
            break;
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        rotation_needed,
        "Channel with old messages should need rotation"
    );
}

#[test]
fn test_read_all_skips_malformed_lines() {
    // Regression test: A raw text line in channel.jsonl (not valid JSON) was causing
    // read_all() to fail completely. This happened in production when a raw message
    // was somehow written directly to the file without JSON wrapping.
    //
    // The fix should skip invalid lines and continue reading valid messages,
    // allowing the channel to be read even with some corruption.
    use std::io::Write;

    let temp_dir = TempDir::new().unwrap();

    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Write some valid messages
    channel
        .send(&Message::text("agent1", "First valid message"))
        .unwrap();
    channel
        .send(&Message::text("agent2", "Second valid message"))
        .unwrap();

    // Manually inject a malformed line (raw text, not JSON)
    // This simulates the corruption observed in production
    {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(channel.channel_file_path())
            .unwrap();
        writeln!(file, "@lead Some raw text that is not JSON").unwrap();
    }

    // Write another valid message after the corruption
    channel
        .send(&Message::text("agent3", "Third valid message"))
        .unwrap();

    // read_all() should skip the invalid line and return the 3 valid messages
    let messages = read_all_with_retry(&channel, 5).unwrap();
    assert_eq!(
        messages.len(),
        3,
        "Should skip malformed line and read 3 valid messages"
    );
    assert_eq!(messages[0].content, "First valid message");
    assert_eq!(messages[1].content, "Second valid message");
    assert_eq!(messages[2].content, "Third valid message");
}

#[test]
fn test_read_since_cursor_skips_malformed_lines() {
    // Similar to test_read_all_skips_malformed_lines but tests the cursor-based
    // reading path which uses a different code path (byte offset seeking).
    use std::io::Write;

    let temp_dir = TempDir::new().unwrap();

    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Write initial messages and read to set cursor position
    channel
        .send(&Message::text("agent1", "First message"))
        .unwrap();
    let _ = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();

    // Write more valid messages
    channel
        .send(&Message::text("agent2", "Second message"))
        .unwrap();

    // Inject a malformed line
    {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(channel.channel_file_path())
            .unwrap();
        writeln!(file, "This is raw text, not JSON").unwrap();
    }

    // Write another valid message after the corruption
    channel
        .send(&Message::text("agent3", "Third message"))
        .unwrap();

    // read_since_cursor should skip the malformed line and return the 2 new valid messages
    let messages = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();
    assert_eq!(
        messages.len(),
        2,
        "Should skip malformed line and read 2 new valid messages"
    );
    assert_eq!(messages[0].content, "Second message");
    assert_eq!(messages[1].content, "Third message");
}

#[test]
fn test_archived_channels_excluded_from_list() {
    let temp_dir = TempDir::new().unwrap();

    // Create two channels: "test1" and "test2"
    let channel1 = Channel::new(temp_dir.path(), "test1").unwrap();
    let _channel2 = Channel::new(temp_dir.path(), "test2").unwrap();

    // Both should appear in the list
    let channels = Channel::list(temp_dir.path(), false, None).unwrap();
    assert!(
        channels.iter().any(|c| c.name == "test1"),
        "test1 should be in the list"
    );
    assert!(
        channels.iter().any(|c| c.name == "test2"),
        "test2 should be in the list"
    );

    // Archive test1
    channel1.archive().unwrap();

    // Now only test2 should appear in the list
    let channels = Channel::list(temp_dir.path(), false, None).unwrap();
    assert!(
        !channels.iter().any(|c| c.name == "test1"),
        "Archived channel test1 should not be in the list"
    );
    assert!(
        channels.iter().any(|c| c.name == "test2"),
        "test2 should still be in the list"
    );

    // Verify the archived directory exists with correct name
    let archived_path = temp_dir.path().join("channels").join("test1.archived");
    assert!(
        archived_path.exists(),
        "Archived directory should exist at {:?}",
        archived_path
    );
}

#[test]
fn test_list_skips_invalid_channel_names() {
    let temp_dir = TempDir::new().unwrap();

    // Create valid channels
    Channel::new(temp_dir.path(), "valid-channel").unwrap();
    Channel::new(temp_dir.path(), "feature_123").unwrap();

    // Manually create directories with invalid names in channels/ directory
    // (these could be created by filesystem bugs, manual edits, etc.)
    // Each has a history/current.jsonl so they pass the file-existence check,
    // but their names should be rejected by is_valid_channel_name().
    let channels_dir = temp_dir.path().join("channels");
    fs::create_dir_all(&channels_dir).unwrap();
    for invalid_dir in &["test extra text", ".hidden"] {
        let hist = channels_dir.join(invalid_dir).join("history");
        fs::create_dir_all(&hist).unwrap();
        File::create(hist.join("current.jsonl")).unwrap();
    }

    // List should only return valid channel names
    let channels = Channel::list(temp_dir.path(), false, None).unwrap();
    let channel_names: Vec<&str> = channels.iter().map(|c| c.name.as_str()).collect();

    assert!(
        channel_names.contains(&"valid-channel"),
        "valid-channel should be in the list"
    );
    assert!(
        channel_names.contains(&"feature_123"),
        "feature_123 should be in the list"
    );
    assert!(
        !channel_names.contains(&"test extra text"),
        "Channel with spaces should be filtered out"
    );
    assert!(
        !channel_names.contains(&".hidden"),
        "Channel with leading dot should be filtered out"
    );
}

#[test]
fn test_both_active_and_archived_files_treats_channel_as_active() {
    let temp_dir = TempDir::new().unwrap();

    // Create a channel, then archive it
    let channel = Channel::new(temp_dir.path(), "tui").unwrap();
    channel.archive().unwrap();

    // Verify it's archived
    let channels = Channel::list(temp_dir.path(), true, None).unwrap();
    let tui = channels.iter().find(|c| c.name == "tui").unwrap();
    assert!(tui.is_archived, "Should be archived after archive()");

    // Now also create an active file (simulating a channel that was
    // re-created while the archived file still exists)
    Channel::new(temp_dir.path(), "tui").unwrap();

    // Both directories should exist
    let channels_dir = temp_dir.path().join("channels");
    assert!(
        channels_dir
            .join("tui")
            .join("history")
            .join("current.jsonl")
            .exists()
    );
    assert!(
        channels_dir
            .join("tui.archived")
            .join("history")
            .join("current.jsonl")
            .exists()
    );

    // Channel::list should treat it as active (not archived)
    let channels = Channel::list(temp_dir.path(), true, None).unwrap();
    let tui_entries: Vec<_> = channels.iter().filter(|c| c.name == "tui").collect();
    assert_eq!(
        tui_entries.len(),
        1,
        "Should have exactly one entry for 'tui'"
    );
    assert!(
        !tui_entries[0].is_archived,
        "Channel with both active and archived files should be treated as active"
    );

    // Also verify without include_archived — the channel should still appear
    let channels = Channel::list(temp_dir.path(), false, None).unwrap();
    assert!(
        channels.iter().any(|c| c.name == "tui"),
        "Active channel with stale archived file should appear in non-archived list"
    );
}

#[test]
fn test_midtown_channel_not_duplicated_with_legacy_and_channels_dir() {
    let temp_dir = TempDir::new().unwrap();

    // Create the midtown channel (directory layout)
    Channel::new(temp_dir.path(), "midtown").unwrap();

    // Channel::list should return exactly one "midtown" entry
    let channels = Channel::list(temp_dir.path(), false, None).unwrap();
    let midtown_entries: Vec<_> = channels.iter().filter(|c| c.name == "midtown").collect();
    assert_eq!(
        midtown_entries.len(),
        1,
        "Should have exactly one 'midtown' entry"
    );
}

#[test]
fn test_channel_router_basic_routing() {
    let temp_dir = TempDir::new().unwrap();
    let router = ChannelRouter::new(temp_dir.path(), "midtown");

    // Send to default channel (message with no channel field set)
    let msg1 = Message::text("agent1", "Hello main channel");
    router.send(&msg1).unwrap();

    // Send to a topic channel
    let msg2 = Message::for_channel("pr-42", "agent2", "Review feedback", MessageType::Text);
    router.send(&msg2).unwrap();

    // Verify messages went to the right channels
    let main_channel = router.default_channel().unwrap();
    let messages = read_all_with_retry(&main_channel, 5).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "Hello main channel");

    let pr_channel = router.get_channel("pr-42").unwrap();
    let pr_messages = read_all_with_retry(&pr_channel, 5).unwrap();
    assert_eq!(pr_messages.len(), 1);
    assert_eq!(pr_messages[0].content, "Review feedback");
}

#[test]
fn test_channel_router_lazy_opening() {
    let temp_dir = TempDir::new().unwrap();
    let router = ChannelRouter::new(temp_dir.path(), "midtown");

    // Initially no channels open
    assert_eq!(router.open_channels().len(), 0);

    // Send to a channel - it gets opened
    let msg1 = Message::for_channel("task-5", "agent1", "Working on it", MessageType::Status);
    router.send(&msg1).unwrap();
    assert_eq!(router.open_channels().len(), 1);
    assert!(router.open_channels().contains(&"task-5".to_string()));

    // Send to another channel
    let msg2 = Message::for_channel("pr-10", "agent2", "Reviewing", MessageType::Status);
    router.send(&msg2).unwrap();
    assert_eq!(router.open_channels().len(), 2);

    // Send to existing channel - doesn't increase count
    let msg3 = Message::for_channel("task-5", "agent1", "Still working", MessageType::Status);
    router.send(&msg3).unwrap();
    assert_eq!(router.open_channels().len(), 2);
}

#[test]
fn test_channel_router_default_channel() {
    let temp_dir = TempDir::new().unwrap();
    let router = ChannelRouter::new(temp_dir.path(), "my-repo");

    // Get default channel
    let default = router.default_channel().unwrap();
    assert_eq!(default.channel_name(), "my-repo");

    // Message::text() creates a message with channel: None
    let msg = Message::text("agent1", "Test");
    // channel_name() returns "midtown" as a fallback when channel field is None
    assert_eq!(msg.channel_name(), "midtown");
    // But the channel field itself is None
    assert!(msg.channel.is_none());

    // Router uses its default "my-repo" when message.channel is None
    router.send(&msg).unwrap();

    // Message should be in "my-repo" channel (router's default is used)
    let my_repo_ch = router.get_channel("my-repo").unwrap();
    let messages = read_all_with_retry(&my_repo_ch, 5).unwrap();
    assert_eq!(messages.len(), 1);
}

#[test]
fn test_channel_router_concurrent_access() {
    use std::sync::Arc;
    use std::thread;

    let temp_dir = TempDir::new().unwrap();
    let router = Arc::new(ChannelRouter::new(temp_dir.path(), "midtown"));

    // Spawn multiple threads sending to different channels
    let mut handles = vec![];
    for i in 0..10 {
        let router_clone = Arc::clone(&router);
        let handle = thread::spawn(move || {
            let channel_name = format!("task-{}", i % 3); // 3 different channels
            let msg = Message::for_channel(
                channel_name,
                format!("agent{}", i),
                format!("Message {}", i),
                MessageType::Text,
            );
            router_clone.send(&msg).unwrap();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // All threads should have succeeded
    // We should have 3 channels open (task-0, task-1, task-2)
    assert_eq!(router.open_channels().len(), 3);
}

#[test]
fn test_channel_router_clone() {
    let temp_dir = TempDir::new().unwrap();
    let router = ChannelRouter::new(temp_dir.path(), "midtown");

    // Send a message to open a channel
    let msg = Message::for_channel("test-channel", "agent1", "Test", MessageType::Text);
    router.send(&msg).unwrap();

    // Clone the router
    let router2 = router.clone();

    // Clone should have access to the same cached channel
    assert_eq!(router2.open_channels().len(), 1);
    assert!(
        router2
            .open_channels()
            .contains(&"test-channel".to_string())
    );

    // Both routers can send to the channel
    router
        .send(&Message::for_channel(
            "test-channel",
            "agent2",
            "Msg2",
            MessageType::Text,
        ))
        .unwrap();
    router2
        .send(&Message::for_channel(
            "test-channel",
            "agent3",
            "Msg3",
            MessageType::Text,
        ))
        .unwrap();

    let channel = router.get_channel("test-channel").unwrap();
    let messages = read_all_with_retry(&channel, 5).unwrap();
    assert_eq!(messages.len(), 3);
}

#[test]
fn test_channel_router_insight_message_routing() {
    let temp_dir = TempDir::new().unwrap();
    let router = ChannelRouter::new(temp_dir.path(), "midtown");

    // Send an insight to a topic channel
    let insight_msg = Message::for_channel(
        "auth-refactor",
        "park",
        "💡 The tower::Layer stack composes auth providers independently",
        MessageType::Text,
    );
    router.send(&insight_msg).unwrap();

    // Send a non-insight message to the same topic channel
    let regular_msg = Message::for_channel(
        "auth-refactor",
        "park",
        "Working on the auth module",
        MessageType::Text,
    );
    router.send(&regular_msg).unwrap();

    // Verify the topic channel has both messages
    let topic_channel = router.get_channel("auth-refactor").unwrap();
    let topic_messages = read_all_with_retry(&topic_channel, 5).unwrap();
    assert_eq!(topic_messages.len(), 2);
    assert!(topic_messages[0].content.contains("💡"));
    assert!(!topic_messages[1].content.contains("💡"));

    // Note: This test only validates that the router correctly sends messages to topic channels.
    // The cross-posting behavior (where insights are also sent to main channel) is handled
    // by the daemon's send_and_broadcast_async() method, which is tested in daemon/mod.rs.
}

// --- Thread integration tests ---

#[test]
fn test_thread_reply_round_trip() {
    // Full cycle: create a top-level message, create a thread reply referencing it,
    // serialize both to JSONL, read back and verify thread_parent_id is preserved.
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Post the parent message
    let parent = Message::text("agent1", "Starting a topic");
    let parent_id = parent.id.clone();
    channel.send(&parent).unwrap();

    // Post a thread reply
    let reply = Message::thread_reply(
        "midtown",
        "agent2",
        "Reply in thread",
        &parent_id,
        MessageType::Text,
    );
    channel.send(&reply).unwrap();

    // Read back all messages and verify thread_parent_id is preserved
    let messages = read_all_with_retry(&channel, 5).unwrap();
    assert_eq!(messages.len(), 2);

    let read_parent = &messages[0];
    assert_eq!(read_parent.id, parent_id);
    assert_eq!(read_parent.thread_parent_id, None);

    let read_reply = &messages[1];
    assert_eq!(read_reply.thread_parent_id, Some(parent_id.clone()));
    assert_eq!(read_reply.from, "agent2");
    assert_eq!(read_reply.content, "Reply in thread");
}

#[test]
fn test_thread_filter_top_level_only() {
    // Verify that filtering out messages with thread_parent_id leaves only top-level messages.
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    let parent = Message::text("agent1", "Top-level message");
    let parent_id = parent.id.clone();
    channel.send(&parent).unwrap();

    let reply = Message::thread_reply(
        "midtown",
        "agent2",
        "Thread reply",
        &parent_id,
        MessageType::Text,
    );
    channel.send(&reply).unwrap();

    let another_top = Message::text("agent3", "Another top-level");
    channel.send(&another_top).unwrap();

    let all = read_all_with_retry(&channel, 5).unwrap();
    assert_eq!(all.len(), 3);

    // Filter: top-level only (no thread_parent_id)
    let top_level: Vec<_> = all
        .iter()
        .filter(|m| m.thread_parent_id.is_none())
        .collect();
    assert_eq!(top_level.len(), 2);
    assert_eq!(top_level[0].content, "Top-level message");
    assert_eq!(top_level[1].content, "Another top-level");
}

#[test]
fn test_thread_filter_replies_for_parent() {
    // Verify that filtering by thread_parent_id returns only replies for that thread.
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    let parent = Message::text("agent1", "Parent message");
    let parent_id = parent.id.clone();
    channel.send(&parent).unwrap();

    let other_parent = Message::text("agent2", "Another parent");
    let other_id = other_parent.id.clone();
    channel.send(&other_parent).unwrap();

    let reply1 = Message::thread_reply(
        "midtown",
        "agent3",
        "Reply to parent",
        &parent_id,
        MessageType::Text,
    );
    channel.send(&reply1).unwrap();

    let reply2 = Message::thread_reply(
        "midtown",
        "agent4",
        "Another reply to parent",
        &parent_id,
        MessageType::Text,
    );
    channel.send(&reply2).unwrap();

    let other_reply = Message::thread_reply(
        "midtown",
        "agent5",
        "Reply to other parent",
        &other_id,
        MessageType::Text,
    );
    channel.send(&other_reply).unwrap();

    let all = read_all_with_retry(&channel, 5).unwrap();
    assert_eq!(all.len(), 5);

    // Filter: replies for parent_id only
    let thread_replies: Vec<_> = all
        .iter()
        .filter(|m| m.thread_parent_id.as_deref() == Some(&parent_id))
        .collect();
    assert_eq!(thread_replies.len(), 2);
    assert_eq!(thread_replies[0].content, "Reply to parent");
    assert_eq!(thread_replies[1].content, "Another reply to parent");
}

#[test]
fn test_thread_backward_compat_old_jsonl() {
    // Verify that existing channel JSONL without thread_parent_id still parses correctly.
    let temp_dir = TempDir::new().unwrap();
    // Channel::new("midtown") uses channels/midtown.jsonl (not the legacy channel.jsonl path)
    let channels_dir = temp_dir.path().join("channels");
    std::fs::create_dir_all(&channels_dir).unwrap();
    let channel_path = channels_dir.join("midtown.jsonl");

    // Write an old-format JSONL line without thread_parent_id
    let old_line = r#"{"id":"old-msg-id","timestamp":"2026-01-01T00:00:00Z","from":"agent1","content":"Hello","type":"text"}"#;
    std::fs::write(&channel_path, format!("{}\n", old_line)).unwrap();

    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();
    let messages = read_all_with_retry(&channel, 5).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, "old-msg-id");
    assert_eq!(messages[0].from, "agent1");
    assert_eq!(messages[0].content, "Hello");
    assert_eq!(messages[0].thread_parent_id, None); // defaults to None
}

#[test]
fn test_thread_reply_serialized_to_jsonl() {
    // Verify that thread_parent_id is present in the JSONL output for replies
    // and absent for top-level messages (skip_serializing_if = None).
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    let parent = Message::text("agent1", "Parent");
    let parent_id = parent.id.clone();
    channel.send(&parent).unwrap();

    let reply = Message::thread_reply("midtown", "agent2", "Reply", &parent_id, MessageType::Text);
    channel.send(&reply).unwrap();

    // Read raw JSONL file and verify field presence
    let content = std::fs::read_to_string(channel.channel_file_path()).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);

    // Top-level message should NOT have thread_parent_id in JSONL
    assert!(!lines[0].contains("thread_parent_id"));

    // Reply should have thread_parent_id in JSONL
    assert!(lines[1].contains("thread_parent_id"));
    assert!(lines[1].contains(&parent_id));
}

#[test]
fn test_notes_dir_returns_correct_path() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();

    let notes = channel.notes_dir();
    assert_eq!(
        notes,
        temp_dir
            .path()
            .join("channels")
            .join("midtown")
            .join("notes")
    );
    // notes/ directory is created by Channel::new()
    assert!(
        notes.exists(),
        "notes/ directory should be created by Channel::new()"
    );
}

#[test]
fn test_rotate_writes_archive_to_history_dir() {
    use chrono::{Duration, Utc};
    use std::io::Write;

    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "midtown").unwrap();
    let now = Utc::now();

    // Write one old message directly so rotation has something to archive
    {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(channel.channel_file_path())
            .unwrap();
        let old_msg = Message {
            id: "old-1".to_string(),
            timestamp: now - Duration::hours(3),
            from: "agent1".to_string(),
            content: "Old".to_string(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
            thread_parent_id: None,
        };
        writeln!(file, "{}", serde_json::to_string(&old_msg).unwrap()).unwrap();
    }

    let archived = channel.rotate(60).unwrap();
    assert_eq!(archived, 1);

    // Archive should be written to channels/midtown/history/<date>.jsonl
    let today = now.format("%Y-%m-%d").to_string();
    let archive_path = temp_dir
        .path()
        .join("channels")
        .join("midtown")
        .join("history")
        .join(format!("{}.jsonl", today));
    assert!(
        archive_path.exists(),
        "Archive should be in history/ dir: {:?}",
        archive_path
    );

    // Active file should still exist (for tailf compatibility)
    assert!(channel.channel_file_path().exists());
}

#[test]
fn test_migration_legacy_channel_jsonl() {
    let temp_dir = TempDir::new().unwrap();

    // Write a message to the legacy channel.jsonl
    let legacy = temp_dir.path().join("channel.jsonl");
    std::fs::write(&legacy, "{\"id\":\"1\",\"content\":\"hi\"}\n").unwrap();

    // Channel::new triggers migration
    let _ = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Legacy file should be gone (renamed to new location)
    assert!(
        !legacy.exists(),
        "Legacy channel.jsonl should be migrated away"
    );

    // New location should contain the content
    let new_path = temp_dir
        .path()
        .join("channels")
        .join("midtown")
        .join("history")
        .join("current.jsonl");
    assert!(new_path.exists(), "Migrated file should exist at new path");
    let content = std::fs::read_to_string(&new_path).unwrap();
    assert!(
        content.contains("hi"),
        "Migrated content should be preserved"
    );
}

#[test]
fn test_migration_flat_channel_jsonl_in_channels_dir() {
    let temp_dir = TempDir::new().unwrap();

    // Write a message to the flat channels/features.jsonl (old layout)
    let channels_dir = temp_dir.path().join("channels");
    fs::create_dir_all(&channels_dir).unwrap();
    let flat_file = channels_dir.join("features.jsonl");
    std::fs::write(&flat_file, "{\"id\":\"2\",\"content\":\"feature msg\"}\n").unwrap();

    // Channel::new("midtown") triggers migration of all flat files
    let _ = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Flat file should be gone
    assert!(
        !flat_file.exists(),
        "Flat channels/features.jsonl should be migrated away"
    );

    // New location should exist
    let new_path = channels_dir
        .join("features")
        .join("history")
        .join("current.jsonl");
    assert!(
        new_path.exists(),
        "Migrated features channel should exist at new path"
    );
    let content = std::fs::read_to_string(&new_path).unwrap();
    assert!(
        content.contains("feature msg"),
        "Migrated content should be preserved"
    );
}

#[test]
fn test_migration_archived_jsonl() {
    let temp_dir = TempDir::new().unwrap();

    // Write a message to the old-style archived file
    let channels_dir = temp_dir.path().join("channels");
    fs::create_dir_all(&channels_dir).unwrap();
    let archived_flat = channels_dir.join("old-feature.archived.jsonl");
    std::fs::write(
        &archived_flat,
        "{\"id\":\"3\",\"content\":\"archived msg\"}\n",
    )
    .unwrap();

    // Migration triggered by Channel::new
    let _ = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Old file should be gone
    assert!(
        !archived_flat.exists(),
        "Old .archived.jsonl should be migrated away"
    );

    // New directory layout should exist
    let new_path = channels_dir
        .join("old-feature.archived")
        .join("history")
        .join("current.jsonl");
    assert!(
        new_path.exists(),
        "Migrated archived channel should exist at new path"
    );
    let content = std::fs::read_to_string(&new_path).unwrap();
    assert!(
        content.contains("archived msg"),
        "Archived content should be preserved"
    );
}

#[test]
fn test_migration_both_legacy_and_flat_appends_orphan() {
    // Regression test: If both channel.jsonl (V1) and channels/midtown.jsonl (V2)
    // exist, step 1 migrates V1 first, then step 2 must append V2's content
    // instead of silently abandoning it.
    let temp_dir = TempDir::new().unwrap();

    // Create V1 legacy file: channel.jsonl
    let legacy = temp_dir.path().join("channel.jsonl");
    std::fs::write(&legacy, "{\"id\":\"v1\",\"content\":\"from legacy\"}\n").unwrap();

    // Create V2 flat file: channels/midtown.jsonl
    let channels_dir = temp_dir.path().join("channels");
    fs::create_dir_all(&channels_dir).unwrap();
    let flat_file = channels_dir.join("midtown.jsonl");
    std::fs::write(&flat_file, "{\"id\":\"v2\",\"content\":\"from flat\"}\n").unwrap();

    // Migration triggered by Channel::new
    let _ = Channel::new(temp_dir.path(), "midtown").unwrap();

    // Both source files should be gone
    assert!(!legacy.exists(), "Legacy channel.jsonl should be migrated");
    assert!(
        !flat_file.exists(),
        "Flat channels/midtown.jsonl should be migrated"
    );

    // Migrated file should contain content from BOTH sources
    let new_path = channels_dir
        .join("midtown")
        .join("history")
        .join("current.jsonl");
    let content = std::fs::read_to_string(&new_path).unwrap();
    assert!(
        content.contains("from legacy"),
        "Content from V1 legacy file should be preserved"
    );
    assert!(
        content.contains("from flat"),
        "Content from V2 flat file should be preserved (not orphaned)"
    );
}
