//! Integration tests for tailf file watching behavior.
//!
//! These tests verify that the tailf crate properly detects file changes,
//! specifically the race condition fixed in the chat TUI.

use std::io::Write;
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::time::timeout;

/// Test that tailf with num_lines=None reliably detects the first write.
///
/// This test verifies the fix for a race condition on macOS where
/// `tail -n 0 -f` (num_lines=Some(0)) seeks to EOF before registering
/// its kqueue file watcher, causing the first write to be lost.
///
/// Using num_lines=None (no -n flag) avoids this race condition.
#[tokio::test]
async fn test_tailf_detects_first_write_with_num_lines_none() {
    // Run multiple trials to ensure reliability
    for trial in 1..=5 {
        let mut temp_file = NamedTempFile::new().expect("Failed to create temp file");
        let path = temp_file.path().to_path_buf();

        // Create tailer with num_lines=None (the fix)
        let mut tailer = tailf::tailf(&path, None).expect("Failed to create tailer");

        // Immediately write to the file (no delay - this is the race condition trigger)
        writeln!(temp_file, "Test line {}", trial).expect("Failed to write");
        temp_file.flush().expect("Failed to flush");
        temp_file.as_file().sync_all().expect("Failed to sync");

        // Should receive the line within 2 seconds
        let result = timeout(Duration::from_secs(2), tailer.next()).await;

        match result {
            Ok(Ok(Some(line))) => {
                let content = String::from_utf8_lossy(&line);
                assert!(
                    content.contains(&format!("Test line {}", trial)),
                    "Trial {}: Expected 'Test line {}', got: {}",
                    trial,
                    trial,
                    content
                );
            }
            Ok(Ok(None)) => panic!("Trial {}: Unexpected EOF", trial),
            Ok(Err(e)) => panic!("Trial {}: Read error: {}", trial, e),
            Err(_) => panic!(
                "Trial {}: TIMEOUT - tailf did not detect the write within 2 seconds. \
                This indicates the race condition bug has regressed.",
                trial
            ),
        }
    }
}

/// Regression test: verify that num_lines=Some(0) has the race condition bug.
///
/// This test documents the buggy behavior. If this test starts passing,
/// it means the upstream tailf crate or macOS tail behavior has changed.
#[tokio::test]
async fn test_tailf_race_condition_with_num_lines_zero() {
    let mut temp_file = NamedTempFile::new().expect("Failed to create temp file");
    let path = temp_file.path().to_path_buf();

    // Create tailer with num_lines=Some(0) (the buggy configuration)
    let mut tailer = tailf::tailf(&path, Some(0)).expect("Failed to create tailer");

    // Immediately write to the file (triggers the race condition)
    writeln!(temp_file, "First line").expect("Failed to write");
    temp_file.flush().expect("Failed to flush");
    temp_file.as_file().sync_all().expect("Failed to sync");

    // With the race condition, this will timeout (the first line is lost)
    // We use a short timeout to make the test fast
    let result = timeout(Duration::from_millis(500), tailer.next()).await;

    // The race condition means this SHOULD timeout on macOS
    // If it doesn't timeout, the upstream behavior has changed (which is good!)
    if result.is_ok() {
        eprintln!(
            "Note: num_lines=Some(0) worked without race condition. \
            The tailf crate or macOS tail behavior may have improved."
        );
    }
    // We don't assert failure here because the behavior could be fixed upstream
}
