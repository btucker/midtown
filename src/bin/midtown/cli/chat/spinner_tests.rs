//! Tests for time-based spinner animation.

use super::CHANNEL_LEAD_THINKING_TIMEOUT;
use super::CoworkerInfo;
use super::ToolActivityEntry;
use super::tests::test_app;
use std::time::Duration;

#[test]
fn test_tick_spinner_does_not_advance_immediately() {
    let mut app = test_app();
    let frame_before = app.spinner_char();
    app.tick_spinner();
    let frame_after = app.spinner_char();
    assert_eq!(
        frame_before, frame_after,
        "Spinner should not advance within 100ms of creation"
    );
}

#[test]
fn test_tick_spinner_advances_after_interval() {
    let mut app = test_app();
    let frame_before = app.spinner_char();
    // Sleep just over the 100ms interval
    std::thread::sleep(Duration::from_millis(110));
    app.tick_spinner();
    let frame_after = app.spinner_char();
    assert_ne!(
        frame_before, frame_after,
        "Spinner should advance after 100ms"
    );
}

#[test]
fn test_spinner_char_is_consistent_without_tick() {
    let app = test_app();
    let char1 = app.spinner_char();
    let char2 = app.spinner_char();
    assert_eq!(
        char1, char2,
        "spinner_char() should be deterministic without tick"
    );
}

#[test]
fn test_any_spinner_visible_false_when_idle() {
    let app = test_app();
    assert!(
        !app.any_spinner_visible(),
        "No spinners visible when lead not working and no coworkers"
    );
}

#[test]
fn test_any_spinner_visible_true_when_lead_working() {
    let mut app = test_app();
    app.lead_working = true;
    assert!(
        app.any_spinner_visible(),
        "Spinner should be visible when lead is working"
    );
}

#[test]
fn test_any_spinner_visible_true_when_active_coworker() {
    let mut app = test_app();
    app.coworkers = vec![CoworkerInfo {
        name: "lexington".to_string(),
        task_id: Some(42),
        phase: Some("dev".to_string()),
        pr_number: None,
        health: "green".to_string(),
        provider: "claude".to_string(),
        profile: "default".to_string(),
        progress: None,
        time_estimate: None,
    }];
    assert!(
        app.any_spinner_visible(),
        "Spinner should be visible when coworker is active"
    );
}

#[test]
fn test_any_spinner_visible_false_when_coworker_idle() {
    let mut app = test_app();
    app.coworkers = vec![CoworkerInfo {
        name: "lexington".to_string(),
        task_id: None,
        phase: Some("idle".to_string()),
        pr_number: None,
        health: "green".to_string(),
        provider: "claude".to_string(),
        profile: "default".to_string(),
        progress: None,
        time_estimate: None,
    }];
    assert!(
        !app.any_spinner_visible(),
        "Spinner should not be visible when all coworkers idle"
    );
}

#[test]
fn test_any_spinner_visible_true_with_in_progress_tool_entry() {
    // Regression: when lead_working is false but a tool entry is still in-progress,
    // the spinner should still be visible so the animation frame keeps advancing.
    // Without this, the spinner freezes on its last frame.
    let mut app = test_app();
    app.lead_working = false; // lead_working is false (stale RPC data)
    app.tool_activity = std::collections::HashMap::from([(
        "lead".to_string(),
        vec![ToolActivityEntry {
            header: "\u{203a} Read foo.rs".to_string(), // › = in-progress
            completed_at: None,
        }],
    )]);
    assert!(
        app.any_spinner_visible(),
        "Spinner should be visible when there are in-progress tool entries, even if lead_working is false"
    );
}

#[test]
fn test_any_spinner_visible_true_when_channel_thinking() {
    // When a user submits a message to a topic channel, optimistic thinking state
    // is set immediately and should make the spinner visible.
    let mut app = test_app();
    app.set_channel_lead_thinking("myproject");
    assert!(
        app.any_spinner_visible(),
        "Spinner should be visible when channel_lead_thinking is active"
    );
}

#[test]
fn test_any_spinner_visible_false_when_channel_thinking_expired() {
    // Thinking state expires after CHANNEL_LEAD_THINKING_TIMEOUT seconds.
    // An expired entry should not make the spinner visible.
    let mut app = test_app();
    let expired =
        std::time::Instant::now() - CHANNEL_LEAD_THINKING_TIMEOUT - Duration::from_secs(1);
    app.channel_lead_thinking
        .insert("myproject".to_string(), expired);
    assert!(
        !app.any_spinner_visible(),
        "Spinner should not be visible when channel_lead_thinking has expired"
    );
}
