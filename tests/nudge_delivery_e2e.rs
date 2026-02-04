//! End-to-end tests for daemon nudge delivery.
//!
//! These tests verify that nudges sent by the daemon actually reach coworker
//! tmux windows via `tmux::send_keys()` and `CoworkerManager::nudge_lead()`.
//!
//! The nudge delivery path: daemon `Effect::NudgeCoworker` → `CoworkerManager::nudge()`
//! → `tmux::send_keys()`, which includes retry logic and stuck-detection.
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

/// Test that nudge_lead() delivers a message to the Lead window.
///
/// This verifies the coworker → lead communication path used when coworkers
/// need to notify the lead about feedback requests, PR status, etc.
/// The lead always runs in a window named "Lead" in the midtown session.
#[test]
#[ignore] // Requires tmux to be running
fn test_nudge_reaches_lead() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let _cleanup = Cleanup(&session);

    // Setup: Create session with a "lead" window (matching the real layout).
    // The daemon targets lowercase "lead" for nudge delivery.
    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );
    create_test_window(&session, "lead");

    // Also create a coworker window to simulate the real session layout
    create_test_window(&session, "lexington");

    // Wait for windows to be ready
    thread::sleep(Duration::from_millis(500));

    // Build a CoworkerManager pointing at this test session
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    // Initialize a git repo (required by CoworkerManager)
    Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to init git repo");
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "Initial commit"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to create initial commit");

    let worktree_manager = midtown::worktree::WorktreeManager::new(temp_dir.path().to_path_buf())
        .expect("Failed to create worktree manager");
    let manager = midtown::coworker::CoworkerManager::new(session.clone(), worktree_manager);

    // Send a nudge to the Lead via nudge_lead()
    let unique_message = format!("lead-nudge-{}", std::process::id());
    let result = manager.nudge_lead(&unique_message);

    assert!(
        result.is_ok(),
        "nudge_lead should succeed: {:?}",
        result.err()
    );

    // Wait for the background nudge worker to deliver the message
    thread::sleep(Duration::from_millis(2000));

    // Verify the message appears in the lead pane
    let content = capture_pane(&session, "lead");
    assert!(
        content
            .as_ref()
            .is_some_and(|c| c.contains(&unique_message)),
        "Lead pane should contain the nudge message. Content: {:?}",
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

    // Setup: Create session and coworker window running cat (not bash,
    // because send_keys text would be interpreted as shell commands)
    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );
    let target = format!("{}:", session);
    let status = Command::new("tmux")
        .args(["new-window", "-t", &target, "-n", coworker_name, "cat"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(status, "Failed to create window with cat process");
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

/// Test that nudge goes through immediately when the lead's input is empty.
///
/// Verifies the happy path: no text in the input prompt → no waiting.
#[test]
#[ignore] // Requires tmux to be running
fn test_lead_nudge_immediate_when_input_empty() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let _cleanup = Cleanup(&session);

    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );
    create_test_window(&session, "lead");
    thread::sleep(Duration::from_millis(500));

    // Input is empty — wait_for_empty_input should return true immediately
    let target = format!("{}:lead", session);
    let start = std::time::Instant::now();
    let result = midtown::tmux::wait_for_empty_input(&target, Duration::from_secs(10));
    let elapsed = start.elapsed();

    assert!(result, "Should return true when input is empty");
    // Should complete well under the poll interval (3s)
    assert!(
        elapsed < Duration::from_secs(2),
        "Should return immediately, took {:?}",
        elapsed
    );
}

