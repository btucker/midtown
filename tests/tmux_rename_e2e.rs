//! End-to-end tests for tmux window rename handling.
//!
//! Verifies that `list_windows()` strips status suffixes from renamed windows
//! so that `sync_with_tmux()` and `discover_existing()` can match coworkers
//! by their base name after the window has been renamed.
//!
//! Run with `cargo test -- --ignored tmux_rename` as these require tmux.

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
    format!("midtown-rename-test-{}-{}", std::process::id(), counter)
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .args(["list-sessions"])
        .output()
        .map(|o| o.status.success() || o.status.code() == Some(1))
        .unwrap_or(false)
}

fn create_test_session(session: &str) -> bool {
    Command::new("tmux")
        .args(["new-session", "-d", "-s", session])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn create_test_window(session: &str, window: &str) -> bool {
    Command::new("tmux")
        .args(["new-window", "-t", session, "-n", window])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn rename_tmux_window(session: &str, old_name: &str, new_name: &str) -> bool {
    let target = format!("{}:{}", session, old_name);
    Command::new("tmux")
        .args(["rename-window", "-t", &target, new_name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn kill_test_session(session: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}

#[test]
#[timeout(30000)]
#[ignore] // Requires tmux to be running
fn test_list_windows_strips_status_suffix() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    assert!(
        create_test_session(&session),
        "Failed to create tmux session"
    );

    // Create a coworker window named "york"
    assert!(
        create_test_window(&session, "york"),
        "Failed to create window"
    );
    thread::sleep(Duration::from_millis(100));

    // Rename the window to simulate a status suffix: "york" -> "york:done#204"
    assert!(
        rename_tmux_window(&session, "york", "york:done#204"),
        "Failed to rename window"
    );
    thread::sleep(Duration::from_millis(100));

    // list_windows should return the base name "york", not "york:done#204"
    let windows = midtown::tmux::list_windows(&session).expect("list_windows failed");

    kill_test_session(&session);

    assert!(
        windows.contains(&"york".to_string()),
        "Expected list_windows to return base name 'york', got: {:?}",
        windows
    );
    assert!(
        !windows.contains(&"york:done#204".to_string()),
        "list_windows should NOT return the suffixed name, got: {:?}",
        windows
    );
}

#[test]
#[timeout(30000)]
#[ignore] // Requires tmux to be running
fn test_list_windows_deduplicates_after_stripping() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    assert!(
        create_test_session(&session),
        "Failed to create tmux session"
    );

    // Create a window and rename it with suffix
    assert!(
        create_test_window(&session, "amsterdam"),
        "Failed to create window"
    );
    thread::sleep(Duration::from_millis(100));

    rename_tmux_window(&session, "amsterdam", "amsterdam:dev#5");
    thread::sleep(Duration::from_millis(100));

    // list_windows should return just "amsterdam" (no suffix, no duplicates)
    let windows = midtown::tmux::list_windows(&session).expect("list_windows failed");

    kill_test_session(&session);

    let amsterdam_count = windows.iter().filter(|w| w.as_str() == "amsterdam").count();
    assert_eq!(
        amsterdam_count, 1,
        "Expected exactly one 'amsterdam', got {} in {:?}",
        amsterdam_count, windows
    );
}

#[test]
#[timeout(30000)]
#[ignore] // Requires tmux to be running
fn test_rename_window_works_after_previous_rename() {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return;
    }

    let session = test_session_name();
    assert!(
        create_test_session(&session),
        "Failed to create tmux session"
    );
    assert!(
        create_test_window(&session, "york"),
        "Failed to create window"
    );
    thread::sleep(Duration::from_millis(100));

    // First rename: york -> york:dev#5
    midtown::tmux::rename_window(&session, "york", Some("developing task 5"))
        .expect("First rename failed");
    thread::sleep(Duration::from_millis(100));

    // Verify the window was renamed
    let raw_output = Command::new("tmux")
        .args(["list-windows", "-t", &session, "-F", "#{window_name}"])
        .output()
        .expect("list-windows failed");
    let raw_names: Vec<String> = String::from_utf8_lossy(&raw_output.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect();
    assert!(
        raw_names.iter().any(|n| n.starts_with("york:")),
        "Expected a renamed window starting with 'york:', got: {:?}",
        raw_names
    );

    // Second rename: should still find the window and rename it
    midtown::tmux::rename_window(&session, "york", Some("testing task 5"))
        .expect("Second rename failed");
    thread::sleep(Duration::from_millis(100));

    // Verify the second rename took effect
    let raw_output2 = Command::new("tmux")
        .args(["list-windows", "-t", &session, "-F", "#{window_name}"])
        .output()
        .expect("list-windows failed");
    let raw_names2: Vec<String> = String::from_utf8_lossy(&raw_output2.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect();

    kill_test_session(&session);

    // The window should have the new status suffix
    assert!(
        raw_names2.iter().any(|n| n.contains("test")),
        "Expected window with 'test' status after second rename, got: {:?}",
        raw_names2
    );
}
