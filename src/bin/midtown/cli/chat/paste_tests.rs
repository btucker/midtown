//! Tests for bracketed paste and Alt+Enter newline input.
//!
//! Covers paste event handling (text insertion, cursor positioning, line ending
//! normalization, channel switcher routing) and modifier+Enter newline insertion.

use super::app;
use super::app::FocusedPane;
use super::app::tests::test_app;
use crate::cli::chat::{EventResult, handle_event, try_read_clipboard_image};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

#[test]
fn test_paste_inserts_text_at_cursor() {
    let mut app = test_app();

    let event = Event::Paste("hello world".to_string());
    handle_event(&mut app, event);

    assert_eq!(app.input_text, "hello world");
    assert_eq!(app.input_cursor, 11);
}

#[test]
fn test_paste_preserves_newlines() {
    let mut app = test_app();

    let event = Event::Paste("line1\nline2\nline3".to_string());
    handle_event(&mut app, event);

    assert_eq!(app.input_text, "line1\nline2\nline3");
    assert_eq!(app.input_cursor, 17);
}

#[test]
fn test_paste_inserts_at_middle_of_existing_text() {
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "helo".to_string();
    app.input_cursor = 2; // cursor between 'he' and 'lo'

    let event = Event::Paste("l".to_string());
    handle_event(&mut app, event);

    assert_eq!(app.input_text, "hello");
    assert_eq!(app.input_cursor, 3);
}

#[test]
fn test_paste_auto_focuses_input_bar() {
    let mut app = test_app();
    app.focused_pane = FocusedPane::Chat;

    let event = Event::Paste("hello".to_string());
    handle_event(&mut app, event);

    assert_eq!(app.focused_pane, FocusedPane::InputBar);
}

#[test]
fn test_paste_empty_string_preserves_existing_state() {
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "existing".to_string();
    app.input_cursor = 3;

    let event = Event::Paste(String::new());
    handle_event(&mut app, event);

    assert_eq!(
        app.input_text, "existing",
        "Existing text should be preserved on empty paste"
    );
    assert_eq!(app.input_cursor, 3, "Cursor position should be unchanged");
}

#[test]
fn test_paste_normalizes_crlf_line_endings() {
    let mut app = test_app();

    let event = Event::Paste("line1\r\nline2\r\nline3".to_string());
    handle_event(&mut app, event);

    assert_eq!(
        app.input_text, "line1\nline2\nline3",
        "\\r\\n should be normalized to \\n"
    );
    assert!(
        !app.input_text.contains('\r'),
        "No \\r characters should remain"
    );
}

#[test]
fn test_paste_normalizes_bare_cr_line_endings() {
    let mut app = test_app();

    let event = Event::Paste("line1\rline2\rline3".to_string());
    handle_event(&mut app, event);

    assert_eq!(
        app.input_text, "line1\nline2\nline3",
        "Bare \\r should be normalized to \\n"
    );
}

#[test]
fn test_paste_routes_to_channel_switcher_when_open() {
    let mut app = test_app();
    app.channel_switcher.show = true;

    let event = Event::Paste("test".to_string());
    handle_event(&mut app, event);

    assert_eq!(
        app.channel_switcher.input, "test",
        "Pasted text should go to channel switcher filter"
    );
    assert_eq!(
        app.input_text, "",
        "Main input should remain empty when channel switcher is open"
    );
}

#[test]
fn test_alt_enter_inserts_newline() {
    let mut app = test_app();

    let event = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    handle_event(&mut app, event);

    assert_eq!(app.input_text, "\n");
    assert_eq!(app.input_cursor, 1);
}

#[test]
fn test_shift_enter_inserts_newline() {
    let mut app = test_app();

    let event = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    handle_event(&mut app, event);

    assert_eq!(app.input_text, "\n");
    assert_eq!(app.input_cursor, 1);
}

