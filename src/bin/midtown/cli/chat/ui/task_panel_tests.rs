use super::super::super::app::tests::test_app;
use super::super::super::app::{FocusedPane, KanbanTask, TaskStatus};
use super::draw_task_panel;

// ── draw_task_panel: placeholder when task not found ─────────────────────────

/// When open_task_id references a task that doesn't exist, the panel renders
/// a "Task not found" placeholder without panicking.
#[test]
fn test_draw_task_panel_task_not_found() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = test_app();
    app.open_task_id = Some("999".to_string());
    // tasks list is empty — task won't be found

    let backend = TestBackend::new(40, 20);
    let mut terminal = Terminal::new(backend).unwrap();

    // Should not panic
    terminal
        .draw(|f| {
            let area = f.area();
            draw_task_panel(f, &app, area);
        })
        .unwrap();

    let rendered = terminal.backend().buffer().clone();
    let content: String = rendered
        .content()
        .iter()
        .map(|c| c.symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        content.contains("Task not found") || content.contains("999"),
        "Should render placeholder for unknown task, got: {:?}",
        &content[..content.len().min(200)]
    );
}

// ── draw_task_panel: renders task metadata ────────────────────────────────────

/// Task metadata (subject, status, owner, channel) is rendered in the panel.
#[test]
fn test_draw_task_panel_renders_metadata() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = test_app();
    app.tasks = vec![KanbanTask {
        id: "42".to_string(),
        subject: "Fix the bug".to_string(),
        description: Some("This is a description.".to_string()),
        owner: Some("york".to_string()),
        status: TaskStatus::InProgress,
        modified_at: None,
        channel: Some("main".to_string()),
        blocked_by: vec![],
    }];
    app.open_task_id = Some("42".to_string());

    let backend = TestBackend::new(50, 20);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = f.area();
            draw_task_panel(f, &app, area);
        })
        .unwrap();

    let rendered = terminal.backend().buffer().clone();
    let content: String = rendered
        .content()
        .iter()
        .map(|c| c.symbol().chars().next().unwrap_or(' '))
        .collect();

    assert!(
        content.contains("Fix the bug"),
        "Should render task subject, got: {:?}",
        &content[..content.len().min(200)]
    );
    assert!(
        content.contains("york"),
        "Should render task owner, got: {:?}",
        &content[..content.len().min(200)]
    );
    assert!(
        content.contains("in_progress"),
        "Should render task status, got: {:?}",
        &content[..content.len().min(200)]
    );
}

// ── draw_task_panel: metadata visible when description is long ────────────────

/// When the description is long, the top metadata rows (Status, Owner, etc.)
/// remain visible — the panel shows from the top, not the bottom.
#[test]
fn test_draw_task_panel_metadata_visible_with_long_description() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let long_description = "word ".repeat(200); // ~200 words, forces many wrapped lines
    let mut app = test_app();
    app.tasks = vec![KanbanTask {
        id: "10".to_string(),
        subject: "Long task".to_string(),
        description: Some(long_description),
        owner: Some("lexington".to_string()),
        status: TaskStatus::Pending,
        modified_at: None,
        channel: None,
        blocked_by: vec![],
    }];
    app.open_task_id = Some("10".to_string());

    let backend = TestBackend::new(40, 10); // narrow, short terminal
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = f.area();
            draw_task_panel(f, &app, area);
        })
        .unwrap();

    let rendered = terminal.backend().buffer().clone();
    let content: String = rendered
        .content()
        .iter()
        .map(|c| c.symbol().chars().next().unwrap_or(' '))
        .collect();

    // Status metadata should be visible at the top despite description overflow
    assert!(
        content.contains("pending") || content.contains("Status"),
        "Status metadata should be visible with long description, got: {:?}",
        &content[..content.len().min(400)]
    );
}

// ── open_task / close_task lifecycle ─────────────────────────────────────────

/// open_task sets open_task_id.
#[test]
fn test_open_task_sets_id() {
    let mut app = test_app();
    app.open_task("123");
    assert_eq!(app.open_task_id, Some("123".to_string()));
}

/// close_task clears open_task_id and resets focused_pane to InputBar.
/// Regression test: double-Esc no longer exits TUI (was caused by focused_pane
/// remaining as Board after close_task).
#[test]
fn test_close_task_clears_id_and_resets_focus() {
    let mut app = test_app();
    app.open_task_id = Some("123".to_string());
    app.focused_pane = FocusedPane::Board;

    app.close_task();

    assert!(app.open_task_id.is_none(), "open_task_id should be cleared");
    assert_eq!(
        app.focused_pane,
        FocusedPane::InputBar,
        "focused_pane should reset to InputBar after close_task"
    );
}

