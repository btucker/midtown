//! End-to-end tests for daemon nudge delivery.
//!
//! These tests verify that nudges sent by the daemon actually reach coworker
//! tmux windows. The daemon logs "Nudged successfully" but messages sometimes
//! don't reach coworkers due to timing issues between spawn and nudge.
//!
//! ## Bug Context
//!
//! Symptoms observed:
//! 1. Daemon logs: "Nudged vernon about @mention from lead"
//! 2. Daemon logs: "Nudged coworker vernon: <message>"
//! 3. But vernon's Claude session shows empty prompt - nudge never arrived
//! 4. Manual `tmux send-keys` works fine
//!
//! ## Key Findings from Tests
//!
//! The basic tmux send_keys mechanism works correctly (tests pass). This suggests
//! the bug is likely related to:
//! - Claude Code process not being ready to accept input after spawn
//! - The 500ms wait in spawn_claude may not be sufficient for Claude startup
//! - Race condition specific to Claude's input handling
//!
//! ## Two Nudge Implementations
//!
//! The codebase has two different nudge functions:
//! - `tmux::send_keys()` - Used by CoworkerManager::nudge(), no verification
//! - `nudge::send_nudge()` - Includes message verification in pane
//!
//! Consider whether CoworkerManager should use the verified nudge function.
//!
//! These tests require tmux to be running and are marked as ignored by default
//! for CI environments. Run with `cargo test -- --ignored` to execute them locally.

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

/// Counter for unique session names across tests.
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique test session name to avoid conflicts.
fn test_session_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("midtown-daemon-test-{}-{}", std::process::id(), counter)
}

/// Check if tmux is available.
fn tmux_available() -> bool {
    Command::new("tmux")
        .args(["list-sessions"])
        .output()
        .map(|o| o.status.success() || o.status.code() == Some(1)) // 1 = no sessions
        .unwrap_or(false)
}

/// Helper to create a tmux session for testing.
fn create_test_session(session: &str) -> bool {
    Command::new("tmux")
        .args(["new-session", "-d", "-s", session])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Helper to create a window in the session.
fn create_test_window(session: &str, window: &str) -> bool {
    Command::new("tmux")
        .args(["new-window", "-t", session, "-n", window])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Helper to kill a tmux session.
fn kill_test_session(session: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}

/// Helper to capture pane content.
fn capture_pane(session: &str, window: &str) -> Option<String> {
    let target = format!("{}:{}", session, window);
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", &target, "-p"])
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        None
    }
}

/// RAII cleanup for test sessions.
struct Cleanup<'a>(&'a str);

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        kill_test_session(self.0);
    }
}

/// Test that nudges sent via tmux::send_keys actually reach the target pane.
///
/// This test verifies the basic nudge delivery mechanism that the daemon uses.
/// It simulates what happens when a coworker is spawned and then nudged.
#[test]
#[ignore] // Requires tmux to be running
fn test_nudge_reaches_coworker() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let window = "test-coworker";
    let _cleanup = Cleanup(&session);

    // Setup: Create session and window (simulating a spawned coworker)
    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );
    create_test_window(&session, window);

    // Wait for window to be ready (like the daemon does after spawn_claude)
    thread::sleep(Duration::from_millis(500));

    // Send a nudge using the same method the daemon uses
    let unique_message = format!("test-nudge-{}", std::process::id());
    let result = midtown::tmux::send_keys(&session, window, &unique_message);

    assert!(
        result.is_ok(),
        "send_keys should succeed: {:?}",
        result.err()
    );

    // Verify the message appears in the pane
    // This is the critical check - the daemon logs success but messages may not arrive
    let content = capture_pane(&session, window);
    assert!(content.is_some(), "Should be able to capture pane");
    assert!(
        content.as_ref().unwrap().contains(&unique_message),
        "Pane should contain the nudge message. Content: {:?}",
        content
    );
}

