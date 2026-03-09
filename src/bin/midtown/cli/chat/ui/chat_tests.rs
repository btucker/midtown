//! Tests for `draw_lead_indicator` rendering and input bar cursor behavior.

use super::super::super::app::tests::test_app;
use super::*;

#[test]
fn test_block_cursor_uses_palette_fg_not_palette_bg() {
    // The `█` (FULL BLOCK) end-of-text cursor must use palette.fg as its foreground color.
    // U+2588 FULL BLOCK fills the entire character cell with the *foreground* color,
    // so using palette.bg (dark in dark themes) makes the cursor invisible.
    // The cursor must be styled with fg=palette.fg so it appears light/white in dark themes.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let backend = TestBackend::new(40, 3);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hi".to_string();
    app.input_cursor = 2; // at end of "hi" — renders █

    let palette = app.theme.palette();

    terminal
        .draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 40,
                height: 3,
            };
            draw_input_bar(f, &app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    // border(x=0) + prompt "› "(x=1,2) + "hi"(x=3,4) + cursor(x=5), content row y=1
    let cursor_cell = buffer.cell((5, 1)).unwrap();

    assert_eq!(
        cursor_cell.symbol(),
        "█",
        "Cursor at end of text should show '█'"
    );
    assert_eq!(
        cursor_cell.fg, palette.fg,
        "Block cursor '█' must use palette.fg as foreground — '█' fills the entire cell with fg, \
         so using palette.bg (dark in dark themes) makes it invisible. Got fg={:?}, expected palette.fg={:?}",
        cursor_cell.fg, palette.fg
    );
    assert_ne!(
        cursor_cell.fg, palette.bg,
        "Block cursor must NOT use palette.bg as foreground — that makes '█' invisible in dark themes"
    );
}

#[test]
fn test_cursor_renders_over_character_not_before_it() {
    // When cursor is in the middle of text, it should overlay the character
    // at the cursor position (with highlighting), not insert '█' before it.
    // Bug: previously the cursor block was inserted BEFORE the character,
    // pushing subsequent characters one position to the right.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    let backend = TestBackend::new(40, 3);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hello".to_string();
    app.input_cursor = 2; // cursor on second 'l'

    terminal
        .draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 40,
                height: 3,
            };
            draw_input_bar(f, &app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    // Layout: x=1 (left border) + 2 (prompt "› ") = 3 for first char
    // Cursor at position 2 => column 3 + 2 = 5
    let cursor_cell = buffer.cell((5, 1)).unwrap();

    assert_eq!(
        cursor_cell.symbol(),
        "l",
        "Cursor position should show 'l' (the character under the cursor), not '█'. Got: {:?}",
        cursor_cell.symbol()
    );
    assert_ne!(
        cursor_cell.bg,
        Color::Reset,
        "Cursor position should have a background color to highlight the cursor"
    );
}

#[test]
fn test_cursor_at_end_renders_block() {
    // When cursor is at the end of text (past last char), a standalone '█' should appear.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    let backend = TestBackend::new(40, 3);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.focused_pane = FocusedPane::InputBar;
    app.input_text = "hi".to_string();
    app.input_cursor = 2; // past end of "hi"

    terminal
        .draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 40,
                height: 3,
            };
            draw_input_bar(f, &app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    // "hi" occupies columns 3, 4; cursor block at column 5
    let cursor_cell = buffer.cell((5, 1)).unwrap();

    assert_eq!(
        cursor_cell.symbol(),
        "█",
        "Cursor at end of text should show '█'. Got: {:?}",
        cursor_cell.symbol()
    );
    assert_ne!(
        cursor_cell.bg,
        Color::Reset,
        "End-of-text cursor block should have a background color"
    );
}

