use std::collections::VecDeque;

use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::EventResult;
use super::app;
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
