//! Tests for `draw_lead_indicator` rendering and input bar cursor behavior.

use super::super::super::app::ToolActivityEntry;
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

fn make_tool_entry(header: &str, completed: bool) -> ToolActivityEntry {
    ToolActivityEntry {
        header: header.to_string(),
        completed_at: if completed {
            Some(std::time::Instant::now())
        } else {
            None
        },
    }
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
fn test_draw_lead_indicator_agent_name_on_last_line() {
    // Agent name should appear on the LAST (bottom) line, not the first.
    // New entries should come in at the bottom (CLI-style ordering).
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let backend = TestBackend::new(80, 2);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    // Two entries: older completed, newer in-progress
    app.tool_activity = std::collections::HashMap::from([(
        "midtown".to_string(),
        vec![
            make_tool_entry("\u{2713} Read foo.rs", true), // older, completed
            make_tool_entry("\u{203a} Write bar.rs", false), // newer, in-progress
        ],
    )]);
    app.lead_working = true;

    terminal
        .draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 2,
            };
            draw_lead_indicator(f, &mut app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let row0 = buffer_row(buffer, 0, 20);
    let row1 = buffer_row(buffer, 1, 20);

    assert!(
        row1.contains("midtown"),
        "Agent name 'midtown' should be on the LAST line (row 1), got row1={row1:?} row0={row0:?}",
    );
    assert!(
        !row0.contains("midtown"),
        "Agent name 'midtown' should NOT be on the first line (row 0), got row0={row0:?}",
    );
}

#[test]
fn test_draw_lead_indicator_older_entries_on_top() {
    // Older entries should be displayed above newer entries (chronological order).
    // The completed (✓) older entry should be on row 0; in-progress (›) newer on row 1.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let backend = TestBackend::new(80, 2);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.tool_activity = std::collections::HashMap::from([(
        "midtown".to_string(),
        vec![
            make_tool_entry("\u{2713} Read foo.rs", true), // older
            make_tool_entry("\u{203a} Write bar.rs", false), // newer
        ],
    )]);
    app.lead_working = true;

    terminal
        .draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 2,
            };
            draw_lead_indicator(f, &mut app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let row0 = buffer_row(buffer, 0, 80);
    let row1 = buffer_row(buffer, 1, 80);

    assert!(
        row0.contains("foo"),
        "Older entry (foo.rs) should be on row 0 (top), got row0={row0:?}",
    );
    assert!(
        row1.contains("bar"),
        "Newer entry (bar.rs) should be on row 1 (bottom), got row1={row1:?}",
    );
}

#[test]
fn test_draw_lead_indicator_no_ellipsis_truncation() {
    // Descriptions should NOT be truncated with "..." when rendered.
    // The text should just be clipped at the terminal edge, not have "..." added.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let backend = TestBackend::new(30, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.tool_activity = std::collections::HashMap::from([(
        "midtown".to_string(),
        vec![make_tool_entry("\u{203a} ABCDEFGHIJKLMNOPQRSTUVWXY", false)],
    )]);
    app.lead_working = true;

    terminal
        .draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 30,
                height: 1,
            };
            draw_lead_indicator(f, &mut app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let row = buffer_row(buffer, 0, 30);

    assert!(
        !row.contains("..."),
        "Lead indicator should NOT use '...' truncation — just clip at terminal edge. Got: {row:?}",
    );
}

#[test]
fn test_draw_lead_indicator_name_shown_without_lead_working() {
    // When lead_working is false but tool entries are in-progress,
    // the agent name must still appear (pulsing bold/normal). This ensures visual
    // activity is shown even when lead_working is stale, consistent with
    // any_spinner_visible() which fires the animation timer for in-progress entries.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let backend = TestBackend::new(80, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.lead_working = false; // explicitly NOT working
    app.tool_activity = std::collections::HashMap::from([(
        "midtown".to_string(),
        vec![make_tool_entry("\u{203a} Read foo.rs", false)], // in-progress
    )]);

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
        row.contains("midtown"),
        "Agent name 'midtown' should appear when tool entries are in-progress, even if lead_working=false. Got: {row:?}",
    );
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
        row.contains("midtown"),
        "Idle indicator should show agent name 'midtown' as dim placeholder. Got: {row:?}",
    );
}

#[test]
fn test_draw_lead_indicator_name_bold_when_only_completed_entries() {
    // When entries exist but none are in-progress (all ✓/✗), the name should still be
    // rendered BOLD via static style (not pulse_name_style). The static style guarantees
    // BOLD regardless of spinner_frame, preventing false animation when the frame advances
    // due to active coworkers in other channels.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;

    let backend = TestBackend::new(80, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.tool_activity = std::collections::HashMap::from([(
        "midtown".to_string(),
        vec![make_tool_entry("\u{2713} Read foo.rs", true)], // completed — no in-progress
    )]);

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

    // At frame 0 the static bold style and pulse_name_style both produce BOLD.
    // The key guard is in the code: has_in_progress=false → static style path taken.
    let name_start_col = (0u16..80)
        .find(|&x| buffer.cell((x, 0)).map(|c| c.symbol()) == Some("m"))
        .expect("'midtown' should appear in the buffer");

    let cell = buffer.cell((name_start_col, 0)).unwrap();
    assert!(
        cell.modifier.contains(Modifier::BOLD),
        "Agent name should be BOLD when only completed entries exist. Got: {:?}",
        cell.modifier
    );
}
