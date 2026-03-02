use super::*;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn test_build_snippet_match_at_start() {
    let content = "Hello world, this is a test message for search";
    let snippet = build_snippet(content, "Hello", 50);
    assert!(snippet.contains("Hello"));
    assert!(!snippet.starts_with("..."));
}

#[test]
fn test_build_snippet_match_in_middle() {
    let content = "The quick brown fox jumps over the lazy dog and then does some more things after that for padding";
    let snippet = build_snippet(content, "jumps", 20);
    assert!(snippet.contains("jumps"));
    // Should have ellipsis since match is in the middle
    assert!(snippet.contains("..."));
}

#[test]
fn test_build_snippet_no_match() {
    let content = "Hello world";
    let snippet = build_snippet(content, "xyz", 50);
    // Falls back to truncation
    assert!(snippet.contains("Hello world"));
}

#[test]
fn test_build_snippet_case_insensitive_position() {
    let content = "This is a TEST message";
    let snippet = build_snippet(content, "test", 50);
    assert!(snippet.contains("TEST"));
}

#[test]
fn test_build_snippet_short_content() {
    let content = "short";
    let snippet = build_snippet(content, "short", 50);
    assert_eq!(snippet, "short");
}

#[test]
fn test_channel_name_from_path_valid() {
    let path =
        PathBuf::from("/home/user/.midtown/projects/repo/channels/midtown/history/current.jsonl");
    assert_eq!(channel_name_from_path(&path), Some("midtown".to_string()));
}

#[test]
fn test_channel_name_from_path_dated_file() {
    let path = PathBuf::from("/data/channels/proj-search/history/2026-03-01.jsonl");
    assert_eq!(
        channel_name_from_path(&path),
        Some("proj-search".to_string())
    );
}

#[test]
fn test_channel_name_from_path_invalid_structure() {
    let path = PathBuf::from("/some/random/path/file.jsonl");
    assert_eq!(channel_name_from_path(&path), None);
}

#[test]
fn test_channel_name_from_path_no_history_dir() {
    let path = PathBuf::from("/channels/midtown/logs/current.jsonl");
    assert_eq!(channel_name_from_path(&path), None);
}

#[test]
fn test_search_empty_query() {
    let dir = TempDir::new().unwrap();
    let result = search_messages_sync(dir.path(), "", 50).unwrap();
    assert!(result.results.is_empty());
    assert_eq!(result.total, 0);
}

#[test]
fn test_search_whitespace_query() {
    let dir = TempDir::new().unwrap();
    let result = search_messages_sync(dir.path(), "   ", 50).unwrap();
    assert!(result.results.is_empty());
}

#[test]
fn test_search_no_channels_dir() {
    let dir = TempDir::new().unwrap();
    let result = search_messages_sync(dir.path(), "hello", 50).unwrap();
    assert!(result.results.is_empty());
    assert_eq!(result.total, 0);
}

#[test]
fn test_search_with_messages() {
    let dir = TempDir::new().unwrap();
    let base = dir.path();

    // Create channel directory structure
    let history_dir = base.join("channels").join("test-channel").join("history");
    std::fs::create_dir_all(&history_dir).unwrap();

    // Write test messages
    let msg1 = crate::Message::text("alice", "Hello world from alice");
    let msg2 = crate::Message::text("bob", "Goodbye from bob");
    let msg3 = crate::Message::text("alice", "Another hello message");

    let mut content = String::new();
    content.push_str(&serde_json::to_string(&msg1).unwrap());
    content.push('\n');
    content.push_str(&serde_json::to_string(&msg2).unwrap());
    content.push('\n');
    content.push_str(&serde_json::to_string(&msg3).unwrap());
    content.push('\n');

    std::fs::write(history_dir.join("current.jsonl"), &content).unwrap();

    let result = search_messages_sync(base, "hello", 50).unwrap();
    assert_eq!(result.total, 2);
    assert_eq!(result.results.len(), 2);
    assert!(result.results.iter().all(|r| r.channel == "test-channel"));
    assert!(
        result
            .results
            .iter()
            .all(|r| r.content.to_lowercase().contains("hello"))
    );
}

