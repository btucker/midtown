// task_panel.rs has been removed — task detail display is now handled by thread.rs.
// This file retains lifecycle tests for open_task_as_thread and thread_task_ids that
// were previously testing the task-panel/thread integration.

use super::super::super::app::FocusedPane;
use super::super::super::app::tests::test_app;

// ── open_task_as_thread lifecycle ─────────────────────────────────────────────

/// open_task_as_thread sets thread_parent_id to the message_id and thread_task_ids
/// to contain the task_id, without requiring the message to exist in app.messages.
#[test]
fn test_open_task_as_thread_sets_ids() {
    let mut app = test_app();
    app.open_task_as_thread("42", "msg-uuid-123");

    assert_eq!(
        app.thread_parent_id,
        Some("msg-uuid-123".to_string()),
        "thread_parent_id should be set to message_id"
    );
    assert_eq!(
        app.thread_task_ids,
        vec!["42".to_string()],
        "thread_task_ids should contain the task_id"
    );
    assert_eq!(
        app.focused_pane,
        FocusedPane::Thread,
        "focused_pane should be Thread after open_task_as_thread"
    );
}

/// open_task_as_thread works even when the creation message is not in app.messages.
#[test]
fn test_open_task_as_thread_does_not_require_parent_message() {
    let mut app = test_app();
    assert!(app.messages.is_empty());

    app.open_task_as_thread("99", "nonexistent-uuid");

    assert_eq!(app.thread_parent_id, Some("nonexistent-uuid".to_string()));
    assert_eq!(app.thread_task_ids, vec!["99".to_string()]);
}

/// open_task_as_thread collects thread replies from loaded messages.
#[test]
fn test_open_task_as_thread_collects_replies() {
    let mut app = test_app();
    let message_id = "task-creation-msg".to_string();

    let mut reply = midtown::Message::text("user", "What's the progress?");
    reply.thread_parent_id = Some(message_id.clone());
    app.messages.push_back(reply.clone());

    app.messages
        .push_back(midtown::Message::text("user", "Hello"));

    app.open_task_as_thread("42", &message_id);

    assert_eq!(
        app.thread_messages.len(),
        1,
        "should collect only the thread reply"
    );
    assert_eq!(app.thread_messages[0].id, reply.id);
}

/// close_thread clears thread_task_ids and thread_parent_id.
#[test]
fn test_close_thread_clears_thread_task_ids() {
    let mut app = test_app();
    app.thread_parent_id = Some("msg-1".to_string());
    app.thread_task_ids = vec!["42".to_string()];

    app.close_thread();

    assert!(
        app.thread_task_ids.is_empty(),
        "close_thread should clear thread_task_ids"
    );
    assert!(
        app.thread_parent_id.is_none(),
        "close_thread should clear thread_parent_id"
    );
}

/// open_thread (regular message thread) clears thread_task_ids.
#[test]
fn test_open_thread_clears_thread_task_ids() {
    let mut app = test_app();
    app.thread_task_ids = vec!["42".to_string()];

    let parent = midtown::Message::text("alice", "Hello");
    let parent_id = parent.id.clone();
    app.messages.push_back(parent);

    app.open_thread(&parent_id);

    assert!(
        app.thread_task_ids.is_empty(),
        "open_thread should clear thread_task_ids"
    );
    assert_eq!(app.thread_parent_id, Some(parent_id));
}

/// open_task_without_message opens the thread panel without a parent message.
#[test]
fn test_open_task_without_message() {
    let mut app = test_app();
    app.open_task_without_message("55");

    assert_eq!(
        app.thread_task_ids,
        vec!["55".to_string()],
        "thread_task_ids should contain the task_id"
    );
    assert!(
        app.thread_parent_id.is_none(),
        "thread_parent_id should be None when task has no creation message"
    );
    assert_eq!(
        app.focused_pane,
        FocusedPane::Thread,
        "focused_pane should be Thread"
    );
    assert!(
        app.is_thread_panel_open(),
        "thread panel should be open even without parent message"
    );
}

/// is_thread_panel_open returns true when thread_parent_id is set.
#[test]
fn test_is_thread_panel_open_with_parent_id() {
    let mut app = test_app();
    assert!(!app.is_thread_panel_open());

    app.thread_parent_id = Some("msg-1".to_string());
    assert!(app.is_thread_panel_open());
}

/// is_thread_panel_open returns true when thread_task_ids is non-empty (no parent).
#[test]
fn test_is_thread_panel_open_with_task_ids_only() {
    let mut app = test_app();
    assert!(!app.is_thread_panel_open());

    app.thread_task_ids = vec!["42".to_string()];
    assert!(app.is_thread_panel_open());
}
