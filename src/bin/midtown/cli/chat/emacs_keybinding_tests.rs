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

// Kill/delete operations call detect_autocomplete_trigger() so autocomplete
// state stays consistent when text is removed beneath a visible dropdown.
#[test]
fn test_ctrl_k_dismisses_autocomplete() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.autocomplete.show = true;
    app.input_text = "hello".to_string();
    app.input_cursor = 0;

    handle_event(&mut app, ctrl_key(KeyCode::Char('k')));
    assert!(!app.autocomplete.show);
}

#[test]
fn test_ctrl_u_dismisses_autocomplete() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.autocomplete.show = true;
    app.input_text = "hello".to_string();
    app.input_cursor = 5;

    handle_event(&mut app, ctrl_key(KeyCode::Char('u')));
    assert!(!app.autocomplete.show);
}

#[test]
fn test_ctrl_w_dismisses_autocomplete() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.autocomplete.show = true;
    app.input_text = "hello world".to_string();
    app.input_cursor = 11;

    handle_event(&mut app, ctrl_key(KeyCode::Char('w')));
    assert!(!app.autocomplete.show);
}

#[test]
fn test_ctrl_d_dismisses_autocomplete() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.autocomplete.show = true;
    app.input_text = "hello".to_string();
    app.input_cursor = 0;

    handle_event(&mut app, ctrl_key(KeyCode::Char('d')));
    assert!(!app.autocomplete.show);
}

// Consecutive kill operations append/prepend to the kill ring so that Ctrl+Y
// can yank all killed text at once (emacs kill ring semantics).
// Forward kills (Ctrl+K) append; backward kills (Ctrl+U, Ctrl+W) prepend.
#[test]
fn test_consecutive_ctrl_k_appends_to_kill_ring() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world foo".to_string();
    app.input_cursor = 5; // after "hello"

    // First Ctrl+K kills " world foo"
    handle_event(&mut app, ctrl_key(KeyCode::Char('k')));
    assert_eq!(app.kill_ring, Some(" world foo".to_string()));
    assert_eq!(app.input_text, "hello");

    // Position cursor mid-line, second Ctrl+K should append (but input is now "hello")
    // Instead test with fresh text: reset and do two kills in sequence
    app.input_text = "aaa bbb".to_string();
    app.input_cursor = 0;
    handle_event(&mut app, ctrl_key(KeyCode::Char('k'))); // kills "aaa bbb"
    assert_eq!(app.kill_ring, Some(" world fooaaa bbb".to_string())); // appended
}

#[test]
fn test_backward_kill_prepends_to_kill_ring() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world end".to_string();
    app.input_cursor = 11; // after "hello world"

    // First: Ctrl+K kills "end" (forward kill)
    handle_event(&mut app, ctrl_key(KeyCode::Char('k')));
    assert_eq!(app.kill_ring, Some(" end".to_string()));
    assert_eq!(app.input_text, "hello world");

    // Second: Ctrl+U kills "hello world" (backward kill) — should prepend
    handle_event(&mut app, ctrl_key(KeyCode::Char('u')));
    assert_eq!(app.kill_ring, Some("hello world end".to_string())); // prepended
    assert_eq!(app.input_text, "");
}

#[test]
fn test_backward_word_kill_prepends_to_kill_ring() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "aaa bbb ccc".to_string();
    app.input_cursor = 7; // after "aaa bbb"

    // First: Ctrl+K kills " ccc" (forward)
    handle_event(&mut app, ctrl_key(KeyCode::Char('k')));
    assert_eq!(app.kill_ring, Some(" ccc".to_string()));

    // Second: Ctrl+W kills "bbb" (backward word) — should prepend
    handle_event(&mut app, ctrl_key(KeyCode::Char('w')));
    assert_eq!(app.kill_ring, Some("bbb ccc".to_string())); // "bbb" prepended before " ccc"
}

