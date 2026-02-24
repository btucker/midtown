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
fn test_any_spinner_visible_true_when_coworker_status_change_is_active() {
    let mut app = test_app();
    app.coworker_pulse_frames.insert("lexington".to_string(), 2);
    assert!(
        app.any_spinner_visible(),
        "Spinner should be visible when a coworker status change is pulsing"
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
fn test_pulse_bold_at_frame_zero_is_bold() {
    // Frame 0: (0 / 5).is_multiple_of(2) → 0.is_multiple_of(2) → true → BOLD
    let app = test_app();
    assert!(app.pulse_bold(), "Frame 0 should be bold phase");
}

#[test]
fn test_pulse_bold_at_frame_ten_is_normal() {
    // Frame 10: (10 / 5).is_multiple_of(2) → 2.is_multiple_of(2) → true → BOLD
    // Frame 5: (5 / 5).is_multiple_of(2) → 1.is_multiple_of(2) → false → normal
    let mut app = test_app();
    app.spinner_frame = 5;
    assert!(!app.pulse_bold(), "Frame 5 should be normal phase");
}

#[test]
fn test_pulse_name_style_bold_branch() {
    use ratatui::style::{Color, Modifier};
    // Frame 0 → pulse_bold = true → style has BOLD modifier
    let app = test_app();
    let style = app.pulse_name_style(Color::Yellow);
    assert_eq!(style.fg, Some(Color::Yellow));
    assert!(
        style.add_modifier.contains(Modifier::BOLD),
        "Frame 0 should produce BOLD style"
    );
}

#[test]
fn test_pulse_name_style_normal_branch() {
    use ratatui::style::{Color, Modifier};
    // Frame 5 → pulse_bold = false → style has no BOLD modifier
    let mut app = test_app();
    app.spinner_frame = 5;
    let style = app.pulse_name_style(Color::Yellow);
    assert_eq!(style.fg, Some(Color::Yellow));
    assert!(
        !style.add_modifier.contains(Modifier::BOLD),
        "Frame 5 should produce normal (non-bold) style"
    );
}

#[test]
fn test_coworker_name_style_fades_over_time() {
    use ratatui::style::{Color, Modifier};
    let mut app = test_app();

    app.spinner_frame = 0;
    let dim_style = app.coworker_name_style(Color::Yellow, 0, true);
    assert!(
        dim_style.add_modifier.contains(Modifier::DIM),
        "Coworker names should start dimmed"
    );

    app.spinner_frame = 2;
    let normal_style = app.coworker_name_style(Color::Yellow, 0, true);
    assert!(
        !normal_style.add_modifier.contains(Modifier::DIM)
            && !normal_style.add_modifier.contains(Modifier::BOLD),
        "Coworker names should pass through normal during fade middle"
    );

    app.spinner_frame = 5;
    let bold_style = app.coworker_name_style(Color::Yellow, 0, true);
    assert!(
        bold_style.add_modifier.contains(Modifier::BOLD),
        "Coworker names should reach bold at wave peak"
    );
    assert_eq!(normal_style.fg, bold_style.fg);
}

#[test]
fn test_coworker_name_style_is_waved_by_row_index() {
    let mut app = test_app();
    app.spinner_frame = 0;
    let row0 = app.coworker_name_style(ratatui::style::Color::Yellow, 0, true);

    app.spinner_frame = 2;
    let row1_delayed = app.coworker_name_style(ratatui::style::Color::Yellow, 1, true);
    assert_eq!(
        row0, row1_delayed,
        "Row index should advance the same animation by 2-frame steps"
    );
}

#[test]
fn test_coworker_name_style_does_not_animate_without_status_change() {
    use ratatui::style::{Color, Modifier};
    let app = test_app();
    let static_style = app.coworker_name_style(Color::Yellow, 0, false);

    assert_eq!(static_style.fg, Some(Color::Yellow));
    assert!(
        !static_style
            .add_modifier
            .contains(Modifier::DIM | Modifier::BOLD),
        "Coworker names should stay static when no change is active"
    );
}

#[test]
fn test_any_spinner_visible_false_without_active_coworker_change() {
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
        !app.any_spinner_visible(),
        "Coworker list changes only should not keep spinner active without an explicit pulse"
    );
}

#[test]
fn test_update_coworker_status_marks_pulse_on_status_line_change() {
    let mut app = test_app();
    let mut first = CoworkerInfo {
        name: "park".to_string(),
        task_id: Some(1),
        phase: Some("dev".to_string()),
        pr_number: None,
        health: "green".to_string(),
        provider: "claude".to_string(),
        profile: "default".to_string(),
        progress: Some(10),
        time_estimate: None,
    };
    app.update_coworker_status(vec![first.clone()]);
    assert!(
        !app.is_coworker_name_pulsing("park"),
        "First coworker status snapshot should not pulse"
    );

    first.phase = Some("test".to_string());
    app.update_coworker_status(vec![first]);
    assert!(
        app.is_coworker_name_pulsing("park"),
        "Status-line changes should trigger a pulse for that coworker"
    );
}

#[test]
fn test_update_coworker_status_drops_pulse_after_ticks() {
    let mut app = test_app();
    let coworker = CoworkerInfo {
        name: "york".to_string(),
        task_id: Some(2),
        phase: Some("dev".to_string()),
        pr_number: None,
        health: "green".to_string(),
        provider: "claude".to_string(),
        profile: "default".to_string(),
        progress: None,
        time_estimate: None,
    };

    app.update_coworker_status(vec![CoworkerInfo {
        name: "york".to_string(),
        task_id: Some(2),
        phase: Some("idle".to_string()),
        pr_number: None,
        health: "green".to_string(),
        provider: "claude".to_string(),
        profile: "default".to_string(),
        progress: None,
        time_estimate: None,
    }]);
    app.update_coworker_status(vec![coworker]);
    assert!(app.is_coworker_name_pulsing("york"));

    for _ in 0..super::App::COWORKER_PULSE_CYCLE_FRAMES {
        app.advance_coworker_pulse_frames();
    }

    assert!(
        !app.is_coworker_name_pulsing("york"),
        "Pulse should expire after the configured cycle"
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
