//! Regression tests for the /channel create command.

use super::app::FocusedPane;
use super::app::tests::test_app;
use super::tests::key_press;
use super::{EventResult, handle_event};
use crossterm::event::KeyCode;

/// Regression test: create_channel must not perform filesystem I/O in test mode.
///
/// Previously, create_channel() called Channel::new() with real project paths
/// even when test_mode was true. This caused tests to create directories like
/// ~/.midtown/projects/midtown/channels/test-channel/ which the daemon then
/// treated as legitimate topic channels and spawned channel leads for.
#[test]
fn test_channel_create_does_not_write_to_filesystem_in_test_mode() {
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    assert!(!app.load_channel_messages_called);

    // Execute /channel create — this triggers create_channel()
    app.input_text = "/channel create test-no-disk-write".to_string();
    app.input_cursor = app.input_text.len();
    let result = handle_event(&mut app, key_press(KeyCode::Enter));

    assert!(matches!(result, EventResult::Continue));
    assert_eq!(app.selected_channel, "test-no-disk-write");
    // create_channel must NOT have triggered load_channel_messages in test mode.
    // If it did, it would write to the real project directory on disk.
    assert!(
        !app.load_channel_messages_called,
        "create_channel must not call load_channel_messages() in test mode to avoid filesystem writes"
    );
}