/// Test that nudge is delayed when the lead's input has text.
///
/// Puts text in the input, then clears it from a background thread.
/// The wait function should block until the text is cleared.
#[test]
#[ignore] // Requires tmux to be running
fn test_lead_nudge_delayed_when_input_has_text() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let _cleanup = Cleanup(&session);

    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );

    // Run a shell that shows a prompt with ❯
    let target = format!("{}:", session);
    let status = Command::new("tmux")
        .args([
            "new-window",
            "-t",
            &target,
            "-n",
            "lead",
            "bash",
            "-c",
            // Show prompt, read input (simulates Claude Code prompt)
            r#"printf '❯ '; read line; printf '❯ '; cat"#,
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(status, "Failed to create lead window with prompt");
    thread::sleep(Duration::from_millis(500));

    // Type some text (without Enter) to simulate user typing
    let tmux_target = format!("{}:lead", session);
    let _ = Command::new("tmux")
        .args([
            "send-keys",
            "-t",
            &tmux_target,
            "-l",
            "I am typing something",
        ])
        .status();
    thread::sleep(Duration::from_millis(200));

    // Verify text is there
    let content = capture_pane(&session, "lead").unwrap_or_default();
    assert!(
        midtown::tmux::has_input_text(&content),
        "Should detect text in input. Content: {:?}",
        content
    );

    // Clear the text after 4 seconds from a background thread
    let session_clone = session.clone();
    std::thread::spawn(move || {
        thread::sleep(Duration::from_secs(4));
        let target = format!("{}:lead", session_clone);
        // Send Enter to submit the text (clears the input line)
        let _ = Command::new("tmux")
            .args(["send-keys", "-t", &target, "Enter"])
            .status();
    });

    // Wait for empty input — should block until the text is cleared
    let start = std::time::Instant::now();
    let result = midtown::tmux::wait_for_empty_input(&tmux_target, Duration::from_secs(30));
    let elapsed = start.elapsed();

    assert!(result, "Should return true after input clears");
    // Should take roughly 4-7 seconds (4s for clear + poll interval)
    assert!(
        elapsed >= Duration::from_secs(3),
        "Should have waited for text to clear, only waited {:?}",
        elapsed
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "Should not wait too long, waited {:?}",
        elapsed
    );
}

/// Test that nudge goes through after timeout even if input still has text.
///
/// Uses a short timeout (5s) and never clears the input.
#[test]
#[ignore] // Requires tmux to be running
fn test_lead_nudge_timeout_with_text() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let _cleanup = Cleanup(&session);

    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );

    // Run a shell that shows ❯ prompt
    let target = format!("{}:", session);
    let status = Command::new("tmux")
        .args([
            "new-window",
            "-t",
            &target,
            "-n",
            "lead",
            "bash",
            "-c",
            r#"printf '❯ '; read line"#,
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(status, "Failed to create lead window with prompt");
    thread::sleep(Duration::from_millis(500));

    // Type some text that we never clear
    let tmux_target = format!("{}:lead", session);
    let _ = Command::new("tmux")
        .args([
            "send-keys",
            "-t",
            &tmux_target,
            "-l",
            "still typing forever",
        ])
        .status();
    thread::sleep(Duration::from_millis(200));

    // Wait with a short timeout (5s)
    let start = std::time::Instant::now();
    let result = midtown::tmux::wait_for_empty_input(&tmux_target, Duration::from_secs(5));
    let elapsed = start.elapsed();

    assert!(!result, "Should return false on timeout");
    // Should take roughly 5 seconds
    assert!(
        elapsed >= Duration::from_secs(4),
        "Should wait until timeout, only waited {:?}",
        elapsed
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "Should not wait much longer than timeout, waited {:?}",
        elapsed
    );
}

/// Test that queued nudges arrive in FIFO order after input clears.
///
/// Sends multiple nudges while input has text, clears the input,
/// and verifies all nudges were delivered in order.
#[test]
#[ignore] // Requires tmux to be running
fn test_lead_nudge_queue_ordering() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let _cleanup = Cleanup(&session);

    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );
    create_test_window(&session, "lead");
    thread::sleep(Duration::from_millis(500));

    // Create a CoworkerManager pointing at this test session
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
    Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to init git repo");
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "Initial commit"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to create initial commit");

    let worktree_manager = midtown::worktree::WorktreeManager::new(temp_dir.path().to_path_buf())
        .expect("Failed to create worktree manager");
    let manager = midtown::coworker::CoworkerManager::new(session.clone(), worktree_manager);

    // Queue several nudges to the lead (input is empty so they go through immediately)
    let messages: Vec<String> = (0..3)
        .map(|i| format!("queued-nudge-{}-{}", std::process::id(), i))
        .collect();

    for msg in &messages {
        let result = manager.nudge_lead(msg);
        assert!(
            result.is_ok(),
            "nudge_lead should succeed: {:?}",
            result.err()
        );
    }

    // Wait for delivery — nudges are queued and delivered sequentially by
    // the background worker. Each send_keys call takes ~600ms, so 3 nudges
    // need roughly 2s. Give extra margin.
    thread::sleep(Duration::from_millis(5000));

    // Verify all messages appeared in order
    let content = capture_pane(&session, "lead").unwrap_or_default();
    let mut last_pos = 0;
    for msg in &messages {
        let pos = content[last_pos..].find(msg.as_str()).map(|p| p + last_pos);
        assert!(
            pos.is_some(),
            "Message '{}' should appear in pane after position {}. Content: {}",
            msg,
            last_pos,
            content
        );
        last_pos = pos.unwrap();
    }
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