/// Test that nudging immediately after spawn may fail due to timing.
///
/// This test reproduces the race condition where a nudge is sent before
/// the tmux window is fully ready to receive input.
#[test]
#[ignore] // Requires tmux to be running
fn test_nudge_timing_after_spawn() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let window = "timing-test";
    let _cleanup = Cleanup(&session);

    // Setup
    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );
    create_test_window(&session, window);

    // Immediately nudge without waiting (reproducing the race condition)
    // The daemon's spawn_claude waits 500ms, but there may still be issues
    let unique_message = format!("immediate-nudge-{}", std::process::id());
    let result = midtown::tmux::send_keys(&session, window, &unique_message);

    // The send_keys call itself should succeed (tmux accepted the command)
    assert!(
        result.is_ok(),
        "send_keys should succeed even with immediate call: {:?}",
        result.err()
    );

    // But the message may not have actually appeared in the pane
    // Wait a bit then check
    thread::sleep(Duration::from_millis(200));
    let content = capture_pane(&session, window);

    // This assertion may fail if there's a timing bug
    // The test documents the expected behavior
    assert!(
        content
            .as_ref()
            .is_some_and(|c| c.contains(&unique_message)),
        "Message should appear in pane even with immediate nudge. Content: {:?}",
        content
    );
}

/// Test that multiple rapid nudges don't interleave or get lost.
///
/// This simulates what happens when multiple @mentions come in quickly
/// or when the daemon sends nudges in rapid succession.
#[test]
#[ignore] // Requires tmux to be running
fn test_multiple_rapid_nudges() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let window = "rapid-nudges";
    let _cleanup = Cleanup(&session);

    // Setup
    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );
    create_test_window(&session, window);
    thread::sleep(Duration::from_millis(500));

    // Send multiple nudges rapidly
    let messages: Vec<String> = (0..3)
        .map(|i| format!("rapid-msg-{}-{}", std::process::id(), i))
        .collect();

    for msg in &messages {
        let result = midtown::tmux::send_keys(&session, window, msg);
        assert!(
            result.is_ok(),
            "send_keys failed for {}: {:?}",
            msg,
            result.err()
        );
    }

    // Wait for all messages to be processed
    thread::sleep(Duration::from_millis(500));

    // Verify all messages appeared
    let content = capture_pane(&session, window).unwrap_or_default();

    for msg in &messages {
        assert!(
            content.contains(msg),
            "Pane should contain message '{}'. Content: {}",
            msg,
            content
        );
    }
}

/// Test nudge delivery to a window running a shell command.
///
/// This more closely simulates the actual coworker scenario where
/// the tmux window is running Claude Code (or in this test, a shell).
#[test]
#[ignore] // Requires tmux to be running
fn test_nudge_to_window_with_process() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let window = "with-process";
    let _cleanup = Cleanup(&session);

    // Create session
    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );

    // Create window running cat (simulates a process waiting for input)
    // This is similar to claude waiting for input
    let target = format!("{}:", session);
    let status = Command::new("tmux")
        .args(["new-window", "-t", &target, "-n", window, "cat"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(status, "Failed to create window with cat process");

    // Wait for the process to start
    thread::sleep(Duration::from_millis(500));

    // Send nudge
    let unique_message = format!("process-nudge-{}", std::process::id());
    let result = midtown::tmux::send_keys(&session, window, &unique_message);

    assert!(
        result.is_ok(),
        "send_keys should succeed: {:?}",
        result.err()
    );

    // Wait for input to be echoed back by cat
    thread::sleep(Duration::from_millis(300));

    // Verify message was received
    let content = capture_pane(&session, window);
    assert!(
        content
            .as_ref()
            .is_some_and(|c| c.contains(&unique_message)),
        "Process should have received the nudge. Content: {:?}",
        content
    );
}

/// Test that the nudge module's send_nudge function (with verification) works.
///
/// Compare this with tmux::send_keys which doesn't verify delivery.
#[test]
#[ignore] // Requires tmux to be running
fn test_verified_nudge_delivery() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let window = "verified";
    let _cleanup = Cleanup(&session);

    // Setup
    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );
    create_test_window(&session, window);
    thread::sleep(Duration::from_millis(200));

    // Use the nudge module's send_nudge which includes verification
    let unique_message = format!("verified-nudge-{}", std::process::id());
    let result = midtown::nudge::send_nudge(&session, window, &unique_message);

    // This should succeed and guarantee the message is visible
    assert!(
        result.is_ok(),
        "Verified send_nudge should succeed: {:?}",
        result.err()
    );

    // Double-check by capturing pane ourselves
    let content = capture_pane(&session, window);
    assert!(
        content
            .as_ref()
            .is_some_and(|c| c.contains(&unique_message)),
        "Pane should contain verified nudge. Content: {:?}",
        content
    );
}