#[test]
fn test_noop_kill_preserves_kill_chain() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 5;

    // First kill: Ctrl+K kills " world"
    handle_event(&mut app, ctrl_key(KeyCode::Char('k')));
    assert_eq!(app.kill_ring, Some(" world".to_string()));
    assert_eq!(app.input_text, "hello");
    assert_eq!(app.input_cursor, 5); // at EOL

    // No-op Ctrl+K at end of line — should NOT break the chain
    handle_event(&mut app, ctrl_key(KeyCode::Char('k')));
    assert_eq!(app.kill_ring, Some(" world".to_string())); // unchanged

    // Move cursor to beginning and kill — should still append
    app.input_cursor = 0;
    handle_event(&mut app, ctrl_key(KeyCode::Char('k')));
    assert_eq!(app.kill_ring, Some(" worldhello".to_string())); // appended
}

#[test]
fn test_non_kill_resets_kill_ring_accumulation() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 5;

    // First kill
    handle_event(&mut app, ctrl_key(KeyCode::Char('k')));
    assert_eq!(app.kill_ring, Some(" world".to_string()));

    // Non-kill command (Ctrl+A — move to beginning)
    handle_event(&mut app, ctrl_key(KeyCode::Char('a')));

    // Second kill after a non-kill — should NOT append
    app.input_text = "hello new".to_string();
    app.input_cursor = 0;
    handle_event(&mut app, ctrl_key(KeyCode::Char('k')));
    assert_eq!(app.kill_ring, Some("hello new".to_string())); // replaced, not appended
}

// Ctrl+Y yanks the kill ring content at the cursor position.
#[test]
fn test_ctrl_y_yanks_kill_ring_at_cursor() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 5;

    // Kill " world"
    handle_event(&mut app, ctrl_key(KeyCode::Char('k')));
    assert_eq!(app.input_text, "hello");
    assert_eq!(app.kill_ring, Some(" world".to_string()));

    // Move cursor to beginning, yank
    app.input_cursor = 0;
    handle_event(&mut app, ctrl_key(KeyCode::Char('y')));
    assert_eq!(app.input_text, " worldhello");
    assert_eq!(app.input_cursor, 6); // cursor moves past yanked text
}

#[test]
fn test_ctrl_y_with_empty_kill_ring_is_no_op() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello".to_string();
    app.input_cursor = 3;
    app.kill_ring = None;

    handle_event(&mut app, ctrl_key(KeyCode::Char('y')));
    assert_eq!(app.input_text, "hello");
    assert_eq!(app.input_cursor, 3);
}

#[test]
fn test_ctrl_y_noop_when_channel_switcher_open() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.kill_ring = Some("yanked".to_string());
    app.input_text = "hello".to_string();
    app.input_cursor = 5;
    app.channel_switcher.show = true;

    handle_event(&mut app, ctrl_key(KeyCode::Char('y')));
    // Should NOT insert into main input when channel switcher is open
    assert_eq!(app.input_text, "hello");
    assert_eq!(app.input_cursor, 5);
}

#[test]
fn test_ctrl_u_noop_when_channel_switcher_open() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 5;
    app.channel_switcher.show = true;

    handle_event(&mut app, ctrl_key(KeyCode::Char('u')));
    // Should NOT kill from main input when channel switcher is open
    assert_eq!(app.input_text, "hello world");
    assert_eq!(app.input_cursor, 5);
    assert_eq!(app.kill_ring, None);
}

#[test]
fn test_ctrl_w_noop_when_channel_switcher_open() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 11;
    app.channel_switcher.show = true;

    handle_event(&mut app, ctrl_key(KeyCode::Char('w')));
    // Should NOT kill from main input when channel switcher is open
    assert_eq!(app.input_text, "hello world");
    assert_eq!(app.input_cursor, 11);
    assert_eq!(app.kill_ring, None);
}

#[test]
fn test_ctrl_y_preserves_text_around_cursor() {
    use app::FocusedPane;
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.kill_ring = Some("XYZ".to_string());
    app.input_text = "ab".to_string();
    app.input_cursor = 1; // between 'a' and 'b'

    handle_event(&mut app, ctrl_key(KeyCode::Char('y')));
    assert_eq!(app.input_text, "aXYZb");
    assert_eq!(app.input_cursor, 4); // after "aXYZ"
}