fn buffer_row(buffer: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
    (0..width)
        .map(|x| {
            buffer
                .cell((x, y))
                .map(|c| c.symbol().to_string())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn test_lead_indicator_height_with_optimistic_thinking() {
    // When channel_lead_thinking is active but no tool entries exist yet,
    // lead_indicator_height should return 1 to reserve space for the spinner.
    let mut app = test_app();
    app.selected_channel = "myproject".to_string();
    app.set_channel_lead_thinking("myproject");
    assert_eq!(
        lead_indicator_height(&app),
        1,
        "Should return 1 when channel is thinking but no tool entries exist"
    );
}

#[test]
fn test_lead_indicator_height_idle_returns_one() {
    // Even when idle (no entries, no thinking), the indicator height must be 1.
    // The stable status area never collapses to zero to prevent message jumping.
    use super::super::super::app::CHANNEL_LEAD_THINKING_TIMEOUT;
    let mut app = test_app();
    app.selected_channel = "myproject".to_string();

    // Expired thinking — now fully idle
    let expired = std::time::Instant::now()
        - CHANNEL_LEAD_THINKING_TIMEOUT
        - std::time::Duration::from_secs(1);
    app.channel_lead_thinking
        .insert("myproject".to_string(), expired);
    assert_eq!(
        lead_indicator_height(&app),
        1,
        "Should return 1 (stable placeholder) even when thinking has expired"
    );
}

#[test]
fn test_lead_indicator_height_completely_idle_returns_one() {
    // With no entries and no thinking state at all, height should still be 1.
    let mut app = test_app();
    app.selected_channel = "main".to_string();
    assert_eq!(
        lead_indicator_height(&app),
        1,
        "Should always return at least 1 for stable status area"
    );
}

#[test]
fn test_draw_lead_indicator_shows_agent_name_when_channel_thinking() {
    // When no tool entries exist but channel_lead_thinking is active,
    // draw_lead_indicator should show the spinner and agent name.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let backend = TestBackend::new(80, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.selected_channel = "myproject".to_string();
    app.set_channel_lead_thinking("myproject");

    terminal
        .draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 1,
            };
            draw_lead_indicator(f, &mut app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let row = buffer_row(buffer, 0, 80);

    assert!(
        row.contains("myproject"),
        "Agent name 'myproject' should appear when channel is thinking (no tool entries). Got: {row:?}",
    );
}

#[test]
fn test_draw_lead_indicator_name_pulsed_when_channel_thinking() {
    // The agent name should appear (pulsing bold/normal) when channel_lead_thinking is active.
    // Frame 0 → pulse_bold=true → BOLD modifier applied to the name.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let backend = TestBackend::new(80, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.selected_channel = "myproject".to_string();
    app.set_channel_lead_thinking("myproject");

    terminal
        .draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 1,
            };
            draw_lead_indicator(f, &mut app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let row = buffer_row(buffer, 0, 80);

    assert!(
        row.contains("myproject"),
        "Agent name 'myproject' should appear (pulsing) when channel_lead_thinking is active. Got: {row:?}",
    );
}

#[test]
fn test_draw_lead_indicator_shows_dim_placeholder_when_idle() {
    // When there are no tool entries and no thinking state, the indicator area
    // should render a dim placeholder with the agent name (not an empty blank row).
    // This keeps the status area visible so messages don't jump when activity arrives.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let backend = TestBackend::new(80, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    // No tool entries, no thinking state — fully idle

    terminal
        .draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 1,
            };
            draw_lead_indicator(f, &mut app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let row = buffer_row(buffer, 0, 80);

    assert!(
        row.contains("test"),
        "Idle indicator should show agent name 'test' as dim placeholder. Got: {row:?}",
    );
}

// ── Overlay height tests ─────────────────────────────────────────────

#[test]
fn test_channel_switcher_overlay_height_with_one_item() {
    // Layout: Borders::ALL (top + bottom = 2) + input line (1) + separator (1) + 1 item = 5
    // The old formula `3 + N` gave 4 for 1 item, clipping the bottom border.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let backend = TestBackend::new(80, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.channel_switcher.show = true;
    app.channel_switcher.filtered_channels = vec![super::super::super::app::ChannelSwitcherItem {
        name: "general".to_string(),
        unread_count: 0,
    }];

    let area = Rect::new(0, 0, 80, 40);
    terminal
        .draw(|f| {
            draw_channel_switcher_overlay(f, &app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();

    // popup_width=50, popup_x=(80-50)/2=15. Expected height=4+1=5, popup_y=(40-5)/2=17.
    // Use the left edge (popup_x=15) to find corner characters.
    let popup_x = 15u16;
    let top_y = (0u16..40)
        .find(|&y| {
            let sym = buffer.cell((popup_x, y)).map(|c| c.symbol().to_string());
            sym.as_deref() == Some("┌")
        })
        .expect("Should find top-left corner '┌' of channel switcher overlay");

    // With correct height=5, bottom border is at top_y + 4
    let bottom_y = top_y + 4;
    let bottom_sym = buffer
        .cell((popup_x, bottom_y))
        .map(|c| c.symbol().to_string());
    assert_eq!(
        bottom_sym.as_deref(),
        Some("└"),
        "Bottom-left corner '└' should be at row {} (height=5 from top_y={}). \
         Got {:?} — with the old `3+N` formula, height would be 4 and this row would be empty.",
        bottom_y,
        top_y,
        bottom_sym
    );
}

#[test]
fn test_search_overlay_height_with_one_result() {
    // Same layout as channel switcher: 2 borders + 1 input + 1 separator + 1 item = 5
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let backend = TestBackend::new(80, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.search.show = true;
    app.search.results = vec![midtown::search::SearchResult {
        id: "1".to_string(),
        from: "alice".to_string(),
        content: "hello".to_string(),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        channel: "general".to_string(),
        message_type: "text".to_string(),
        snippet: "hello".to_string(),
        thread_parent_id: None,
    }];

    // popup_width=60, popup_x=(80-60)/2=10. Expected height=4+1=5, popup_y=(40-5)/2=17.
    let area = Rect::new(0, 0, 80, 40);
    terminal
        .draw(|f| {
            draw_search_overlay(f, &app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();

    let popup_x = 10u16;
    let top_y = (0u16..40)
        .find(|&y| {
            let sym = buffer.cell((popup_x, y)).map(|c| c.symbol().to_string());
            sym.as_deref() == Some("┌")
        })
        .expect("Should find top-left corner '┌' of search overlay");

    let bottom_y = top_y + 4;
    let bottom_sym = buffer
        .cell((popup_x, bottom_y))
        .map(|c| c.symbol().to_string());
    assert_eq!(
        bottom_sym.as_deref(),
        Some("└"),
        "Bottom-left corner '└' should be at row {} (height=5 from top_y={})",
        bottom_y,
        top_y,
    );
}
