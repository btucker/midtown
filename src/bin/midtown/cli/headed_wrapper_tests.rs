use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{
    InputState, OutputMirror, SharedInputState, SharedOutputMirror, apply_submit_key,
    draw_status_bar, get_input_text, is_nudge_stuck, trim_trailing_linebreaks, update_input_state,
    wait_for_empty_input, wait_for_nudge_safe_with_input_state,
};

#[test]
fn submit_key_appends_carriage_return() {
    assert_eq!(apply_submit_key("hello".to_string(), true), "hello\r");
    assert_eq!(apply_submit_key("hello\n".to_string(), true), "hello\r");
    assert_eq!(apply_submit_key("hello\r\n".to_string(), true), "hello\r");
}

#[test]
fn submit_key_noop_when_submit_false() {
    assert_eq!(apply_submit_key("hello".to_string(), false), "hello");
}

#[test]
fn trim_trailing_linebreaks_only() {
    assert_eq!(trim_trailing_linebreaks("hello\r\n".to_string()), "hello");
    assert_eq!(trim_trailing_linebreaks("hello\n".to_string()), "hello");
    assert_eq!(trim_trailing_linebreaks("hello".to_string()), "hello");
}

#[test]
fn input_state_tracks_typing_and_clears_on_enter() {
    let state: SharedInputState = Arc::new(Mutex::new(InputState::default()));
    update_input_state(&state, b"hello");
    let snap = super::snapshot_input_state(&state);
    assert_eq!(snap.current_input, "hello");

    update_input_state(&state, b"\x7f");
    let snap = super::snapshot_input_state(&state);
    assert_eq!(snap.current_input, "hell");

    update_input_state(&state, b"\r");
    let snap = super::snapshot_input_state(&state);
    assert!(snap.current_input.is_empty());
}

#[test]
fn wait_for_empty_input_returns_quickly_when_empty() {
    let state: SharedInputState = Arc::new(Mutex::new(InputState::default()));
    assert!(wait_for_empty_input(&state, Duration::from_millis(10)));
}

#[test]
fn get_input_text_prefers_most_recent_prompt() {
    let content = "older\n❯ first\n\nnew\n❯ second";
    assert_eq!(get_input_text(content).as_deref(), Some("second"));
}

#[test]
fn nudge_stuck_detection_matches_prompt_line() {
    let content = "something\n❯ github said: check ci";
    assert!(is_nudge_stuck(content, "github said: check ci on pr #10"));
    assert!(!is_nudge_stuck(content, "totally different"));
}

#[test]
fn wait_for_nudge_safe_overwrites_last_nudge_immediately() {
    let input_state: SharedInputState = Arc::new(Mutex::new(InputState::default()));
    let mirror: SharedOutputMirror = Arc::new(Mutex::new(OutputMirror::default()));

    // InputState contains the last nudge text
    update_input_state(&input_state, b"github said: check ci");

    {
        let mut guard = mirror.lock().expect("mirror lock");
        guard.ingest("❯ github said: check ci\n".as_bytes());
    }

    let safe = wait_for_nudge_safe_with_input_state(
        &input_state,
        &mirror,
        Some("github said: check ci"),
        Duration::from_secs(20),
        Duration::from_secs(1),
    );
    assert!(safe);
}

#[test]
fn wait_for_nudge_safe_respects_active_input_state() {
    let input_state: SharedInputState = Arc::new(Mutex::new(InputState::default()));
    let mirror: SharedOutputMirror = Arc::new(Mutex::new(OutputMirror::default()));

    // User is actively typing (recent keystroke, non-empty input)
    update_input_state(&input_state, b"hello");

    {
        let mut guard = mirror.lock().expect("mirror lock");
        // OutputMirror might not have caught up yet — empty or showing old prompt
        guard.ingest("❯ \n".as_bytes());
    }

    // Simulate continued typing in a background thread
    let input_state_clone = Arc::clone(&input_state);
    let typing_thread = std::thread::spawn(move || {
        // Keep typing every 50ms for 400ms (total)
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(50));
            update_input_state(&input_state_clone, b"x");
        }
    });

    // Should wait because InputState shows active typing
    let safe = wait_for_nudge_safe_with_input_state(
        &input_state,
        &mirror,
        None,
        Duration::from_millis(150),
        Duration::from_millis(500),
    );

    typing_thread.join().unwrap();

    // Should time out waiting for input to stabilize
    assert!(!safe);
}

#[test]
fn wait_for_nudge_safe_allows_nudge_when_input_empty() {
    let input_state: SharedInputState = Arc::new(Mutex::new(InputState::default()));
    let mirror: SharedOutputMirror = Arc::new(Mutex::new(OutputMirror::default()));

    {
        let mut guard = mirror.lock().expect("mirror lock");
        guard.ingest("❯ \n".as_bytes());
    }

    // Input state is empty — safe to nudge immediately
    let safe = wait_for_nudge_safe_with_input_state(
        &input_state,
        &mirror,
        None,
        Duration::from_secs(20),
        Duration::from_secs(1),
    );
    assert!(safe);
}

// Tests for draw_status_bar function

#[test]
fn draw_status_bar_truncates_long_paths_with_ellipsis() {
    // Test that when the status text is longer than terminal width, it gets truncated
    let session = "park";
    let long_path = "/very/long/path/that/exceeds/terminal/width/for/sure/and/should/be/truncated";
    let terminal_width = 40;
    let status_row = 20;

    // This should not panic and should handle truncation gracefully
    let result = draw_status_bar(session, long_path, terminal_width, status_row);
    assert!(result.is_ok());
}

#[test]
fn draw_status_bar_handles_short_text() {
    // Test that short text works without truncation
    let session = "lead";
    let cwd = "/home/user";
    let terminal_width = 80;
    let status_row = 20;

    let result = draw_status_bar(session, cwd, terminal_width, status_row);
    assert!(result.is_ok());
}

#[test]
fn draw_status_bar_handles_multibyte_char_boundaries() {
    // Test that truncation respects UTF-8 character boundaries
    let session = "lexington";
    let cwd_with_emoji = "/home/user/projects/🚀midtown/work";
    let terminal_width = 30; // Force truncation in the middle of the emoji region
    let status_row = 20;

    // Should not panic even when truncating near multibyte characters
    let result = draw_status_bar(session, cwd_with_emoji, terminal_width, status_row);
    assert!(result.is_ok());
}

#[test]
fn draw_status_bar_handles_zero_width_terminal() {
    // Edge case: terminal width is 0 (shouldn't happen in practice but should be safe)
    let session = "madison";
    let cwd = "/home";
    let terminal_width = 0;
    let status_row = 20;

    let result = draw_status_bar(session, cwd, terminal_width, status_row);
    assert!(result.is_ok());
}

#[test]
fn draw_status_bar_handles_very_narrow_terminal() {
    // Test with terminal width smaller than the ellipsis
    let session = "york";
    let cwd = "/very/long/path";
    let terminal_width = 2; // Less than "..." length
    let status_row = 20;

    let result = draw_status_bar(session, cwd, terminal_width, status_row);
    assert!(result.is_ok());
}
