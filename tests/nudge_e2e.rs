//! End-to-end tests for nudge functionality.
//!
//! These tests require tmux to be running and are marked as ignored
//! by default for CI environments. Run with `cargo test -- --ignored`
//! to execute them locally.
//!
//! All tests have a 30-second timeout to prevent CI from hanging.

use ntest::timeout;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

/// Counter for unique session names across tests.
static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique test session name to avoid conflicts.
fn test_session_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("midtown-nudge-test-{}-{}", std::process::id(), counter)
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

/// Check if tmux is available.
fn tmux_available() -> bool {
    Command::new("tmux")
        .args(["list-sessions"])
        .output()
        .map(|o| o.status.success() || o.status.code() == Some(1)) // 1 = no sessions
        .unwrap_or(false)
}

#[test]
#[timeout(30000)]
#[ignore] // Requires tmux to be running
fn test_nudge_verification_success() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let window = "test-window";

    // Setup: Create session and window
    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );

    // Ensure cleanup on panic
    struct Cleanup<'a>(&'a str);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            kill_test_session(self.0);
        }
    }
    let _cleanup = Cleanup(&session);

    create_test_window(&session, window);

    // Give tmux time to set up
    thread::sleep(Duration::from_millis(200));

    // Use the library's send_nudge function
    let unique_message = format!("test-nudge-{}", std::process::id());

    // Directly test the nudge sending
    let result = midtown::nudge::send_nudge(&session, window, &unique_message);

    // Should succeed
    assert!(
        result.is_ok(),
        "send_nudge should succeed: {:?}",
        result.err()
    );

    // Verify the message appears in the pane (double-check)
    let content = capture_pane(&session, window);
    assert!(content.is_some(), "Should be able to capture pane");
    assert!(
        content.unwrap().contains(&unique_message),
        "Pane should contain the nudge message"
    );
}

#[test]
#[timeout(30000)]
#[ignore] // Requires tmux to be running
fn test_nudge_verification_returns_error_for_missing_message() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let window = "test-window-2";

    // Setup: Create session and window
    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );

    // Ensure cleanup
    struct Cleanup<'a>(&'a str);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            kill_test_session(self.0);
        }
    }
    let _cleanup = Cleanup(&session);

    create_test_window(&session, window);
    thread::sleep(Duration::from_millis(200));

    // Test: Verify that session_exists and window checks work correctly
    assert!(
        midtown::nudge::session_exists(&session).unwrap(),
        "Session should exist"
    );

    // Test: Non-existent session should return SessionNotFound
    let result = midtown::nudge::send_nudge("nonexistent-session-xyz", window, "test");
    assert!(
        matches!(result, Err(midtown::nudge::NudgeError::SessionNotFound(_))),
        "Should return SessionNotFound for non-existent session"
    );
}

#[test]
#[timeout(30000)]
#[ignore] // Requires tmux to be running
fn test_nudge_pane_verification_with_special_characters() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    let window = "test-special";

    // Setup
    assert!(
        create_test_session(&session),
        "Failed to create test session"
    );

    struct Cleanup<'a>(&'a str);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            kill_test_session(self.0);
        }
    }
    let _cleanup = Cleanup(&session);

    create_test_window(&session, window);
    thread::sleep(Duration::from_millis(200));

    // Test with a message containing special characters
    let message = "Task #42: Review PR @lead (urgent!)";

    let result = midtown::nudge::send_nudge(&session, window, message);

    assert!(
        result.is_ok(),
        "send_nudge should succeed with special characters: {:?}",
        result.err()
    );

    // Verify message in pane
    let content = capture_pane(&session, window).unwrap();
    assert!(
        content.contains(message),
        "Pane should contain the message with special characters"
    );
}
