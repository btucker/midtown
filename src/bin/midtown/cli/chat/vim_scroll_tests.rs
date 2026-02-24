//! Tests for vim-style keyboard scroll bindings and Ctrl+U/Ctrl+D half-page scroll.
//!
//! Covers:
//! - j/k/g/G bindings when Chat pane is focused (should scroll, not insert)
//! - j/k/g/G bindings when InputBar is focused (should insert, not scroll)
//! - j/k when a draft exists in InputBar but focus is elsewhere (should insert into draft)
//! - Ctrl+U / Ctrl+D half-page scroll when input is empty
//! - Ctrl+U / Ctrl+D text editing when InputBar has content
//! - Ctrl+D / Ctrl+U blocked by channel_switcher overlay

use super::*;
use app::FocusedPane;
use app::tests::test_app;
use crossterm::event::{KeyEvent, KeyModifiers};
use midtown::Message;

fn ctrl_key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::CONTROL))
}

fn shift_key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::SHIFT))
}

// ---------------------------------------------------------------------------
// Vim-style scroll bindings — Chat pane focused
// ---------------------------------------------------------------------------

#[test]
fn test_vim_j_scrolls_down_when_unfocused() {
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.focused_pane = FocusedPane::Chat;
    app.scroll_offset = 10;

    handle_event(&mut app, key_press(KeyCode::Char('j')));
    assert!(
        app.scroll_offset < 10,
        "j should scroll down (decrease offset)"
    );
    assert_eq!(
        app.focused_pane,
        FocusedPane::Chat,
        "j should not focus InputBar"
    );
    assert_eq!(app.input_text, "", "j should not insert into input");
}

#[test]
fn test_vim_k_scrolls_up_when_unfocused() {
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.focused_pane = FocusedPane::Chat;
    app.scroll_offset = 0;

    handle_event(&mut app, key_press(KeyCode::Char('k')));
    assert!(
        app.scroll_offset > 0,
        "k should scroll up (increase offset)"
    );
    assert_eq!(
        app.focused_pane,
        FocusedPane::Chat,
        "k should not focus InputBar"
    );
    assert_eq!(app.input_text, "", "k should not insert into input");
}

#[test]
fn test_vim_g_scrolls_to_top_when_unfocused() {
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.focused_pane = FocusedPane::Chat;
    app.scroll_offset = 0;

    handle_event(&mut app, key_press(KeyCode::Char('g')));
    assert!(app.scroll_offset > 0, "g should scroll to top (offset > 0)");
    assert_eq!(
        app.focused_pane,
        FocusedPane::Chat,
        "g should not focus InputBar"
    );
}

#[test]
#[allow(non_snake_case)]
fn test_vim_G_scrolls_to_bottom_when_unfocused() {
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.focused_pane = FocusedPane::Chat;
    app.scroll_offset = 20;
    // 'G' with kitty protocol comes as Char('G') via the shift transform
    handle_event(&mut app, shift_key(KeyCode::Char('g')));
    assert_eq!(app.scroll_offset, 0, "G should scroll to bottom");
    assert_eq!(
        app.focused_pane,
        FocusedPane::Chat,
        "G should not focus InputBar"
    );
}

// ---------------------------------------------------------------------------
// Vim-style bindings — InputBar focused (should insert, never scroll)
// ---------------------------------------------------------------------------

#[test]
fn test_vim_j_inserts_when_inputbar_focused_and_empty() {
    // Regression: pressing 'j' with InputBar focused and empty input
    // used to scroll instead of inserting — causing "got it" → "ot it".
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = String::new();
    app.scroll_offset = 10;

    handle_event(&mut app, key_press(KeyCode::Char('j')));
    assert_eq!(
        app.input_text, "j",
        "j with InputBar focused should insert 'j', not scroll"
    );
    assert_eq!(
        app.scroll_offset, 10,
        "scroll_offset must not change when InputBar is focused"
    );
}

#[test]
fn test_vim_k_inserts_when_inputbar_focused_and_empty() {
    // "k sounds good" should not lose the 'k'.
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = String::new();
    app.scroll_offset = 5;

    handle_event(&mut app, key_press(KeyCode::Char('k')));
    assert_eq!(
        app.input_text, "k",
        "k with InputBar focused should insert 'k', not scroll"
    );
    assert_eq!(app.scroll_offset, 5, "scroll_offset must not change");
}

#[test]
fn test_vim_g_inserts_when_inputbar_focused_and_empty() {
    // "got it" should not scroll to top when InputBar is focused.
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = String::new();
    app.scroll_offset = 5;

    handle_event(&mut app, key_press(KeyCode::Char('g')));
    assert_eq!(
        app.input_text, "g",
        "g with InputBar focused should insert 'g', not scroll to top"
    );
    assert_eq!(app.scroll_offset, 5, "scroll_offset must not change");
}

