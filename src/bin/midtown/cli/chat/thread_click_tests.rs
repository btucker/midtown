use std::collections::VecDeque;

use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::EventResult;
use super::app;
use super::app::FocusedPane;
use super::app::tests::test_app;
use super::handle_event;
use super::ui;

/// Helper to create a left mouse click event at given terminal coordinates.
fn mouse_click(column: u16, row: u16) -> Event {
    use crossterm::event::MouseButton;
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn test_click_reply_indicator_opens_thread() {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();

    let parent = midtown::Message::text("park", "Top-level message");
    let parent_id = parent.id.clone();
    let mut reply = midtown::Message::text("lexington", "Reply in thread");
    reply.thread_parent_id = Some(parent_id);

    app.messages = VecDeque::from(vec![parent, reply]);
    app.selected_channel = "midtown".to_string();

    // Render once so click maps are populated.
    terminal
        .draw(|f| {
            ui::draw(f, &mut app);
        })
        .unwrap();

    assert!(
        !app.thread_reply_line_map.is_empty(),
        "thread_reply_line_map should be populated after render"
    );

    let (line, mapped_parent_id) = app
        .thread_reply_line_map
        .iter()
        .next()
        .map(|(line, id)| (*line, id.clone()))
        .expect("expected at least one clickable reply indicator line");
    let chat_area = app
        .chat_messages_area
        .expect("chat_messages_area should be populated after render");

    let click_x = chat_area.x + 8;
    let click_y = chat_area.y + 1 + line;
    let result = handle_event(&mut app, mouse_click(click_x, click_y));
    assert!(matches!(result, EventResult::Continue));

    assert_eq!(app.thread_parent_id, Some(mapped_parent_id));
    assert_eq!(app.focused_pane, app::FocusedPane::Thread);
    assert_eq!(app.thread_messages.len(), 1);
}

/// Bug #1: Thread reply messages must not appear in the main channel chat area.
///
/// When a message has `thread_parent_id` set, it is a reply in a thread and
/// should only be visible in the thread panel — not in the main channel view.
#[test]
fn test_thread_replies_not_rendered_in_main_channel() {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();

    let parent = midtown::Message::text("park", "Parent message content");
    let parent_id = parent.id.clone();
    let mut reply = midtown::Message::text("lexington", "THREAD_REPLY_UNIQUE_MARKER");
    reply.thread_parent_id = Some(parent_id.clone());

    app.messages = VecDeque::from(vec![parent, reply]);
    app.selected_channel = "midtown".to_string();

    terminal
        .draw(|f| {
            ui::draw(f, &mut app);
        })
        .unwrap();

    let chat_area = app
        .chat_messages_area
        .expect("chat_messages_area should be set after render");

    // Scan the chat messages area for the thread reply marker.
    let buf = terminal.backend().buffer();
    let mut reply_found_in_chat = false;
    for row in chat_area.y..chat_area.y + chat_area.height {
        for col in chat_area.x..chat_area.x + chat_area.width {
            let cell = buf.cell((col, row)).map(|c| c.symbol()).unwrap_or("");
            if cell.contains('T') {
                // Build the line content and check if the marker appears
                let line: String = (chat_area.x..chat_area.x + chat_area.width)
                    .filter_map(|c| buf.cell((c, row)).map(|cell| cell.symbol().to_string()))
                    .collect();
                if line.contains("THREAD_REPLY_UNIQUE_MARKER") {
                    reply_found_in_chat = true;
                    break;
                }
            }
        }
    }

    assert!(
        !reply_found_in_chat,
        "Thread reply content must not appear in the main channel chat area"
    );
}

/// Bug #2: Clicking on the thread input area must focus the Thread pane.
///
/// When the thread panel is open, clicking in the thread input box should
/// set focused_pane to FocusedPane::Thread, just like clicking the main
/// input bar focuses FocusedPane::InputBar.
#[test]
fn test_click_thread_input_focuses_thread_pane() {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();

    let parent = midtown::Message::text("park", "Parent message");
    let parent_id = parent.id.clone();
    app.messages = VecDeque::from(vec![parent]);
    app.open_thread(&parent_id);

    // Switch focus away from Thread so we can verify the click restores it.
    app.focused_pane = FocusedPane::InputBar;

    // Render once to populate thread_input_area.
    terminal
        .draw(|f| {
            ui::draw(f, &mut app);
        })
        .unwrap();

    let thread_input_area = app
        .thread_input_area
        .expect("thread_input_area should be set after render when thread is open");

    // Click in the middle of the thread input area.
    let click_x = thread_input_area.x + thread_input_area.width / 2;
    let click_y = thread_input_area.y + thread_input_area.height / 2;
    let result = handle_event(&mut app, mouse_click(click_x, click_y));

    assert!(matches!(result, EventResult::Continue));
    assert_eq!(
        app.focused_pane,
        FocusedPane::Thread,
        "Clicking the thread input area should focus the Thread pane"
    );
}