/// open_task resets focused_pane to InputBar when thread was focused,
/// preventing keystrokes from being routed to the invisible thread buffer.
#[test]
fn test_open_task_resets_focus_when_thread_was_focused() {
    let mut app = test_app();
    app.focused_pane = FocusedPane::Thread;
    app.thread_parent_id = Some("parent-msg".to_string());

    app.open_task("42");

    assert_eq!(
        app.focused_pane,
        FocusedPane::InputBar,
        "open_task should reset focused_pane to InputBar when thread was focused"
    );
    assert!(
        app.thread_parent_id.is_none(),
        "open_task should close thread"
    );
}

/// open_task preserves focused_pane when the user is not in Thread focus.
#[test]
fn test_open_task_preserves_focus_when_not_thread() {
    let mut app = test_app();
    app.focused_pane = FocusedPane::Board;
    app.open_task("42");

    assert_eq!(
        app.focused_pane,
        FocusedPane::Board,
        "open_task should not change focused_pane when not in Thread"
    );
}

/// Opening a task closes any open thread (mutual exclusion).
#[test]
fn test_open_task_closes_thread() {
    let mut app = test_app();
    app.thread_parent_id = Some("msg-1".to_string());
    app.thread_input_text = "draft reply".to_string();

    app.open_task("55");

    assert!(
        app.thread_parent_id.is_none(),
        "open_task should clear thread_parent_id"
    );
    assert!(
        app.thread_input_text.is_empty(),
        "open_task should clear thread_input_text"
    );
    assert_eq!(app.open_task_id, Some("55".to_string()));
}

// ── open_task_as_thread lifecycle ─────────────────────────────────────────────

/// open_task_as_thread sets thread_parent_id to the message_id and thread_task_id
/// to the task_id, without requiring the message to exist in app.messages.
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
        app.thread_task_id,
        Some("42".to_string()),
        "thread_task_id should be set to task_id"
    );
    assert_eq!(
        app.focused_pane,
        FocusedPane::Thread,
        "focused_pane should be Thread after open_task_as_thread"
    );
}

/// open_task_as_thread closes the static task panel (mutual exclusion).
#[test]
fn test_open_task_as_thread_closes_task_panel() {
    let mut app = test_app();
    app.open_task_id = Some("10".to_string());

    app.open_task_as_thread("42", "msg-uuid-123");

    assert!(
        app.open_task_id.is_none(),
        "open_task_as_thread should clear open_task_id"
    );
}

/// open_task_as_thread works even when the creation message is not in app.messages.
/// (Unlike open_thread, which returns early if the parent message isn't found.)
#[test]
fn test_open_task_as_thread_does_not_require_parent_message() {
    let mut app = test_app();
    // No messages loaded — message_id won't be found
    assert!(app.messages.is_empty());

    app.open_task_as_thread("99", "nonexistent-uuid");

    // Should succeed and set thread state
    assert_eq!(app.thread_parent_id, Some("nonexistent-uuid".to_string()));
    assert_eq!(app.thread_task_id, Some("99".to_string()));
}

/// open_task_as_thread collects thread replies from loaded messages.
#[test]
fn test_open_task_as_thread_collects_replies() {
    let mut app = test_app();
    let message_id = "task-creation-msg".to_string();

    // Add a reply in the thread
    let mut reply = midtown::Message::text("user", "What's the progress?");
    reply.thread_parent_id = Some(message_id.clone());
    app.messages.push_back(reply.clone());

    // Add an unrelated message
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

/// close_thread clears thread_task_id.
#[test]
fn test_close_thread_clears_thread_task_id() {
    let mut app = test_app();
    app.thread_parent_id = Some("msg-1".to_string());
    app.thread_task_id = Some("42".to_string());

    app.close_thread();

    assert!(
        app.thread_task_id.is_none(),
        "close_thread should clear thread_task_id"
    );
    assert!(
        app.thread_parent_id.is_none(),
        "close_thread should clear thread_parent_id"
    );
}

/// open_thread (regular message thread) clears thread_task_id.
#[test]
fn test_open_thread_clears_thread_task_id() {
    let mut app = test_app();
    // Set up state as if a task thread was open
    app.thread_task_id = Some("42".to_string());

    // Add a parent message so open_thread can find it
    let parent = midtown::Message::text("alice", "Hello");
    let parent_id = parent.id.clone();
    app.messages.push_back(parent);

    app.open_thread(&parent_id);

    assert!(
        app.thread_task_id.is_none(),
        "open_thread should clear thread_task_id"
    );
    assert_eq!(app.thread_parent_id, Some(parent_id));
}
