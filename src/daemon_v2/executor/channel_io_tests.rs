use super::*;
use tempfile::TempDir;

#[test]
fn post_and_read_message() {
    let dir = TempDir::new().unwrap();
    post_message(dir.path(), "test-chan", "alice", "hello world", None).unwrap();

    let msgs = read_messages(dir.path(), "test-chan", None).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["from"], "alice");
    assert_eq!(msgs[0]["message"], "hello world");
}

#[test]
fn post_system_message_uses_midtown_sender() {
    let dir = TempDir::new().unwrap();
    post_system_message(dir.path(), "sys-chan", "daemon started").unwrap();

    let msgs = read_messages(dir.path(), "sys-chan", None).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["from"], "midtown");
    assert_eq!(msgs[0]["message"], "daemon started");
    assert_eq!(msgs[0]["msg_type"], "system");
}

#[test]
fn read_messages_with_limit() {
    let dir = TempDir::new().unwrap();
    for i in 0..5 {
        post_message(dir.path(), "chan", "bob", &format!("msg {i}"), None).unwrap();
    }

    let msgs = read_messages(dir.path(), "chan", Some(2)).unwrap();
    assert_eq!(msgs.len(), 2);
    // Should be the last 2 messages
    assert_eq!(msgs[0]["message"], "msg 3");
    assert_eq!(msgs[1]["message"], "msg 4");
}

#[test]
fn list_channels_shows_created_channels() {
    let dir = TempDir::new().unwrap();
    post_message(dir.path(), "alpha", "user", "hi", None).unwrap();
    post_message(dir.path(), "beta", "user", "hi", None).unwrap();

    let channels = list_channels(dir.path()).unwrap();
    let names: Vec<&str> = channels.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"alpha"), "missing alpha: {names:?}");
    assert!(names.contains(&"beta"), "missing beta: {names:?}");
}

/// Spec 5.3: WHEN a channel's JSONL file exceeds 10MB THEN the system SHALL
/// roll to a new file, and reading SHALL operate across all files
#[test]
fn jsonl_rolling_at_10mb() {
    let dir = tempfile::TempDir::new().unwrap();

    // Write enough data to exceed 10MB
    // Each message is ~100 bytes of JSON, so we need ~100K messages
    // For speed, create one large message to exceed the threshold
    let big_content = "x".repeat(5 * 1024 * 1024); // 5MB per message

    // First message — under threshold
    post_message(dir.path(), "big-chan", "alice", &big_content, None).unwrap();

    // Check that current.jsonl exists
    let history_dir = dir.path().join("channels").join("big-chan").join("history");
    assert!(
        history_dir.join("current.jsonl").exists(),
        "current.jsonl should exist after first write"
    );

    // Second message — pushes past 10MB, triggers roll
    post_message(dir.path(), "big-chan", "bob", &big_content, None).unwrap();

    // Third message — written to new current.jsonl
    post_message(dir.path(), "big-chan", "carol", "small msg", None).unwrap();

    // Read should return ALL messages across all files
    let msgs = read_messages(dir.path(), "big-chan", None).unwrap();
    assert_eq!(
        msgs.len(),
        3,
        "all 3 messages should be readable across rolled files, got {}",
        msgs.len()
    );

    // Verify we have an archive file
    let archive_files: Vec<_> = std::fs::read_dir(&history_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.ends_with(".jsonl") && name != "current.jsonl"
        })
        .collect();
    assert!(
        !archive_files.is_empty(),
        "should have at least one archive file after rolling"
    );
}

/// Spec 5.3: WHEN messages are read from a channel THEN thread replies SHALL be
/// excluded UNLESS the read request specifies a thread_parent_id
#[test]
fn read_messages_excludes_thread_replies() {
    let dir = TempDir::new().unwrap();
    post_message(dir.path(), "chan", "alice", "top-level 1", None).unwrap();
    post_message(dir.path(), "chan", "alice", "top-level 2", None).unwrap();

    // Get the first message's ID to use as thread parent
    let all_msgs = read_messages(dir.path(), "chan", None).unwrap();
    let parent_id = all_msgs[0]["id"].as_str().unwrap().to_string();

    // Post a thread reply
    post_message(
        dir.path(),
        "chan",
        "bob",
        "this is a reply",
        Some(&parent_id),
    )
    .unwrap();

    // Default read should exclude the thread reply
    let msgs = read_messages(dir.path(), "chan", None).unwrap();
    assert_eq!(
        msgs.len(),
        2,
        "thread reply should be excluded from default read, got {}",
        msgs.len()
    );
    assert!(
        msgs.iter()
            .all(|m| m.get("thread_parent_id").is_none() || m["thread_parent_id"].is_null()),
        "no message should have thread_parent_id in default read"
    );
}