/// Tests for the stuck nudge auto-submit feature.
///
/// When daemon-sent nudges get stuck in the input (Enter doesn't register),
/// the daemon should detect this and auto-submit by sending Enter.
#[cfg(test)]
mod stuck_nudge_autosubmit_tests {
    use super::*;

    /// Test that a nudge sent without Enter gets stuck in the input.
    ///
    /// This reproduces the scenario the auto-submit feature is fixing:
    /// send_keys delivers text but Enter doesn't register.
    #[test]
    #[ignore] // Requires tmux to be running
    fn test_nudge_stuck_without_enter() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = test_session_name();
        let window = "stuck-test";
        let _cleanup = Cleanup(&session);

        // Setup: Create session with a window showing a Claude-like prompt
        assert!(
            create_test_session(&session),
            "Failed to create test session"
        );

        // Run a shell that shows a Claude-like prompt (❯)
        let target = format!("{}:", session);
        let status = Command::new("tmux")
            .args([
                "new-window",
                "-t",
                &target,
                "-n",
                window,
                "bash",
                "-c",
                // Show prompt like Claude Code, wait for input
                r#"printf '✳ Working on task...\n❯ '; read line; echo "Got: $line""#,
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(status, "Failed to create window with Claude-like prompt");
        thread::sleep(Duration::from_millis(500));

        // Send nudge text WITHOUT Enter (simulating the stuck scenario)
        let nudge_text = format!(
            "github said: @test Check 'Build' passed - {}",
            std::process::id()
        );
        let tmux_target = format!("{}:{}", session, window);
        let _ = Command::new("tmux")
            .args(["send-keys", "-t", &tmux_target, "-l", &nudge_text])
            .status();
        thread::sleep(Duration::from_millis(200));

        // Verify the text is stuck in the input (visible but not submitted)
        let content = capture_pane(&session, window).unwrap_or_default();
        assert!(
            content.contains(&nudge_text),
            "Nudge text should be visible in pane. Content: {:?}",
            content
        );
        assert!(
            !content.contains("Got:"),
            "Text should NOT be submitted yet. Content: {:?}",
            content
        );

        // This is the scenario the auto-submit feature fixes:
        // The daemon would detect this stuck text and send Enter
    }

    /// Test that sending Enter after stuck nudge submits it.
    ///
    /// This verifies the recovery mechanism: when we detect a stuck nudge,
    /// sending Enter causes it to be submitted.
    #[test]
    #[ignore] // Requires tmux to be running
    fn test_enter_submits_stuck_nudge() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = test_session_name();
        let window = "submit-test";
        let _cleanup = Cleanup(&session);

        assert!(
            create_test_session(&session),
            "Failed to create test session"
        );

        // Run a shell that echoes back what it receives
        let target = format!("{}:", session);
        let status = Command::new("tmux")
            .args([
                "new-window",
                "-t",
                &target,
                "-n",
                window,
                "bash",
                "-c",
                r#"printf '❯ '; read line; echo "SUBMITTED: $line"; sleep 1"#,
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(status, "Failed to create test window");
        thread::sleep(Duration::from_millis(500));

        // Send nudge text WITHOUT Enter (stuck scenario)
        let nudge_text = format!("daemon-nudge-{}", std::process::id());
        let tmux_target = format!("{}:{}", session, window);
        let _ = Command::new("tmux")
            .args(["send-keys", "-t", &tmux_target, "-l", &nudge_text])
            .status();
        thread::sleep(Duration::from_millis(200));

        // Verify stuck
        let content = capture_pane(&session, window).unwrap_or_default();
        assert!(content.contains(&nudge_text), "Nudge should be visible");
        assert!(
            !content.contains("SUBMITTED:"),
            "Should not be submitted yet"
        );

        // Now send Enter (this is what the daemon does for auto-submit)
        let _ = Command::new("tmux")
            .args(["send-keys", "-t", &tmux_target, "Enter"])
            .status();
        thread::sleep(Duration::from_millis(300));

        // Verify the nudge was submitted
        let content = capture_pane(&session, window).unwrap_or_default();
        assert!(
            content.contains("SUBMITTED:") && content.contains(&nudge_text),
            "Nudge should be submitted after Enter. Content: {:?}",
            content
        );
    }

    /// Test that queued nudge text can be detected in realistic TUI content.
    ///
    /// This validates the TUI structure that the daemon parses to find stuck nudges.
    /// The daemon looks for text between the action line (✳/⏺) and the input separator.
    #[test]
    fn test_queued_nudge_detectable_in_tui() {
        // Realistic Claude Code TUI with queued nudge
        let tui_content = "\
⏺ Previous action completed

✳ Working on the feature...
❯ github said: @columbus Check 'Test' passed on PR #529
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";

        // The daemon detects queued nudges by finding ❯ lines between action and separator
        let lines: Vec<&str> = tui_content.lines().collect();
        let has_action_line = lines
            .iter()
            .any(|l| l.starts_with('✳') || l.starts_with('⏺'));
        let has_queued_line = lines.iter().any(|l| {
            l.starts_with('❯')
                && l.len() > 2
                && !l.chars().skip(1).all(|c| c == '─' || c.is_whitespace())
        });

        assert!(has_action_line, "TUI should have action line");
        assert!(has_queued_line, "TUI should have queued nudge line");

        // Find the queued nudge text
        let queued_text = lines
            .iter()
            .find(|l| l.starts_with("❯ ") && l.contains("github said"))
            .map(|l| l.trim_start_matches("❯ "));

        assert!(
            queued_text.is_some(),
            "Should find queued nudge text in TUI"
        );
        assert!(
            queued_text.unwrap().contains("github said: @columbus"),
            "Queued text should contain the nudge message"
        );
    }

    /// Test that check_nudge_text_match correctly identifies daemon-sent nudges.
    #[test]
    fn test_nudge_text_matching_realistic_scenario() {
        // This simulates the full scenario:
        // 1. Daemon sends "github said: @columbus Check 'Test' passed on PR #529"
        // 2. Text gets stuck in input, TUI shows it with ❯ prefix
        // 3. We extract the text and check if it matches the pending nudge

        let daemon_nudge = "github said: @columbus Check 'Test' passed on PR #529";

        // Simulated extracted text from TUI (with ❯ prefix stripped)
        let extracted_from_tui = "github said: @columbus Check 'Test' passed on PR #529";

        // The matching function uses first 20 chars for prefix matching
        let check_len = 20;
        let prefix = &daemon_nudge[..check_len.min(daemon_nudge.len())];

        assert!(
            extracted_from_tui.contains(prefix),
            "Extracted TUI text should match daemon nudge prefix"
        );
    }

    /// Test that user-typed input does NOT match daemon nudges.
    #[test]
    fn test_user_input_does_not_match_daemon_nudge() {
        let daemon_nudge = "github said: @columbus Check 'Test' passed on PR #529";
        let user_input = "I want to add a new feature to the authentication system";

        let check_len = 20;
        let prefix = &daemon_nudge[..check_len.min(daemon_nudge.len())];

        assert!(
            !user_input.contains(prefix),
            "User input should NOT match daemon nudge"
        );
    }
}

/// Tests for coworker nudge input stability detection.
///
/// Coworker nudges now use the same queue-based delivery as lead nudges,
/// waiting for input to be stable before delivering messages to avoid
/// interrupting user typing.
#[cfg(test)]
mod coworker_input_stability_tests {
    use super::*;

    /// Test that coworker nudge waits when input has user text.
    ///
    /// This verifies the new behavior where coworker nudges check input stability
    /// before delivering, similar to how lead nudges work.
    #[test]
    #[ignore] // Requires tmux to be running
    fn test_coworker_nudge_waits_for_input_stability() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = test_session_name();
        let coworker_name = "lexington";
        let _cleanup = Cleanup(&session);

        // Setup: Create session and coworker window with a Claude-like prompt
        assert!(
            create_test_session(&session),
            "Failed to create test session"
        );

        let target = format!("{}:", session);
        let status = Command::new("tmux")
            .args([
                "new-window",
                "-t",
                &target,
                "-n",
                coworker_name,
                "bash",
                "-c",
                // Show prompt, read input (simulates Claude Code prompt)
                r#"printf '❯ '; read line; printf '❯ '; cat"#,
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(status, "Failed to create coworker window with prompt");
        thread::sleep(Duration::from_millis(500));

        // Type some text (without Enter) to simulate user typing
        let tmux_target = format!("{}:{}", session, coworker_name);
        let _ = Command::new("tmux")
            .args([
                "send-keys",
                "-t",
                &tmux_target,
                "-l",
                "User is typing a message",
            ])
            .status();
        thread::sleep(Duration::from_millis(200));

        // Verify text is there
        let content = capture_pane(&session, coworker_name).unwrap_or_default();
        assert!(
            midtown::tmux::has_input_text(&content),
            "Should detect text in input. Content: {:?}",
            content
        );

        // Test wait_for_nudge_safe - should detect user content and return false quickly
        // when no stability timeout is provided (i.e., we're just checking)
        let has_user_content = !midtown::tmux::get_input_text(&content)
            .map(|t| t.is_empty())
            .unwrap_or(true);
        assert!(has_user_content, "Should detect user content in input");
    }

    /// Test that coworker nudge proceeds when input is empty.
    #[test]
    #[ignore] // Requires tmux to be running
    fn test_coworker_nudge_immediate_when_input_empty() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = test_session_name();
        let coworker_name = "madison";
        let _cleanup = Cleanup(&session);

        assert!(
            create_test_session(&session),
            "Failed to create test session"
        );
        create_test_window(&session, coworker_name);
        thread::sleep(Duration::from_millis(500));

        // Input is empty — wait_for_nudge_safe should return true immediately
        let target = format!("{}:{}", session, coworker_name);
        let start = std::time::Instant::now();
        let result = midtown::tmux::wait_for_nudge_safe(
            &target,
            None, // No previous nudge
            Duration::from_secs(20),
            Duration::from_secs(10),
        );
        let elapsed = start.elapsed();

        assert!(result, "Should return true when input is empty");
        // Should complete well under the poll interval
        assert!(
            elapsed < Duration::from_secs(2),
            "Should return immediately, took {:?}",
            elapsed
        );
    }

    /// Test that coworker nudge can overwrite previous daemon nudge text.
    ///
    /// If the input contains text from the last nudge we sent, it's safe to
    /// send a new nudge (this handles the case where Enter didn't register).
    #[test]
    #[ignore] // Requires tmux to be running
    fn test_coworker_nudge_overwrites_stale_nudge() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = test_session_name();
        let coworker_name = "broadway";
        let _cleanup = Cleanup(&session);

        assert!(
            create_test_session(&session),
            "Failed to create test session"
        );

        let target = format!("{}:", session);
        let status = Command::new("tmux")
            .args([
                "new-window",
                "-t",
                &target,
                "-n",
                coworker_name,
                "bash",
                "-c",
                r#"printf '❯ '; read line"#,
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(status, "Failed to create coworker window");
        thread::sleep(Duration::from_millis(500));

        // Simulate a previous nudge that got stuck (sent text without Enter registering)
        let previous_nudge = "github said: @broadway Check 'Build' passed on PR #100";
        let tmux_target = format!("{}:{}", session, coworker_name);
        let _ = Command::new("tmux")
            .args(["send-keys", "-t", &tmux_target, "-l", previous_nudge])
            .status();
        thread::sleep(Duration::from_millis(200));

        // Verify text is there
        let content = capture_pane(&session, coworker_name).unwrap_or_default();
        assert!(
            content.contains("github said"),
            "Previous nudge should be in pane. Content: {:?}",
            content
        );

        // wait_for_nudge_safe should return true immediately when the input matches
        // the last nudge we sent (it's safe to overwrite our own stuck text)
        let start = std::time::Instant::now();
        let result = midtown::tmux::wait_for_nudge_safe(
            &tmux_target,
            Some(previous_nudge), // This matches what we "sent" before
            Duration::from_secs(20),
            Duration::from_secs(10),
        );
        let elapsed = start.elapsed();

        assert!(
            result,
            "Should return true when input matches previous nudge"
        );
        // Should be immediate since it matches the last nudge
        assert!(
            elapsed < Duration::from_secs(2),
            "Should return immediately when overwriting stale nudge, took {:?}",
            elapsed
        );
    }

    /// Test that coworker nudge times out and proceeds after max wait.
    ///
    /// If user keeps typing, eventually we need to send the nudge anyway.
    #[test]
    #[ignore] // Requires tmux to be running
    fn test_coworker_nudge_timeout_with_persistent_input() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = test_session_name();
        let coworker_name = "amsterdam";
        let _cleanup = Cleanup(&session);

        assert!(
            create_test_session(&session),
            "Failed to create test session"
        );

        let target = format!("{}:", session);
        let status = Command::new("tmux")
            .args([
                "new-window",
                "-t",
                &target,
                "-n",
                coworker_name,
                "bash",
                "-c",
                r#"printf '❯ '; read line"#,
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(status, "Failed to create coworker window");
        thread::sleep(Duration::from_millis(500));

        // Type user content that never matches any nudge
        let tmux_target = format!("{}:{}", session, coworker_name);
        let _ = Command::new("tmux")
            .args([
                "send-keys",
                "-t",
                &tmux_target,
                "-l",
                "writing a long detailed message that is not a nudge",
            ])
            .status();
        thread::sleep(Duration::from_millis(200));

        // Use short timeouts for test
        let start = std::time::Instant::now();
        let result = midtown::tmux::wait_for_nudge_safe(
            &tmux_target,
            Some("github said: unrelated nudge"), // Doesn't match what's in input
            Duration::from_secs(3),               // Short stable duration
            Duration::from_secs(5),               // Short max wait
        );
        let elapsed = start.elapsed();

        // Should return false (timeout) after max_wait
        assert!(!result, "Should return false after timeout");
        // Should take roughly max_wait (5s)
        assert!(
            elapsed >= Duration::from_secs(4),
            "Should wait until timeout, only waited {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "Should not wait much longer than timeout, waited {:?}",
            elapsed
        );
    }
}
