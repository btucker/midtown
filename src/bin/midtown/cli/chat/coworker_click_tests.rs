use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::EventResult;
use super::app::CoworkerInfo;
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

fn make_active_coworker(name: &str, phase: &str) -> CoworkerInfo {
    CoworkerInfo {
        name: name.to_string(),
        task_id: None,
        phase: Some(phase.to_string()),
        pr_number: None,
        health: "green".to_string(),
        provider: "claude".to_string(),
        profile: "default".to_string(),
        progress: None,
        time_estimate: None,
    }
}

/// Clicking on a coworker row within the sidebar x-bounds returns AttachCoworker.
#[test]
fn test_click_coworker_row_returns_attach() {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.coworkers = vec![
        make_active_coworker("park", "dev"),
        make_active_coworker("york", "test"),
    ];

    // Render so coworker_line_map and board_area are populated.
    terminal
        .draw(|f| {
            ui::draw(f, &mut app);
        })
        .unwrap();

    assert!(
        !app.coworker_line_map.is_empty(),
        "coworker_line_map should be populated after render"
    );

    let board_rect = app
        .board_area
        .expect("board_area should be populated after render");

    // Click on the first coworker row at a valid x inside the board area.
    let (&cw_y, cw_name) = app
        .coworker_line_map
        .iter()
        .find(|(_, name)| name.as_str() == "park")
        .expect("park should be in coworker_line_map");
    let cw_name = cw_name.clone(); // release borrow before handle_event

    let click_x = board_rect.x + 2; // inside the sidebar
    let result = handle_event(&mut app, mouse_click(click_x, cw_y));
    assert!(
        matches!(result, EventResult::AttachCoworker(ref name) if *name == cw_name),
        "clicking coworker row should return AttachCoworker"
    );
}

/// Clicking at the same y-coordinate as a coworker row but outside the sidebar
/// x-bounds must NOT trigger AttachCoworker. This guards against unbounded
/// click regions that would capture clicks in the chat panel.
#[test]
fn test_click_coworker_y_outside_sidebar_x_does_not_attach() {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.coworkers = vec![make_active_coworker("park", "dev")];

    terminal
        .draw(|f| {
            ui::draw(f, &mut app);
        })
        .unwrap();

    let board_rect = app
        .board_area
        .expect("board_area should be populated after render");

    let (&cw_y, _) = app
        .coworker_line_map
        .iter()
        .next()
        .expect("coworker_line_map should have an entry");

    // Click at the coworker's y but far to the right of the sidebar (in the chat area).
    let click_x = board_rect.x + board_rect.width + 10;
    let result = handle_event(&mut app, mouse_click(click_x, cw_y));
    assert!(
        !matches!(result, EventResult::AttachCoworker(_)),
        "click outside sidebar x-bounds should NOT trigger AttachCoworker"
    );
}