#[test]
fn test_search_limit() {
    let dir = TempDir::new().unwrap();
    let base = dir.path();

    let history_dir = base.join("channels").join("test").join("history");
    std::fs::create_dir_all(&history_dir).unwrap();

    let mut content = String::new();
    for i in 0..10 {
        let msg = crate::Message::text("user", format!("test message number {}", i));
        content.push_str(&serde_json::to_string(&msg).unwrap());
        content.push('\n');
    }

    std::fs::write(history_dir.join("current.jsonl"), &content).unwrap();

    let result = search_messages_sync(base, "test message", 3).unwrap();
    assert_eq!(result.total, 10);
    assert_eq!(result.results.len(), 3);
}

#[test]
fn test_search_sorted_by_timestamp_desc() {
    let dir = TempDir::new().unwrap();
    let base = dir.path();

    let history_dir = base.join("channels").join("test").join("history");
    std::fs::create_dir_all(&history_dir).unwrap();

    // Create messages with known order (they'll have auto-generated timestamps)
    let msg1 = crate::Message::text("alice", "search term first");
    // Small sleep to ensure different timestamps
    std::thread::sleep(std::time::Duration::from_millis(10));
    let msg2 = crate::Message::text("bob", "search term second");

    let mut content = String::new();
    content.push_str(&serde_json::to_string(&msg1).unwrap());
    content.push('\n');
    content.push_str(&serde_json::to_string(&msg2).unwrap());
    content.push('\n');

    std::fs::write(history_dir.join("current.jsonl"), &content).unwrap();

    let result = search_messages_sync(base, "search term", 50).unwrap();
    assert_eq!(result.results.len(), 2);
    // Newest first
    assert!(result.results[0].timestamp >= result.results[1].timestamp);
}

#[test]
fn test_search_case_insensitive() {
    let dir = TempDir::new().unwrap();
    let base = dir.path();

    let history_dir = base.join("channels").join("test").join("history");
    std::fs::create_dir_all(&history_dir).unwrap();

    let msg = crate::Message::text("alice", "UPPERCASE content here");
    let content = format!("{}\n", serde_json::to_string(&msg).unwrap());
    std::fs::write(history_dir.join("current.jsonl"), &content).unwrap();

    let result = search_messages_sync(base, "uppercase", 50).unwrap();
    assert_eq!(result.total, 1);
}

#[test]
fn test_search_multiple_channels() {
    let dir = TempDir::new().unwrap();
    let base = dir.path();

    for ch_name in &["alpha", "beta"] {
        let history_dir = base.join("channels").join(ch_name).join("history");
        std::fs::create_dir_all(&history_dir).unwrap();

        let msg = crate::Message::text("user", format!("findme in {}", ch_name));
        let content = format!("{}\n", serde_json::to_string(&msg).unwrap());
        std::fs::write(history_dir.join("current.jsonl"), &content).unwrap();
    }

    let result = search_messages_sync(base, "findme", 50).unwrap();
    assert_eq!(result.total, 2);
    let channels: Vec<&str> = result.results.iter().map(|r| r.channel.as_str()).collect();
    assert!(channels.contains(&"alpha"));
    assert!(channels.contains(&"beta"));
}

#[test]
fn test_snippet_has_context() {
    let result = build_snippet(
        "This is some text before the MATCH keyword and some text after it too",
        "MATCH",
        15,
    );
    assert!(result.contains("MATCH"));
    assert!(result.contains("..."));
}

#[test]
fn test_snap_to_char_boundary_ascii() {
    let s = "hello";
    assert_eq!(snap_to_char_boundary(s, 3, true), 3);
    assert_eq!(snap_to_char_boundary(s, 3, false), 3);
}

#[test]
fn test_snap_to_char_boundary_beyond_end() {
    let s = "hi";
    assert_eq!(snap_to_char_boundary(s, 100, true), 2);
    assert_eq!(snap_to_char_boundary(s, 100, false), 2);
}

#[test]
fn test_build_snippet_with_unicode() {
    let content = "Hello 🌃 world, this is a test";
    let snippet = build_snippet(content, "world", 50);
    assert!(snippet.contains("world"));
    assert!(snippet.contains("🌃"));
}