#[test]
fn test_try_read_clipboard_image_returns_ok() {
    // In CI/test environments, no clipboard image is available.
    // The function should return Ok(None) rather than panicking or returning Err.
    let result = try_read_clipboard_image();
    // Either Ok(None) (no image) or Ok(Some(_)) (image found) — never Err in normal flow
    assert!(
        result.is_ok(),
        "try_read_clipboard_image should not return Err: {:?}",
        result
    );
}

#[test]
fn test_alt_enter_with_autocomplete_shown() {
    // Alt+Enter should insert newline even if autocomplete is showing,
    // matching Shift+Enter precedence (see test_shift_enter_with_autocomplete_shown
    // in autocomplete_tests.rs)
    let mut app = test_app();
    app.input_text = "@p".to_string();
    app.input_cursor = 2;
    app.autocomplete.show = true;
    app.autocomplete.selected_index = 0;

    let event = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    let result = handle_event(&mut app, event);

    assert!(matches!(result, EventResult::Continue));
    assert_eq!(
        app.input_text, "@p\n",
        "Alt+Enter should insert newline, not select autocomplete"
    );
    assert_eq!(app.input_cursor, 3);
}

#[test]
fn test_enter_with_pending_image_clears_pending_image() {
    // When pending_image is set and user presses Enter,
    // pending_image should be cleared (delivery attempted) even if daemon isn't running.
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.pending_image = Some(app::PendingImageInfo {
        dimensions: (100, 100),
        media_type: "image/png".to_string(),
    });

    // Press Enter - send_image_to_lead() will fail (no daemon in test), but
    // pending_image must still be cleared.
    let event = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let _ = handle_event(&mut app, event);

    assert!(
        app.pending_image.is_none(),
        "pending_image should be cleared after Enter"
    );
}

#[test]
fn test_ctrl_v_does_nothing_when_no_image_in_clipboard() {
    // In test/CI environments, no clipboard image is available.
    // Ctrl+V should be a no-op (no error, no pending_image set).
    let mut app = test_app();
    let event = Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
    let result = handle_event(&mut app, event);

    assert!(matches!(result, EventResult::Continue));
    // pending_image remains None since no image in clipboard
    assert!(app.pending_image.is_none());
}

#[test]
fn test_ctrl_v_sets_pending_image_and_esc_clears_it() {
    // Test that when pending_image is set, Esc clears it.
    let mut app = test_app();

    // Manually simulate what Ctrl+V would do if an image was found:
    app.pending_image = Some(app::PendingImageInfo {
        dimensions: (100, 100),
        media_type: "image/png".to_string(),
    });
    assert!(app.pending_image.is_some());

    // Esc should clear pending_image
    let event = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let _ = handle_event(&mut app, event);
    assert!(
        app.pending_image.is_none(),
        "Esc should clear pending_image"
    );
}

#[test]
fn test_pending_image_not_affected_by_regular_paste() {
    // Regular text paste should not affect pending_image state.
    let mut app = test_app();
    app.pending_image = Some(app::PendingImageInfo {
        dimensions: (200, 100),
        media_type: "image/png".to_string(),
    });

    let event = Event::Paste("some text".to_string());
    let _ = handle_event(&mut app, event);

    // Text paste should not clear pending_image
    assert!(
        app.pending_image.is_some(),
        "Text paste should not clear pending_image"
    );
    assert_eq!(app.input_text, "some text");
}

#[test]
fn test_pending_image_cleared_by_esc_before_input_clear() {
    // When both pending_image and input_text are set, first Esc should clear
    // pending_image (not input_text), acting as a first-step cancel.
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "some text".to_string();
    app.pending_image = Some(app::PendingImageInfo {
        dimensions: (100, 100),
        media_type: "image/png".to_string(),
    });

    let event = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let _ = handle_event(&mut app, event);

    // First Esc should clear pending_image but leave input_text
    assert!(
        app.pending_image.is_none(),
        "First Esc should clear pending_image"
    );
}
