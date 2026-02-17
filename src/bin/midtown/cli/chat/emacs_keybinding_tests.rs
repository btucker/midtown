//! Tests for emacs-style keybindings in the TUI chat input.

use super::*;
use app::tests::test_app;
use crossterm::event::{KeyEvent, KeyModifiers};

fn ctrl_key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::CONTROL))
}

fn alt_key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::ALT))
}

#[test]
fn test_ctrl_a_moves_to_beginning() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 5;

    handle_event(&mut app, ctrl_key(KeyCode::Char('a')));
    assert_eq!(app.input_cursor, 0);
}

#[test]
fn test_ctrl_e_moves_to_end() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 0;

    handle_event(&mut app, ctrl_key(KeyCode::Char('e')));
    assert_eq!(app.input_cursor, 11);
}

#[test]
fn test_ctrl_k_kills_to_end_when_input_focused() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 5;

    handle_event(&mut app, ctrl_key(KeyCode::Char('k')));
    assert_eq!(app.input_text, "hello");
    assert_eq!(app.input_cursor, 5);
    assert_eq!(app.kill_ring, Some(" world".to_string()));
}

#[test]
fn test_ctrl_k_toggles_channel_switcher_when_not_in_input() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::Chat;

    handle_event(&mut app, ctrl_key(KeyCode::Char('k')));
    // Channel switcher should be toggled (channel_switcher.show)
    assert!(app.channel_switcher.show);
}

#[test]
fn test_ctrl_u_kills_to_beginning() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 5;

    handle_event(&mut app, ctrl_key(KeyCode::Char('u')));
    assert_eq!(app.input_text, " world");
    assert_eq!(app.input_cursor, 0);
    assert_eq!(app.kill_ring, Some("hello".to_string()));
}

#[test]
fn test_ctrl_w_kills_previous_word() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 11;

    handle_event(&mut app, ctrl_key(KeyCode::Char('w')));
    assert_eq!(app.input_text, "hello ");
    assert_eq!(app.input_cursor, 6);
    assert_eq!(app.kill_ring, Some("world".to_string()));
}

#[test]
fn test_ctrl_b_moves_back_one_char() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello".to_string();
    app.input_cursor = 3;

    handle_event(&mut app, ctrl_key(KeyCode::Char('b')));
    assert_eq!(app.input_cursor, 2);
}

#[test]
fn test_ctrl_f_moves_forward_one_char() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello".to_string();
    app.input_cursor = 3;

    handle_event(&mut app, ctrl_key(KeyCode::Char('f')));
    assert_eq!(app.input_cursor, 4);
}

#[test]
fn test_ctrl_d_deletes_char_under_cursor() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello".to_string();
    app.input_cursor = 2;

    handle_event(&mut app, ctrl_key(KeyCode::Char('d')));
    assert_eq!(app.input_text, "helo");
    assert_eq!(app.input_cursor, 2);
}

#[test]
fn test_alt_b_moves_back_one_word() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 11;

    handle_event(&mut app, alt_key(KeyCode::Char('b')));
    assert_eq!(app.input_cursor, 6);
}

#[test]
fn test_alt_f_moves_forward_one_word() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 0;

    handle_event(&mut app, alt_key(KeyCode::Char('f')));
    assert_eq!(app.input_cursor, 5);
}

#[test]
fn test_alt_f_skips_whitespace_before_word() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 5; // on the space

    handle_event(&mut app, alt_key(KeyCode::Char('f')));
    assert_eq!(app.input_cursor, 11); // end of "world"
}
