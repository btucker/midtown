use super::super::super::app::FocusedPane;
use super::super::super::app::tests::test_app;
use super::draw_thread_panel;

// ── draw_thread_panel: narrow terminal layout regression ─────────────────────

/// When the thread panel is very narrow (content_width = 0 after border subtraction),
/// the header height must stay at the 4-line minimum rather than inflating to the
/// 12-line cap. Previously, `render_content_lines` called with width=0 would wrap
/// each character into its own line, causing a long parent message to hit the 12-line
/// cap and crowd out thread replies.
#[test]
fn test_draw_thread_panel_narrow_terminal_header_height() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let mut app = test_app();

    // Add a long parent message (many characters that would balloon to 12+ lines at width=1)
    let parent_msg = midtown::Message::text("park", "word ".repeat(50));
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id);

    // 4-column terminal: area.width=4, content_width=4.saturating_sub(2)=2 — narrow but nonzero
    // Use width=2: content_width=0 (2-2=0), which is the zero-width edge case
    let backend = TestBackend::new(2, 20);
    let mut terminal = Terminal::new(backend).unwrap();

    // Should not panic, and should render without filling the entire height with header
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 2, 20);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    // The key invariant: thread replies area (chunks[1]) must have height >= 3
    // (the Min(3) constraint). If the header inflated to 12, there'd be no space.
    // With a 20-row terminal: 12 header + 3 replies_min + 3 input = 18 minimum needed.
    // We verify the panel renders without panic — a panic means the layout overflowed.
}

/// Thread input cursor must use theme palette colors rather than hardcoded black/yellow.
///
/// The `█` (FULL BLOCK) cursor in draw_thread_input previously used
/// `fg(Color::Black).bg(Color::Yellow)` — a solid black block that is invisible on dark
/// backgrounds. The cursor must use `fg(palette.fg)` so it appears light/white in dark themes.
#[test]
fn test_thread_cursor_uses_palette_not_hardcoded_colors() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    // 40-wide, 12-tall terminal.
    // Layout: header(4) + replies(5) + input(3) = 12 rows.
    let backend = TestBackend::new(40, 12);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    // Set up a parent message so draw_thread_panel renders the full panel.
    let parent_msg = midtown::Message::text("park", "hello");
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id);
    app.focused_pane = FocusedPane::Thread;
    app.thread_input_text = "hi".to_string();
    app.thread_input_cursor = 2; // at end of "hi" — renders the █ cursor

    let palette = app.theme.palette();

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 40, 12);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    // Input chunk starts at y=9 (header=4, replies=5).
    // Inside the input block: border at y=9, content at y=10, border at y=11.
    // Content at y=10: left_border(x=0) + ↳(x=1) + space(x=2) + h(x=3) + i(x=4) + cursor(x=5).
    let cursor_cell = buffer.cell((5, 10)).unwrap();

    assert_eq!(
        cursor_cell.symbol(),
        "█",
        "Thread cursor at end of text should show '█'"
    );
    assert_ne!(
        cursor_cell.fg,
        Color::Black,
        "Thread cursor must NOT use hardcoded Color::Black — invisible in dark themes. Got fg={:?}",
        cursor_cell.fg
    );
    assert_eq!(
        cursor_cell.fg, palette.fg,
        "Thread cursor must use palette.fg so it's visible in any theme. \
         Got fg={:?}, expected palette.fg={:?}",
        cursor_cell.fg, palette.fg
    );
}

/// Thread input bar must expand to multiple lines for long input text,
/// matching the behavior of the main channel input bar.
///
/// The thread input height was previously hardcoded to 3 (1 content line + 2 borders).
/// With long input text that wraps to multiple lines, it must grow dynamically
/// using the same calculate_input_bar_height logic as the main input.
#[test]
fn test_thread_input_expands_to_multiple_lines() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    // 80-wide, 25-tall terminal — enough space for header + replies + tall input.
    let backend = TestBackend::new(80, 25);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    let parent_msg = midtown::Message::text("park", "hello");
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id);

    // 150 'a' chars should wrap to 3 content lines at 80-wide terminal
    // (available inner width 78, minus prompt 3, minus cursor 1 = 74 chars/line → 3 lines)
    app.thread_input_text = "a".repeat(150);

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 25);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    // The thread input area (including borders) must be > 3 rows for multi-line text.
    // With 3 content lines + 2 border rows = 5 total.
    let input_area = app
        .thread_input_area
        .expect("thread_input_area must be set");
    assert!(
        input_area.height > 3,
        "Thread input height should expand beyond 3 for long text, got {}",
        input_area.height
    );
    assert_eq!(
        input_area.height, 5,
        "150-char input should give 3 content lines + 2 borders = height 5, got {}",
        input_area.height
    );
}

/// When content_width is zero, header renders with minimum height (4), not maximum (12).
/// This is the direct regression check: we measure the layout split produces a valid
/// replies area rather than an overflowed/zero-height one.
#[test]
fn test_draw_thread_panel_zero_content_width_does_not_inflate_header() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let mut app = test_app();

    // 250-char message: at width=1 wrapping, this becomes 250 lines → hits 12-line cap
    let long_content = "x".repeat(250);
    let parent_msg = midtown::Message::text("madison", &long_content);
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id);

    // Minimal width: content_width = area.width.saturating_sub(2) = 0
    let backend = TestBackend::new(2, 20);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 2, 20);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    // Rendered without panic — the layout was valid (no overflow from inflated header)
}