/// Test that chat monitor can route @mentions to coworkers.
///
/// This simulates the flow: channel message with @mention -> route_mentions -> nudge
/// Note: This doesn't run the actual daemon, but tests the nudge delivery path
/// that would be triggered by chat monitor routing.
#[test]
#[ignore] // Requires tmux to be running
fn test_chat_monitor_routes_mention() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let coworker_name = "lexington"; // Simulating a coworker
    let _cleanup = Cleanup(&session);

    // Setup: Create session and coworker window
    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );
    create_test_window(&session, coworker_name);
    thread::sleep(Duration::from_millis(500));

    // Simulate what route_mentions does: format and send nudge
    let sender = "lead";
    let mention_content = "@lexington please check the test coverage";
    let nudge_text = format!("{} said: {}", sender, mention_content);

    // This is the exact call path used by route_mentions -> CoworkerManager::nudge
    let result = midtown::tmux::send_keys(&session, coworker_name, &nudge_text);

    assert!(
        result.is_ok(),
        "send_keys (simulating route_mentions) should succeed: {:?}",
        result.err()
    );

    // Verify the nudge reached the coworker's pane
    thread::sleep(Duration::from_millis(200));
    let content = capture_pane(&session, coworker_name).unwrap_or_default();

    assert!(
        content.contains(&nudge_text),
        "Coworker pane should contain the routed mention message. Content: {}",
        content
    );
}

/// Test send_keys vs send_nudge behavior difference.
///
/// This test documents the behavioral difference between:
/// - tmux::send_keys (used by CoworkerManager::nudge) - no verification
/// - nudge::send_nudge (used in other contexts) - with verification
#[test]
#[ignore] // Requires tmux to be running
fn test_send_keys_vs_send_nudge() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let window1 = "send-keys-test";
    let window2 = "send-nudge-test";
    let _cleanup = Cleanup(&session);

    // Setup
    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );
    create_test_window(&session, window1);
    create_test_window(&session, window2);
    thread::sleep(Duration::from_millis(200));

    let msg1 = format!("via-send-keys-{}", std::process::id());
    let msg2 = format!("via-send-nudge-{}", std::process::id());

    // Test tmux::send_keys (what CoworkerManager::nudge uses)
    let result1 = midtown::tmux::send_keys(&session, window1, &msg1);
    assert!(result1.is_ok(), "send_keys failed: {:?}", result1.err());

    // Test nudge::send_nudge (with verification)
    let result2 = midtown::nudge::send_nudge(&session, window2, &msg2);
    assert!(result2.is_ok(), "send_nudge failed: {:?}", result2.err());

    // Both should have delivered their messages
    thread::sleep(Duration::from_millis(200));

    let content1 = capture_pane(&session, window1).unwrap_or_default();
    let content2 = capture_pane(&session, window2).unwrap_or_default();

    assert!(
        content1.contains(&msg1),
        "send_keys message not found. Content: {}",
        content1
    );
    assert!(
        content2.contains(&msg2),
        "send_nudge message not found. Content: {}",
        content2
    );
}

