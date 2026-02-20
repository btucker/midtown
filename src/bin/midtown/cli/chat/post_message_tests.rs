//! Tests for message posting and immediate display after Enter.

use crossterm::event::Event;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use midtown::{Channel, Message};
use tempfile::TempDir;

use super::app::tests::test_app_with_channel;
use super::handle_event;

fn key_press(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn test_message_appears_immediately_after_enter() {
    // After pressing Enter to post a message, the message should appear in
    // app.messages immediately — without waiting for tailf or the 1-second timer.
    // Bug: the event handler didn't call refresh() after post_message(), so the
    // message only appeared when the next tailf event or 1-second timer fired.
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "test-channel").unwrap();
    let mut app = test_app_with_channel(channel);
    app.focused_pane = super::app::FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = app.input_text.chars().count();

    // First refresh establishes cursor position (no messages yet)
    app.refresh();
    assert_eq!(app.messages.len(), 0, "No messages before posting");

    // Press Enter to post
    handle_event(&mut app, key_press(KeyCode::Enter));

    // Message should be visible immediately — no tailf wait needed
    assert_eq!(
        app.messages.len(),
        1,
        "Message should appear immediately after Enter, not waiting for tailf"
    );
    assert_eq!(app.messages[0].content, "hello world");
    assert!(
        app.input_text.is_empty(),
        "Input should be cleared after posting"
    );
}

#[test]
fn test_thread_reply_appears_immediately_after_enter() {
    // Same fix for thread replies: after pressing Enter in the thread input,
    // the reply should appear in thread_messages immediately.
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "test-channel").unwrap();

    // Write a parent message before creating the app
    let parent = Message::text("alice", "parent message");
    let parent_id = parent.id.clone();
    channel.send(&parent).unwrap();

    let mut app = test_app_with_channel(channel);

    app.refresh();
    assert_eq!(app.messages.len(), 1);

    app.thread_parent_id = Some(parent_id);
    app.focused_pane = super::app::FocusedPane::Thread;
    app.thread_input_text = "thread reply".to_string();
    app.thread_input_cursor = app.thread_input_text.chars().count();

    // Press Enter to post the thread reply
    handle_event(&mut app, key_press(KeyCode::Enter));

    // Thread reply should appear immediately
    assert_eq!(
        app.thread_messages.len(),
        1,
        "Thread reply should appear immediately after Enter"
    );
    assert_eq!(app.thread_messages[0].content, "thread reply");
    assert!(
        app.thread_input_text.is_empty(),
        "Thread input should be cleared after posting"
    );
}