#[test]
fn test_vim_j_inserts_when_input_has_text() {
    // When InputBar has content, j must NOT scroll — it should insert.
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello".to_string();
    app.input_cursor = 5;

    handle_event(&mut app, key_press(KeyCode::Char('j')));
    assert_eq!(
        app.input_text, "helloj",
        "j with non-empty input should insert into the text"
    );
}

#[test]
fn test_vim_j_inserts_when_chat_focused_but_draft_exists() {
    // Regression: a draft in InputBar must be preserved even when Chat has focus.
    // The `input_text.is_empty()` guard ensures scrolling is only gated on text
    // presence, not on which pane has focus.
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.focused_pane = FocusedPane::Chat; // focus shifted away from InputBar
    app.input_text = "draft message".to_string(); // but draft exists
    app.input_cursor = 13;
    app.scroll_offset = 5;

    handle_event(&mut app, key_press(KeyCode::Char('j')));
    // Should NOT scroll — should insert 'j' into the draft
    assert_eq!(
        app.input_text, "draft messagej",
        "j with existing draft should insert even when focus is on Chat"
    );
    assert_eq!(
        app.scroll_offset, 5,
        "scroll_offset must not change when a draft exists"
    );
}

// ---------------------------------------------------------------------------
// Ctrl+U / Ctrl+D half-page scroll when input is empty
// ---------------------------------------------------------------------------

#[test]
fn test_ctrl_u_half_page_up_when_input_empty() {
    let mut app = test_app();
    for i in 0..60 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = String::new();
    app.visible_height = 20;
    app.scroll_offset = 0;

    handle_event(&mut app, ctrl_key(KeyCode::Char('u')));
    let expected = 20 / 2; // half_page = visible_height / 2
    assert_eq!(
        app.scroll_offset, expected,
        "Ctrl+U with empty input should scroll half-page up"
    );
    assert_eq!(
        app.input_text, "",
        "Ctrl+U should not modify empty input text"
    );
}

#[test]
fn test_ctrl_u_kills_text_when_input_has_content() {
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 5;

    handle_event(&mut app, ctrl_key(KeyCode::Char('u')));
    assert_eq!(
        app.input_text, " world",
        "Ctrl+U should kill to beginning when input has text"
    );
}

#[test]
fn test_ctrl_d_half_page_down_when_input_empty() {
    let mut app = test_app();
    for i in 0..60 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = String::new();
    app.visible_height = 20;
    app.scroll_offset = 20;

    handle_event(&mut app, ctrl_key(KeyCode::Char('d')));
    let expected = 20 - 20 / 2; // 20 - half_page
    assert_eq!(
        app.scroll_offset, expected,
        "Ctrl+D with empty input should scroll half-page down"
    );
}

#[test]
fn test_ctrl_d_deletes_char_when_input_has_content() {
    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello".to_string();
    app.input_cursor = 0;

    handle_event(&mut app, ctrl_key(KeyCode::Char('d')));
    assert_eq!(
        app.input_text, "ello",
        "Ctrl+D should delete char under cursor when input has text"
    );
}

// ---------------------------------------------------------------------------
// Ctrl+D / Ctrl+U blocked by channel_switcher overlay
// ---------------------------------------------------------------------------

#[test]
fn test_ctrl_d_does_not_scroll_when_channel_switcher_open() {
    // Regression: Ctrl+D lacked the !channel_switcher.show guard that Ctrl+U had.
    // Pressing Ctrl+D with the channel switcher open was scrolling chat behind the overlay.
    let mut app = test_app();
    for i in 0..60 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = String::new();
    app.visible_height = 20;
    app.scroll_offset = 20;
    app.channel_switcher.show = true; // overlay is open

    handle_event(&mut app, ctrl_key(KeyCode::Char('d')));
    assert_eq!(
        app.scroll_offset, 20,
        "Ctrl+D must not scroll when channel switcher is open"
    );
}

#[test]
fn test_ctrl_u_does_not_scroll_when_channel_switcher_open() {
    // Symmetric check: Ctrl+U also must not scroll behind the overlay.
    let mut app = test_app();
    for i in 0..60 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = String::new();
    app.visible_height = 20;
    app.scroll_offset = 20;
    app.channel_switcher.show = true;

    handle_event(&mut app, ctrl_key(KeyCode::Char('u')));
    assert_eq!(
        app.scroll_offset, 20,
        "Ctrl+U must not scroll when channel switcher is open"
    );
}

// ---------------------------------------------------------------------------
// Helper — duplicated from mod.rs tests module so external file compiles
// ---------------------------------------------------------------------------

fn key_press(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}