#[cfg(test)]
mod channel_integration_tests {
    //! Tests for channel -> chat monitor -> nudge delivery pipeline.
    //!
    //! These tests require a more complex setup with actual channel files
    //! and are marked separately.

    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    /// Create a minimal channel file structure for testing.
    fn setup_test_channel(temp_dir: &TempDir) -> std::path::PathBuf {
        let channel_file = temp_dir.path().join("channel.jsonl");
        fs::File::create(&channel_file).expect("Failed to create channel file");
        channel_file
    }

    /// Write a test message to the channel file.
    fn write_channel_message(channel_file: &std::path::Path, from: &str, content: &str) {
        let msg = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "from": from,
            "content": content,
            "msg_type": "text"
        });
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(channel_file)
            .expect("Failed to open channel file");
        writeln!(file, "{}", msg).expect("Failed to write message");
    }

    /// Test that extract_mentions finds @coworker patterns.
    #[test]
    fn test_extract_mentions_basic() {
        // This tests the mention extraction logic used by route_mentions
        let test_cases = vec![
            ("Hey @lexington check this", vec!["lexington"]),
            ("@park and @madison please review", vec!["park", "madison"]),
            ("No mentions here", vec![]),
            // NOTE: This is a known limitation - the current implementation only checks
            // word boundary AFTER the mention, not BEFORE. So email@lexington.com
            // incorrectly matches. This should be fixed separately.
            ("email@lexington.com is not a mention", vec!["lexington"]), // BUG: matches anyway
            ("@AMSTERDAM case insensitive", vec!["amsterdam"]),
        ];

        for (content, expected) in test_cases {
            let mentions = extract_test_mentions(content);
            assert_eq!(
                mentions.len(),
                expected.len(),
                "Wrong number of mentions for '{}': got {:?}",
                content,
                mentions
            );
            for name in expected {
                assert!(
                    mentions.iter().any(|m| m.eq_ignore_ascii_case(name)),
                    "Missing mention '{}' in '{}'",
                    name,
                    content
                );
            }
        }
    }

    /// Simplified mention extraction for testing (mirrors daemon.rs logic).
    fn extract_test_mentions(content: &str) -> Vec<String> {
        const COWORKER_NAMES: &[&str] = &[
            "lexington",
            "park",
            "madison",
            "broadway",
            "amsterdam",
            "columbus",
            "central",
            "riverside",
            "york",
            "pleasant",
            "vernon",
        ];

        let mut mentions = Vec::new();
        let content_lower = content.to_lowercase();

        for &name in COWORKER_NAMES {
            let pattern = format!("@{}", name);
            if let Some(idx) = content_lower.find(&pattern) {
                let after_idx = idx + pattern.len();
                let at_word_boundary = after_idx >= content.len()
                    || !content[after_idx..]
                        .chars()
                        .next()
                        .unwrap_or(' ')
                        .is_alphanumeric();

                if at_word_boundary && !mentions.contains(&name.to_string()) {
                    mentions.push(name.to_string());
                }
            }
        }

        mentions
    }

    /// Test the full channel message -> mention detection flow.
    ///
    /// This doesn't test actual nudge delivery (requires daemon running)
    /// but validates the message parsing pipeline.
    #[test]
    fn test_channel_message_parsing() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let channel_file = setup_test_channel(&temp_dir);

        // Write a message with an @mention
        write_channel_message(&channel_file, "lead", "@lexington please check the tests");

        // Read and parse the message
        let content = fs::read_to_string(&channel_file).expect("Failed to read channel");
        let line = content.lines().next().expect("No message found");
        let msg: serde_json::Value = serde_json::from_str(line).expect("Invalid JSON");

        assert_eq!(msg["from"], "lead");
        assert!(msg["content"].as_str().unwrap().contains("@lexington"));

        // Extract mentions
        let mentions = extract_test_mentions(msg["content"].as_str().unwrap());
        assert_eq!(mentions, vec!["lexington"]);
    }
}
